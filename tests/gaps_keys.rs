//! Keys-managed indexes and key records (docs/gaps-keys.md) survive a
//! restart, including WAL replay before msduck registers its functions, and
//! keep SQL Server's duplicate-key errors.
use msduck::engine::Session;
use msduck::server::Server;

fn run(session: &mut Session, sql: &str) -> (Vec<u8>, bool) {
    session.batch_response(sql, &Default::default(), false, None)
}

/// The bytes of an ERROR token's number, state, class and message.
fn error_body(number: i32, state: u8, class: u8, message: &str) -> Vec<u8> {
    let units: Vec<u16> = message.encode_utf16().collect();
    let mut body = number.to_le_bytes().to_vec();
    body.extend([state, class]);
    body.extend((units.len() as u16).to_le_bytes());
    body.extend(units.iter().flat_map(|u| u.to_le_bytes()));
    body
}

fn fails(session: &mut Session, sql: &str, number: i32, message: &str) {
    let (response, _) = run(session, sql);
    let body = error_body(number, 1, 14, message);
    assert!(
        response.windows(body.len()).any(|w| w == body.as_slice()),
        "{sql}: expected {number} {message}"
    );
}

fn ok(session: &mut Session, sql: &str) {
    let (response, ok) = run(session, sql);
    assert!(ok && !response.contains(&0xAA), "{sql}: {response:?}");
}

fn count(session: &Session, sql: &str) -> i64 {
    session.db.query_row(sql, [], |r| r.get(0)).unwrap()
}

fn checks(session: &mut Session) {
    fails(
        session,
        "INSERT dbo.items VALUES (N'abc', NULL, NULL)",
        2627,
        "Violation of PRIMARY KEY constraint 'pk_items'. Cannot insert duplicate key in object 'dbo.items'. The duplicate key value is (abc).",
    );
    fails(
        session,
        "INSERT dbo.items VALUES (N'new1', NULL, NULL)",
        2627,
        "Violation of UNIQUE KEY constraint 'uq_items_code'. Cannot insert duplicate key in object 'dbo.items'. The duplicate key value is (<NULL>).",
    );
    fails(
        session,
        "INSERT dbo.items VALUES (N'new2', 7, N'mail')",
        2601,
        "Cannot insert duplicate key row in object 'dbo.items' with unique index 'ux_items_email'. The duplicate key value is (mail).",
    );
    fails(
        session,
        "INSERT dbo.events VALUES (1, '2024-01-02 09:00:00 +00:00')",
        2627,
        "Violation of PRIMARY KEY constraint 'pk_events'. Cannot insert duplicate key in object 'dbo.events'. The duplicate key value is (1, 2024-01-02 09:00:00.0000000 +00:00).",
    );
}

#[test]
fn managed_keys_survive_restart_and_write_ahead_log_replay() {
    let directory = std::env::temp_dir().join(format!("msduck-gaps-keys-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("keys.duckdb");
    let path = path.to_str().unwrap();
    {
        let server = Server::open(path).unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        // Leave every statement in the write-ahead log, so reopening must
        // bind the index expressions while replaying it.
        session
            .db
            .execute_batch("PRAGMA disable_checkpoint_on_shutdown")
            .unwrap();
        ok(
            &mut session,
            "CREATE TABLE dbo.items (id nvarchar(40) NOT NULL CONSTRAINT pk_items PRIMARY KEY NONCLUSTERED, code int NULL CONSTRAINT uq_items_code UNIQUE, email nvarchar(100) NULL)
             CREATE UNIQUE INDEX ux_items_email ON dbo.items(email) WHERE email IS NOT NULL
             CREATE CLUSTERED INDEX ix_items_code ON dbo.items(code)
             CREATE TABLE dbo.events (id int NOT NULL, occurred datetimeoffset NOT NULL, CONSTRAINT pk_events PRIMARY KEY (id, occurred))
             INSERT dbo.items VALUES (N'abc', NULL, N'mail'), (N'abd', 1, NULL), (N'abe', 2, NULL)
             INSERT dbo.events VALUES (1, '2024-01-02 10:00:00 +01:00')",
        );
        checks(&mut session);
    }
    for restart in 0..2 {
        let server = Server::open(path).unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        checks(&mut session);
        assert_eq!(
            count(&session, "SELECT count(*) FROM dbo.items"),
            3,
            "{restart}"
        );
        assert_eq!(
            count(&session, "SELECT count(*) FROM main.__msduck_keys"),
            if restart == 0 { 5 } else { 4 },
            "{restart}"
        );
        if restart == 0 {
            ok(&mut session, "DROP INDEX ix_items_code ON dbo.items");
            // The index of a constraint cannot be dropped.
            let (response, _) = run(&mut session, "DROP INDEX pk_items ON dbo.items");
            let body = error_body(
                3723,
                4,
                16,
                "An explicit DROP INDEX is not allowed on index 'dbo.items.pk_items'. It is being used for PRIMARY KEY constraint enforcement.",
            );
            assert!(response.windows(body.len()).any(|w| w == body.as_slice()));
        }
    }
    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn key_records_follow_their_tables_and_index_expressions_use_built_in_functions() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    ok(
        &mut session,
        "CREATE TABLE dbo.t (id nvarchar(10) NOT NULL PRIMARY KEY, d datetime2(3) NULL UNIQUE, v varchar(10) NULL, b varbinary(8) NULL)
         CREATE UNIQUE INDEX ux_t ON dbo.t(v, b) WHERE v IN ('a', 'b') AND id = N'x'",
    );
    let definitions: Vec<String> = session
        .db
        .prepare("SELECT sql FROM duckdb_indexes() WHERE table_name = 't' ORDER BY index_name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<duckdb::Result<_>>()
        .unwrap();
    assert_eq!(definitions.len(), 3);
    for definition in &definitions {
        // Struct field names only; no msduck function.
        let (_, expressions) = definition.split_once(" ON ").unwrap();
        let functions = expressions
            .replace("__msduck_utf16le", "")
            .replace("__msduck_datetime2_3", "");
        assert!(!functions.contains("__msduck"), "{definition}");
    }
    // Generated names follow SQL Server's pattern.
    let names: Vec<String> = session
        .db
        .prepare("SELECT name FROM main.__msduck_keys ORDER BY tag")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<duckdb::Result<_>>()
        .unwrap();
    assert_eq!(names.len(), 3);
    assert!(names[0].starts_with("PK__t__") && names[0].len() == "PK__t__".len() + 16);
    assert!(names[1].starts_with("UQ__t__"));
    assert_eq!(names[2], "ux_t");
    // Dropping and recreating the table forgets the old records.
    ok(&mut session, "DROP TABLE dbo.t");
    ok(
        &mut session,
        "CREATE TABLE dbo.t (id int NOT NULL CONSTRAINT pk_t PRIMARY KEY)",
    );
    ok(&mut session, "INSERT dbo.t VALUES (1)");
    fails(
        &mut session,
        "INSERT dbo.t VALUES (1)",
        2627,
        "Violation of PRIMARY KEY constraint 'pk_t'. Cannot insert duplicate key in object 'dbo.t'. The duplicate key value is (1).",
    );
    assert_eq!(
        count(&session, "SELECT count(*) FROM main.__msduck_keys"),
        1
    );
    // A failed CREATE TABLE leaves neither the table nor key records.
    let (_, created) = run(
        &mut session,
        "CREATE TABLE dbo.u (a nvarchar(10) NOT NULL CONSTRAINT pk_t PRIMARY KEY)",
    );
    assert!(!created);
    assert_eq!(
        count(
            &session,
            "SELECT count(*) FROM duckdb_tables() WHERE table_name = 'u'"
        ),
        0
    );
    assert_eq!(
        count(&session, "SELECT count(*) FROM main.__msduck_keys"),
        1
    );
}
