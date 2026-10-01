use crate::engine::Session;

fn scratch_tables(session: &Session) -> i64 {
    session
        .db
        .query_row(
            "SELECT count(*) FROM duckdb_tables() WHERE database_name='temp' AND table_name LIKE '__msduck_merge_%'",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

/// Every outcome drops the statement's staging tables and leaves no
/// statement-owned backend transaction behind.
#[test]
fn scratch_tables_and_own_transactions_end_with_the_statement() {
    let server = crate::server::Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    let run = |session: &mut Session, sql: &str| {
        session
            .batch_response(sql, &Default::default(), false, None)
            .1
    };
    assert!(run(
        &mut session,
        "CREATE TABLE dbo.t(id INT PRIMARY KEY, n INT NOT NULL CHECK (n > 0))"
    ));
    for (sql, succeeds) in [
        (
            "MERGE dbo.t AS t USING (VALUES (1, 1)) AS s(id, n) ON t.id = s.id WHEN NOT MATCHED THEN INSERT (id, n) VALUES (s.id, s.n) OUTPUT $action, inserted.id;",
            true,
        ),
        (
            "MERGE dbo.t AS t USING (VALUES (1, 2), (1, 3)) AS s(id, n) ON t.id = s.id WHEN MATCHED THEN UPDATE SET n = s.n;",
            false,
        ),
        (
            "MERGE dbo.t AS t USING (VALUES (1, -1)) AS s(id, n) ON t.id = s.id WHEN MATCHED THEN UPDATE SET n = s.n;",
            false,
        ),
        (
            "MERGE dbo.t AS t USING (VALUES (1, 'x')) AS s(id, n) ON t.id = s.id WHEN MATCHED THEN UPDATE SET n = s.n;",
            false,
        ),
    ] {
        assert_eq!(run(&mut session, sql), succeeds, "{sql}");
        assert_eq!(scratch_tables(&session), 0, "{sql}");
        // A new backend transaction can start: none is left open.
        session.db.execute_batch("BEGIN; ROLLBACK").unwrap();
    }
    // Inside the caller's transaction the tables go with the statement too.
    assert!(run(&mut session, "BEGIN TRANSACTION"));
    assert!(run(
        &mut session,
        "MERGE dbo.t AS t USING (VALUES (2, 2)) AS s(id, n) ON t.id = s.id WHEN NOT MATCHED THEN INSERT (id, n) VALUES (s.id, s.n);"
    ));
    assert_eq!(scratch_tables(&session), 0);
    assert!(run(&mut session, "ROLLBACK TRANSACTION"));
    assert_eq!(
        session
            .db
            .query_row("SELECT count(*) FROM dbo.t", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
}
