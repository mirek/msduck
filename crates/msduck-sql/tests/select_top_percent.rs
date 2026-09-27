//! SELECT TOP PERCENT and WITH TIES lowering over reference/select-top-percent.json.
use msduck_sql::top;
use sqlparser::{
    ast::{Query, SetExpr, Statement},
    parser::Parser,
};

fn query(sql: &str) -> Box<Query> {
    let mut statement = msduck_sql::batch::parse(sql).unwrap().remove(0);
    let Statement::Query(query) = &mut statement else {
        panic!("query expected: {sql}")
    };
    query.clone()
}

fn lower(sql: &str) -> Result<String, String> {
    let mut query = query(sql);
    top::ranked(&mut query)?;
    Ok(query.to_string())
}

fn fixture() -> serde_json::Value {
    serde_json::from_str(include_str!("../../../reference/select-top-percent.json")).unwrap()
}

fn captured_error(case: &serde_json::Value) -> Option<(i64, i64, i64, String)> {
    let error = case["result"]["errors"].as_array().unwrap().first()?;
    Some((
        error["number"].as_i64().unwrap(),
        error["state"].as_i64().unwrap(),
        error["class"].as_i64().unwrap(),
        error["message"].as_str().unwrap().to_owned(),
    ))
}

#[test]
fn captured_compile_diagnostics_match_every_retained_run() {
    // Text conversion (8114) and runtime NULL parameters are execution errors.
    let compile = [
        "above full percent",
        "negative percent",
        "null percent",
        "ties without order",
        "percent ties without order",
        "negative count ties",
        "null count ties",
        "distinct invalid order",
    ];
    let fixture = fixture();
    let runs = fixture["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 2);
    for run in runs {
        let mut seen = 0;
        for case in run.as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let sql = case["sql"].as_str().unwrap();
            if !sql.contains("TOP") || sql.contains('@') {
                continue;
            }
            let lowered = lower(sql);
            if compile.contains(&name) {
                seen += 1;
                let message = lowered.expect_err(name);
                let error = top::diagnostic(&message).expect(name);
                assert_eq!(
                    Some((
                        i64::from(error.number),
                        i64::from(error.state),
                        i64::from(error.severity),
                        error.message
                    )),
                    captured_error(case),
                    "{name}"
                );
            } else if name != "text percent" {
                assert!(lowered.is_ok(), "{name}: {lowered:?}");
                assert_eq!(captured_error(case), None, "{name}");
            }
        }
        assert_eq!(seen, compile.len());
    }
}

#[test]
fn runtime_count_diagnostics_share_captured_class() {
    for (message, number) in [
        (
            "Invalid Input Error: A TOP or FETCH clause contains an invalid value.",
            1014,
        ),
        (
            "Invalid Input Error: Percent values must be between 0 and 100.",
            1031,
        ),
    ] {
        let error = top::diagnostic(message).unwrap();
        assert_eq!((error.number, error.state, error.severity), (number, 1, 15));
        assert!(!error.message.starts_with("Invalid Input Error"));
    }
    assert!(top::diagnostic("Invalid Input Error: something else").is_none());
}

#[test]
fn percent_is_converted_once_and_bounds_rows_after_grouping() {
    let sql = lower(
        "SELECT TOP (@p) PERCENT score,COUNT(*) AS frequency FROM t GROUP BY score ORDER BY score DESC",
    )
    .unwrap();
    assert_eq!(sql.matches("@p").count(), 1, "{sql}");
    assert_eq!(sql.matches("CAST(@p AS DOUBLE)").count(), 1, "{sql}");
    assert!(
        sql.contains("GROUP BY score QUALIFY row_number() OVER (ORDER BY score DESC) <= ceil("),
        "{sql}"
    );
    assert!(sql.contains("count(*) OVER () / 100"), "{sql}");
    assert!(!sql.contains("SELECT TOP"), "{sql}");
    assert!(sql.ends_with("ORDER BY score DESC"), "{sql}");
}

#[test]
fn ties_rank_by_order_keys_and_resolve_aliases_and_ordinals() {
    let sql = lower("SELECT TOP (2) WITH TIES id,score AS s FROM t ORDER BY s DESC,2,id").unwrap();
    assert!(
        sql.contains("QUALIFY rank() OVER (ORDER BY score DESC, score, id) <= (SELECT __msduck_top_count(coalesce(2, -1)))"),
        "{sql}"
    );
    // The query ORDER BY keeps SQL Server name resolution for the final sort.
    assert!(sql.ends_with("ORDER BY s DESC, 2, id"), "{sql}");
    let sql = lower(
        "SELECT TOP (1) PERCENT WITH TIES COUNT(*) AS n FROM t GROUP BY g ORDER BY COUNT(*) DESC",
    )
    .unwrap();
    assert!(
        sql.contains("rank() OVER (ORDER BY COUNT(*) DESC)"),
        "{sql}"
    );
}

#[test]
fn distinct_ranks_derived_output_columns() {
    let sql =
        lower("SELECT DISTINCT TOP (25) PERCENT WITH TIES score AS s FROM t ORDER BY score DESC")
            .unwrap();
    assert!(
        sql.starts_with("SELECT * FROM (SELECT DISTINCT score AS s FROM t) AS __msduck_top_distinct QUALIFY rank() OVER (ORDER BY s DESC)"),
        "{sql}"
    );
    assert!(sql.ends_with("ORDER BY s DESC"), "{sql}");
    let sql =
        lower("SELECT DISTINCT TOP (50) PERCENT t.score INTO copy FROM t ORDER BY 1").unwrap();
    assert!(
        sql.starts_with("SELECT * INTO copy FROM (SELECT DISTINCT t.score FROM t)"),
        "{sql}"
    );
    assert!(sql.ends_with("ORDER BY score"), "{sql}");
}

#[test]
fn unsupported_shapes_remain_explicit() {
    for (sql, expected) in [
        (
            "SELECT DISTINCT TOP (1) PERCENT score + 1 FROM t ORDER BY score + 1",
            "unnamed",
        ),
        (
            "SELECT DISTINCT TOP (1) PERCENT a AS x, b AS x FROM t ORDER BY x",
            "repeated",
        ),
        ("SELECT TOP (1) PERCENT * FROM t ORDER BY 1", "wildcard"),
        ("SELECT DISTINCT TOP (1) PERCENT * FROM t", "wildcard"),
        (
            "SELECT TOP (1) PERCENT id FROM t ORDER BY id OFFSET 0 ROWS",
            "OFFSET",
        ),
    ] {
        let error = lower(sql).unwrap_err();
        assert!(error.contains(expected), "{sql}: {error}");
    }
}

#[test]
fn plain_top_and_other_bodies_are_unchanged() {
    for sql in [
        "SELECT TOP (2) id FROM t ORDER BY id",
        "SELECT id FROM t",
        "SELECT 1 UNION ALL SELECT 2",
    ] {
        let mut expected = query(sql);
        let before = expected.to_string();
        top::ranked(&mut expected).unwrap();
        assert_eq!(expected.to_string(), before);
    }
    // Parsed without the batch front end to show the lowering is AST-only.
    let mut parsed = Parser::parse_sql(
        &msduck_sql::dialect::ServerDialect,
        "SELECT TOP (0) PERCENT id FROM t",
    )
    .unwrap()
    .remove(0);
    let Statement::Query(query) = &mut parsed else {
        unreachable!()
    };
    top::ranked(query).unwrap();
    let SetExpr::Select(select) = query.body.as_ref() else {
        unreachable!()
    };
    assert!(select.top.is_none() && select.qualify.is_some());
    assert!(
        query
            .to_string()
            .contains("row_number() OVER (ORDER BY NULL)")
    );
}
