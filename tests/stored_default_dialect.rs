//! Persisted defaults are DuckDB expressions, including named STRUCT casts.
use msduck::{engine::Session, server::Server};

fn batch(session: &mut Session, sql: &str) {
    let (response, ok) = session.batch_response(sql, &Default::default(), false, None);
    assert!(ok, "{sql}: {response:?}");
}

fn prepared_session() -> (Server, Session) {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    batch(
        &mut session,
        "CREATE TABLE units(id INT, value NVARCHAR(2))",
    );
    // Install a backend expression directly to exercise its serialized dialect.
    // This is a native adapter fixture, not additional public T-SQL syntax.
    session
        .db
        .execute_batch(
            "ALTER TABLE units ALTER COLUMN value SET DEFAULT
         __msduck_store_carrier_nvarchar(
           CASE WHEN TRUE THEN CAST(__msduck_unicode_from_le(from_hex('00d8'))
             AS STRUCT(__msduck_utf16le BLOB))
           ELSE __msduck_unicode_from_le(from_hex('7800')) END, 2)",
        )
        .unwrap();
    (server, session)
}

#[test]
fn persisted_struct_defaults_keep_raw_units_for_omitted_and_explicit_values() {
    let (_server, mut session) = prepared_session();
    for sql in [
        "INSERT units(id) VALUES(1)",
        "INSERT units(id,value) VALUES(2,DEFAULT)",
    ] {
        batch(&mut session, sql);
    }
    let values: Vec<(i32, Vec<u8>)> = session
        .db
        .prepare("SELECT id,value.__msduck_utf16le FROM units ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<duckdb::Result<_>>()
        .unwrap();
    assert_eq!(values, vec![(1, vec![0, 0xd8]), (2, vec![0, 0xd8])]);
}

#[test]
fn persisted_struct_default_survives_update_assignment() {
    let (_server, mut session) = prepared_session();
    batch(&mut session, "INSERT units(id,value) VALUES(1,N'x')");
    batch(&mut session, "UPDATE units SET value=DEFAULT WHERE id=1");
    let units: Vec<u8> = session
        .db
        .query_row("SELECT value.__msduck_utf16le FROM units", [], |r| r.get(0))
        .unwrap();
    assert_eq!(units, vec![0, 0xd8]);
}

#[test]
fn persisted_struct_default_survives_merge_insert_and_update() {
    let (_server, mut session) = prepared_session();
    batch(
        &mut session,
        "MERGE units AS t USING (SELECT 1 AS id) AS s ON t.id=s.id WHEN NOT MATCHED THEN INSERT(id) VALUES(s.id);",
    );
    let inserted: Vec<u8> = session
        .db
        .query_row("SELECT value.__msduck_utf16le FROM units", [], |r| r.get(0))
        .unwrap();
    assert_eq!(inserted, vec![0, 0xd8]);
    batch(&mut session, "UPDATE units SET value=N'x' WHERE id=1");
    batch(
        &mut session,
        "MERGE units AS t USING (SELECT 1 AS id) AS s ON t.id=s.id WHEN MATCHED THEN UPDATE SET value=DEFAULT;",
    );
    let units: Vec<u8> = session
        .db
        .query_row("SELECT value.__msduck_utf16le FROM units", [], |r| r.get(0))
        .unwrap();
    assert_eq!(units, vec![0, 0xd8]);
}
