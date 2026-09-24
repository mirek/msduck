use duckdb::types::Value as Bound;
use msduck::server::Server;
use serde_json::Value;

fn bound(value: &Value) -> Bound {
    if value.is_null() {
        Bound::Null
    } else {
        Bound::Text(value.as_str().unwrap().into())
    }
}

#[test]
fn native_outcomes_match_all_240_reference_cases_without_aborting_transaction() {
    let fixture: Value =
        serde_json::from_str(include_str!("../reference/checked-integer.json")).unwrap();
    let server = Server::open(":memory:").unwrap();
    let db = server.connection().unwrap();
    db.execute_batch(
        "CREATE TABLE prior_write(i INTEGER); BEGIN; INSERT INTO prior_write VALUES(7)",
    )
    .unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let name = match case["operation"].as_str().unwrap() {
            "+" => "add",
            "-" => "subtract",
            "*" => "multiply",
            "/" => "divide",
            "%" => "modulo",
            _ => panic!("unknown captured operation"),
        };
        let width = case["width"].as_str().unwrap();
        let left = case["leftWidth"].as_str().unwrap_or(width);
        let right = case["rightWidth"].as_str().unwrap_or(width);
        assert!(matches!(left, "INT" | "BIGINT") && matches!(right, "INT" | "BIGINT"));
        let sql = format!(
            "SELECT r.value,r.error_number,r.error_state,r.error_severity,r.error_message,typeof(r.value),r IS NULL FROM (SELECT __msduck_checked_{name}(CAST(? AS {left}),CAST(? AS {right})) AS r)"
        );
        let (value, number, state, severity, message, kind, null) = db
            .query_row(&sql, [bound(&case["left"]), bound(&case["right"])], |r| {
                Ok((
                    r.get::<_, Option<i64>>(0)?,
                    r.get::<_, Option<i32>>(1)?,
                    r.get::<_, Option<u8>>(2)?,
                    r.get::<_, Option<u8>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, bool>(6)?,
                ))
            })
            .unwrap();
        assert!(!null, "{case}");
        assert_eq!(
            kind,
            if width == "INT" { "INTEGER" } else { "BIGINT" },
            "{case}"
        );
        if let Some(error) = case["result"]["errors"].as_array().unwrap().first() {
            assert_eq!(value, None, "{case}");
            assert_eq!(
                number,
                Some(error["number"].as_i64().unwrap() as i32),
                "{case}"
            );
            assert_eq!(
                state,
                Some(error["state"].as_u64().unwrap() as u8),
                "{case}"
            );
            assert_eq!(
                severity,
                Some(error["class"].as_u64().unwrap() as u8),
                "{case}"
            );
            assert_eq!(message.as_deref(), error["message"].as_str(), "{case}");
        } else {
            let expected = &case["result"]["sets"][0]["rows"][0][0];
            let expected = if expected.is_null() {
                None
            } else if width == "INT" {
                Some(expected.as_i64().unwrap())
            } else {
                Some(expected.as_str().unwrap().parse::<i64>().unwrap())
            };
            assert_eq!(value, expected, "{case}");
            assert_eq!(
                (number, state, severity, message),
                (None, None, None, None),
                "{case}"
            );
        }
    }
    db.execute_batch("INSERT INTO prior_write VALUES(8); COMMIT")
        .unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM prior_write", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
}

#[test]
fn filtered_multi_chunk_outcomes_preserve_nulls_errors_and_single_evaluation() {
    let server = Server::open(":memory:").unwrap();
    let db = server.connection().unwrap();
    db.execute_batch("CREATE SEQUENCE arithmetic_calls START 1; BEGIN;
        CREATE TEMP TABLE arithmetic_stage AS
        SELECT __msduck_checked_divide(nextval('arithmetic_calls'),CAST(CASE WHEN i%6=0 THEN 0 WHEN i%6=2 THEN NULL ELSE 2 END AS BIGINT)) AS r
        FROM range(12000) AS input(i) WHERE i%2=0").unwrap();
    assert_eq!(
        db.query_row("SELECT currval('arithmetic_calls')", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        6000
    );
    let counts: (i64,i64,i64,i64,i64) = db.query_row("SELECT count(*),count(*) FILTER(WHERE r.value IS NOT NULL),count(*) FILTER(WHERE r.error_number=8134 AND r.error_state=1 AND r.error_severity=16 AND r.error_message='Divide by zero error encountered.'),count(*) FILTER(WHERE r.value IS NULL AND r.error_number IS NULL),count(*) FILTER(WHERE r IS NULL) FROM arithmetic_stage", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).unwrap();
    assert_eq!(counts, (6000, 2000, 2000, 2000, 0));
    db.execute_batch("COMMIT").unwrap();
}

#[test]
fn vector_overflow_and_empty_results_retain_declared_widths() {
    let server = Server::open(":memory:").unwrap();
    let db = server.connection().unwrap();
    db.execute_batch("BEGIN; CREATE TEMP TABLE int_overflow AS SELECT __msduck_checked_multiply(CAST(i AS INT),2147483647::INT) AS r FROM range(6000) AS input(i)").unwrap();
    let counts: (i64,i64) = db.query_row("SELECT count(*) FILTER(WHERE r.value IS NOT NULL),count(*) FILTER(WHERE r.error_number=8115 AND r.error_state=2) FROM int_overflow", [], |r| Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(counts, (2, 5998));
    let mut query = db
        .prepare("SELECT __msduck_checked_add(NULL::INT,NULL::BIGINT).value AS value WHERE false")
        .unwrap();
    let mut batches = query.query_arrow([]).unwrap();
    assert_eq!(
        batches.get_schema().field(0).data_type(),
        &duckdb::arrow::datatypes::DataType::Int64
    );
    assert!(batches.next().is_none());
    drop(batches);
    drop(query);
    db.execute_batch("COMMIT").unwrap();
}
