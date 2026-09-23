use duckdb::{Connection, core::LogicalTypeId};

#[test]
fn describing_owns_names_and_types_without_evaluating_volatile_values() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE SEQUENCE metadata_calls").unwrap();
    let statement = db
        .prepare("SELECT nextval('metadata_calls') AS n,CAST(NULL AS DECIMAL(13,8)) AS d")
        .unwrap();
    let columns = statement.prepared_columns().unwrap();
    drop(statement);
    assert_eq!(
        columns
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["n", "d"]
    );
    assert_eq!(columns[0].1.id(), LogicalTypeId::Bigint);
    assert_eq!(columns[1].1.id(), LogicalTypeId::Decimal);
    assert_eq!(columns[1].1.decimal_width(), 13);
    assert_eq!(columns[1].1.decimal_scale(), 8);
    assert!(
        db.query_row("SELECT currval('metadata_calls')", [], |r| r
            .get::<_, i64>(0))
            .is_err()
    );
    assert_eq!(
        db.query_row("SELECT nextval('metadata_calls')", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn exact_division_metadata_is_available_before_runtime_failure() {
    let server = msduck::server::Server::open(":memory:").unwrap();
    let db = server.connection().unwrap();
    let mut statement = db.prepare("SELECT CAST(__msduck_decimal_divide_8(CAST(1 AS DECIMAL(5,2)),CAST(0 AS DECIMAL(5,2)),13) AS DECIMAL(13,8)) AS d").unwrap();
    let columns = statement.prepared_columns().unwrap();
    assert_eq!(columns.len(), 1);
    assert_eq!(columns[0].0, "d");
    assert_eq!(columns[0].1.decimal_width(), 13);
    assert_eq!(columns[0].1.decimal_scale(), 8);
    assert!(
        statement
            .query_arrow([])
            .err()
            .expect("runtime division error")
            .to_string()
            .contains("Divide by zero")
    );
}

#[test]
fn unresolved_parameters_do_not_fabricate_a_usable_result_shape() {
    let db = Connection::open_in_memory().unwrap();
    let statement = db.prepare("SELECT ? AS a,? AS b").unwrap();
    assert!(statement.prepared_columns().is_err());
    let statement = db.prepare("SELECT CAST(? AS DECIMAL(5,2)) AS a").unwrap();
    let columns = statement.prepared_columns().unwrap();
    assert_eq!(columns.len(), 1);
    assert_eq!(columns[0].1.decimal_width(), 5);
    assert_eq!(columns[0].1.decimal_scale(), 2);
}
