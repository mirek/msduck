//! Preserve source labels while packing character INSERT set arms.
use msduck::{engine::Session, server::Server};

fn batch(session: &mut Session, sql: &str) {
    let (response, ok) = session.batch_response(sql, &Default::default(), false, None);
    assert!(ok, "{sql}: {response:?}");
}

#[test]
fn unicode_set_insert_retains_plain_qualified_and_explicit_names() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    batch(
        &mut session,
        "CREATE TABLE a(v NVARCHAR(10)); CREATE TABLE b(v NVARCHAR(10)); INSERT a VALUES(N'z'); INSERT b VALUES(N'x');",
    );
    for (i, source) in [
        "SELECT v FROM a UNION ALL SELECT v FROM b ORDER BY v OFFSET 0 ROWS",
        "SELECT a.v FROM a UNION ALL SELECT b.v FROM b ORDER BY v OFFSET 0 ROWS",
        "SELECT (a.v) FROM a UNION ALL SELECT (b.v) FROM b ORDER BY v OFFSET 1 ROWS",
        "SELECT a.v AS c FROM a UNION ALL SELECT b.v AS c FROM b ORDER BY c OFFSET 0 ROWS",
    ]
    .iter()
    .enumerate()
    {
        batch(
            &mut session,
            &format!(
                "CREATE TABLE dst_{i}(id INT IDENTITY(1,1),v NVARCHAR(10)); INSERT dst_{i}(v) {source}"
            ),
        );
        let rows: Vec<Vec<u8>> = session
            .db
            .prepare(&format!(
                "SELECT v.__msduck_utf16le FROM dst_{i} ORDER BY id"
            ))
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<duckdb::Result<_>>()
            .unwrap();
        let expected = if i == 2 {
            vec![vec![b'z', 0]]
        } else {
            vec![vec![b'x', 0], vec![b'z', 0]]
        };
        assert_eq!(rows, expected, "{source}");
    }
}

#[test]
fn ansi_set_insert_keeps_order_name_and_target_capacity() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    batch(
        &mut session,
        "CREATE TABLE a(v VARCHAR(10)); CREATE TABLE b(v VARCHAR(10)); INSERT a VALUES('z'); INSERT b VALUES('x'); CREATE TABLE dst(id INT IDENTITY(1,1),v VARCHAR(2)); INSERT dst(v) SELECT v FROM a UNION ALL SELECT v FROM b ORDER BY v OFFSET 0 ROWS;",
    );
    let rows: Vec<String> = session
        .db
        .prepare("SELECT v FROM dst ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<duckdb::Result<_>>()
        .unwrap();
    assert_eq!(rows, vec!["x", "z"]);
    batch(&mut session, "DELETE FROM dst; UPDATE a SET v='long';");
    let (_, ok) = session.batch_response(
        "INSERT dst(v) SELECT v FROM a UNION ALL SELECT v FROM b ORDER BY v OFFSET 0 ROWS",
        &Default::default(),
        false,
        None,
    );
    assert!(!ok, "overlong target must remain rejected");
    let count: i64 = session
        .db
        .query_row("SELECT COUNT(*) FROM dst", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0, "failed insertion must remain atomic");
}

#[test]
fn named_set_insert_preserves_raw_utf16() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    batch(
        &mut session,
        r#"CREATE TABLE a(v NVARCHAR(1)); INSERT a SELECT j.value FROM OPENJSON(N'["\ud800"]') j; CREATE TABLE dst(v NVARCHAR(1)); INSERT dst SELECT v FROM a UNION ALL SELECT v FROM a ORDER BY v OFFSET 0 ROWS;"#,
    );
    let rows: Vec<Vec<u8>> = session
        .db
        .prepare("SELECT v.__msduck_utf16le FROM dst")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<duckdb::Result<_>>()
        .unwrap();
    assert_eq!(rows, vec![vec![0, 0xd8], vec![0, 0xd8]]);
}
