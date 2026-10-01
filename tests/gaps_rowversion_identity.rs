//! rowversion columns, decimal identity, SCOPE_IDENTITY and SET
//! IDENTITY_INSERT (docs/gaps-rowversion_identity.md): values and SQL Server
//! error numbers, and the database-wide counter and identity allocators
//! across a restart that replays the write-ahead log.
use msduck::engine::Session;
use msduck::server::Server;

fn run(session: &mut Session, sql: &str) -> (Vec<u8>, bool) {
    session.batch_response(sql, &Default::default(), false, None)
}

fn ok(session: &mut Session, sql: &str) {
    let (response, ok) = run(session, sql);
    assert!(ok && !response.contains(&0xAA), "{sql}: {response:?}");
}

/// The bytes of an ERROR token's number, state, class and message.
fn error_body(number: i32, state: u8, message: &str) -> Vec<u8> {
    let units: Vec<u16> = message.encode_utf16().collect();
    let mut body = number.to_le_bytes().to_vec();
    body.extend([state, 16]);
    body.extend((units.len() as u16).to_le_bytes());
    body.extend(units.iter().flat_map(|u| u.to_le_bytes()));
    body
}

fn fails(session: &mut Session, sql: &str, number: i32, state: u8, message: &str) {
    let (response, _) = run(session, sql);
    let body = error_body(number, state, message);
    assert!(
        response.windows(body.len()).any(|w| w == body.as_slice()),
        "{sql}: expected {number} {message}: {response:?}"
    );
}

fn text(session: &Session, sql: &str) -> String {
    session.db.query_row(sql, [], |r| r.get(0)).unwrap()
}

fn rowversions(session: &Session, table: &str) -> Vec<String> {
    session
        .db
        .prepare(&format!("SELECT hex(rv) FROM dbo.{table} ORDER BY id"))
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<duckdb::Result<_>>()
        .unwrap()
}

#[test]
fn rowversion_values_errors_and_counter() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    ok(
        &mut session,
        "CREATE TABLE dbo.items(id int, value int, rv rowversion NOT NULL)
         CREATE TABLE dbo.stamps(id int, timestamp)
         INSERT dbo.items(id, value) VALUES(1, 10), (2, 20)
         INSERT dbo.stamps(id) VALUES(1)
         INSERT dbo.items VALUES(3, 30, DEFAULT), (4, 40, NULL)
         INSERT dbo.items(id, value, rv) SELECT 5, 50, NULL",
    );
    assert_eq!(
        rowversions(&session, "items"),
        [
            "00000000000007D1",
            "00000000000007D2",
            "00000000000007D4",
            "00000000000007D5",
            "00000000000007D6"
        ]
    );
    assert_eq!(
        text(&session, "SELECT hex(timestamp) FROM dbo.stamps"),
        "00000000000007D3"
    );
    ok(
        &mut session,
        "UPDATE dbo.items SET value = value WHERE id = 2",
    );
    assert_eq!(rowversions(&session, "items")[1], "00000000000007D7");
    fails(
        &mut session,
        "INSERT dbo.items(id, value, rv) VALUES(6, 60, 0x01)",
        273,
        1,
        "Cannot insert an explicit value into a timestamp column. Use INSERT with a column list to exclude the timestamp column, or insert a DEFAULT into the timestamp column.",
    );
    fails(
        &mut session,
        "INSERT dbo.items VALUES(6, 60)",
        213,
        1,
        "Column name or number of supplied values does not match table definition.",
    );
    fails(
        &mut session,
        "UPDATE dbo.items SET rv = DEFAULT",
        272,
        1,
        "Cannot update a timestamp column.",
    );
    fails(
        &mut session,
        "CREATE TABLE dbo.two(a rowversion, b timestamp)",
        2738,
        2,
        "A table can only have one timestamp column. Because table 'two' already has one, the column 'b' cannot be added.",
    );
    fails(
        &mut session,
        "CREATE TABLE dbo.defaulted(rv rowversion DEFAULT 0x01)",
        1755,
        0,
        "Defaults cannot be created on columns of data type timestamp. Table 'defaulted', column 'rv'.",
    );
    // The counter is shared by the database's tables and never rolls back.
    ok(
        &mut session,
        "BEGIN TRAN INSERT dbo.stamps(id) VALUES(2) ROLLBACK",
    );
    ok(&mut session, "INSERT dbo.stamps(id) VALUES(3)");
    assert_eq!(
        text(
            &session,
            "SELECT hex(timestamp) FROM dbo.stamps WHERE id = 3"
        ),
        "00000000000007D9"
    );
}

#[test]
fn identity_values_errors_and_identity_insert() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    ok(
        &mut session,
        "CREATE TABLE dbo.items(id numeric(10,0) IDENTITY(100,5), value int)
         INSERT dbo.items(value) VALUES(1), (2)",
    );
    assert_eq!(
        text(
            &session,
            "SELECT string_agg(CAST(id AS VARCHAR), ',' ORDER BY id) FROM dbo.items"
        ),
        "100,105"
    );
    fails(
        &mut session,
        "CREATE TABLE dbo.bad(id decimal(5,2) IDENTITY, value int)",
        2749,
        2,
        "Identity column 'id' must be of data type int, bigint, smallint, tinyint, or decimal or numeric with a scale of 0, unencrypted, and constrained to be nonnullable.",
    );
    fails(
        &mut session,
        "INSERT dbo.items(id, value) VALUES(1, 1)",
        544,
        1,
        "Cannot insert explicit value for identity column in table 'items' when IDENTITY_INSERT is set to OFF.",
    );
    ok(
        &mut session,
        "CREATE TABLE dbo.other(id int IDENTITY, v int)",
    );
    ok(&mut session, "SET IDENTITY_INSERT dbo.items ON");
    fails(
        &mut session,
        "SET IDENTITY_INSERT other ON",
        8107,
        1,
        "IDENTITY_INSERT is already ON for table 'master.dbo.items'. Cannot perform SET operation for table 'other'.",
    );
    fails(
        &mut session,
        "INSERT dbo.items(value) VALUES(3)",
        545,
        1,
        "Explicit value must be specified for identity column in table 'items' either when IDENTITY_INSERT is set to ON or when a replication user is inserting into a NOT FOR REPLICATION identity column.",
    );
    fails(
        &mut session,
        "INSERT dbo.items(id, value) VALUES(DEFAULT, 3)",
        339,
        1,
        "DEFAULT or NULL are not allowed as explicit identity values.",
    );
    ok(
        &mut session,
        "INSERT dbo.items(id, value) VALUES(500, 3), (7, 4)
         SET IDENTITY_INSERT dbo.items OFF
         INSERT dbo.items(value) VALUES(5)",
    );
    assert_eq!(
        text(
            &session,
            "SELECT string_agg(CAST(id AS VARCHAR), ',' ORDER BY id) FROM dbo.items"
        ),
        "7,100,105,500,505"
    );
}

#[test]
fn counter_and_allocators_survive_write_ahead_log_replay() {
    let directory = std::env::temp_dir().join(format!(
        "msduck-gaps-rowversion-identity-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("rowversion.duckdb");
    let path = path.to_str().unwrap();
    {
        let server = Server::open(path).unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        // Leave every statement in the write-ahead log, so reopening must
        // bind the column defaults while replaying it.
        session
            .db
            .execute_batch("PRAGMA disable_checkpoint_on_shutdown")
            .unwrap();
        ok(
            &mut session,
            "CREATE TABLE dbo.items(id decimal(12,0) IDENTITY(10,10) NOT NULL, rv rowversion)
             INSERT dbo.items DEFAULT VALUES
             SET IDENTITY_INSERT dbo.items ON
             INSERT dbo.items(id) VALUES(1000)
             SET IDENTITY_INSERT dbo.items OFF",
        );
    }
    for restart in 0..2 {
        let server = Server::open(path).unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        ok(&mut session, "INSERT dbo.items DEFAULT VALUES");
        let rows: Vec<(String, String)> = session
            .db
            .prepare("SELECT CAST(id AS VARCHAR), hex(rv) FROM dbo.items ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<duckdb::Result<_>>()
            .unwrap();
        let mut expected = vec![
            ("10".to_owned(), "00000000000007D1".to_owned()),
            ("1000".to_owned(), "00000000000007D2".to_owned()),
            ("1010".to_owned(), "00000000000007D3".to_owned()),
        ];
        if restart == 1 {
            expected.push(("1020".to_owned(), "00000000000007D4".to_owned()));
        }
        assert_eq!(rows, expected, "{restart}");
    }
    let _ = std::fs::remove_dir_all(&directory);
}
