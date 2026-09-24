//! Backend capability probes, not claims of SQL Server compatibility.
use duckdb::Connection;

#[test]
fn backend_runtime_errors_invalidate_the_transaction_and_prior_writes() {
    for sql in [
        "SELECT CAST('invalid' AS INTEGER)",
        "INSERT INTO recovery_values VALUES (1)",
        "SELECT error('arithmetic diagnostic')",
    ] {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE recovery_values(i INTEGER PRIMARY KEY); BEGIN; INSERT INTO recovery_values VALUES(1)").unwrap();
        assert!(db.execute_batch(sql).is_err(), "{sql}");
        let error = db
            .query_row("SELECT count(*) FROM recovery_values", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap_err();
        assert!(
            error.to_string().contains("transaction is aborted"),
            "{sql}: {error}"
        );
        db.execute_batch("ROLLBACK").unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM recovery_values", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}

#[test]
fn backend_savepoint_rejection_does_not_supply_statement_recovery() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(
        "CREATE TABLE recovery_values(i INTEGER); BEGIN; INSERT INTO recovery_values VALUES(1)",
    )
    .unwrap();
    let error = db.execute_batch("SAVEPOINT before_statement").unwrap_err();
    assert!(error.to_string().contains("Parser Error"), "{error}");
    // A parser failure is non-invalidating, unlike the runtime failures above.
    db.execute_batch("COMMIT").unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM recovery_values", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn backend_try_preserves_transaction_but_erases_diagnostics() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(
        "CREATE TABLE recovery_values(i INTEGER); BEGIN; INSERT INTO recovery_values VALUES(1)",
    )
    .unwrap();
    let row: (Option<i32>, Option<i32>) = db
        .query_row(
            "SELECT TRY(CAST('invalid' AS INTEGER)),TRY(CAST(NULL AS INTEGER))",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(row, (None, None));
    db.execute_batch("COMMIT").unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM recovery_values", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn backend_try_cannot_wrap_volatile_values_or_scalar_subqueries() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE SEQUENCE recovery_calls START 1; BEGIN")
        .unwrap();
    for (sql, reason) in [
        ("SELECT TRY(nextval('recovery_calls'))", "volatile"),
        ("SELECT TRY((SELECT 1))", "scalar subquery"),
    ] {
        let error = db.execute_batch(sql).unwrap_err();
        assert!(error.to_string().contains(reason), "{error}");
    }
    assert_eq!(
        db.query_row("SELECT nextval('recovery_calls')", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    db.execute_batch("COMMIT").unwrap();
}
