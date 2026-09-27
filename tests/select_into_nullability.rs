use msduck::{engine::Session, server::Server};

fn execute(session: &mut Session, sql: &str) {
    let (tokens, ok) = session.batch_response(sql, &Default::default(), false, None);
    assert!(ok, "{sql}: {tokens:?}");
}

fn nullable(session: &Session, table: &str, column: &str) -> String {
    session
        .db
        .query_row(
            "SELECT is_nullable FROM information_schema.columns WHERE table_schema='dbo' AND table_name=? AND column_name=?",
            [table, column],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn literal_select_into_column_is_non_nullable_like_reference() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    execute(&mut session, "SELECT 1 AS id INTO dbo.si_literal");
    assert_eq!(nullable(&session, "si_literal", "id"), "NO");
    let (tokens, ok) = session.batch_response(
        "SELECT id FROM dbo.si_literal",
        &Default::default(),
        false,
        None,
    );
    assert!(ok, "{tokens:?}");
    assert!(
        tokens
            .windows(10)
            .any(|part| part == [0x81, 1, 0, 0, 0, 0, 0, 8, 0, 0x38])
    );
}

#[test]
fn source_nullability_survives_empty_and_outer_join_select_into() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    execute(
        &mut session,
        "CREATE TABLE dbo.si_source(id INT NOT NULL, n INT)",
    );
    execute(
        &mut session,
        "INSERT INTO dbo.si_source VALUES(1,NULL),(2,7)",
    );
    execute(
        &mut session,
        "CREATE TABLE dbo.si_right(id INT NOT NULL, v INT NOT NULL)",
    );
    execute(&mut session, "INSERT INTO dbo.si_right VALUES(2,9)");

    execute(
        &mut session,
        "SELECT id,n INTO dbo.si_copy FROM dbo.si_source",
    );
    assert_eq!(nullable(&session, "si_copy", "id"), "NO");
    assert_eq!(nullable(&session, "si_copy", "n"), "YES");

    execute(
        &mut session,
        "SELECT l.id AS id,r.v AS v INTO dbo.si_outer FROM dbo.si_source l LEFT JOIN dbo.si_right r ON l.id=r.id",
    );
    assert_eq!(nullable(&session, "si_outer", "id"), "NO");
    assert_eq!(nullable(&session, "si_outer", "v"), "YES");
    let nulls: i64 = session
        .db
        .query_row(
            "SELECT count(*) FROM dbo.si_outer WHERE v IS NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(nulls, 1);

    execute(&mut session, "SELECT 1 AS id INTO dbo.si_empty WHERE 1=0");
    assert_eq!(nullable(&session, "si_empty", "id"), "NO");
    let rows: i64 = session
        .db
        .query_row("SELECT count(*) FROM dbo.si_empty", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 0);
    execute(
        &mut session,
        "SELECT CAST(NULL AS INT) AS n INTO dbo.si_null",
    );
    assert_eq!(nullable(&session, "si_null", "n"), "YES");
}

#[test]
fn preparation_and_two_phase_failure_preserve_destination_lifecycle() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    session
        .validate_prepared_sql("SELECT 1 AS id INTO dbo.si_prepared", &[])
        .unwrap();
    let uncreated: i64 = session.db.query_row(
        "SELECT count(*) FROM information_schema.tables WHERE table_schema='dbo' AND table_name='si_prepared'", [], |row| row.get(0)).unwrap();
    assert_eq!(uncreated, 0);
    execute(&mut session, "SELECT 1 AS id INTO dbo.si_prepared");
    assert_eq!(nullable(&session, "si_prepared", "id"), "NO");

    let (_, success) = session.batch_response(
        "SELECT i AS id,CAST(s AS INT) AS n INTO dbo.si_failed FROM (VALUES(1,'2'),(2,'bad')) v(i,s)",
        &Default::default(), false, None);
    assert!(!success);
    let rows: i64 = session
        .db
        .query_row("SELECT count(*) FROM dbo.si_failed", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 0);

    execute(&mut session, "BEGIN TRANSACTION");
    execute(&mut session, "SELECT 1 AS id INTO dbo.si_rollback");
    execute(&mut session, "ROLLBACK TRANSACTION");
    let rolled_back: i64 = session.db.query_row(
        "SELECT count(*) FROM information_schema.tables WHERE table_schema='dbo' AND table_name='si_rollback'", [], |row| row.get(0)).unwrap();
    assert_eq!(rolled_back, 0);
}
