//! The database's case-insensitive default collation (issue #868): key
//! indexes drop ignorable units and fold case with built-in DuckDB functions
//! only, so they bind while DuckDB replays its WAL before msduck registers
//! its functions, and they keep enforcing SQL Server's equality after a
//! restart.
use msduck::engine::Session;
use msduck::server::Server;
use msduck_sql::dialect::ext::keys::value;

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

#[test]
fn folded_unicode_keys_apply_the_fold_to_every_unit_in_duckdb() {
    let server = Server::open(":memory:").unwrap();
    let session = Session::new(server.connection().unwrap()).unwrap();
    // Every unit of the folded blocks, ignorable units, and trailing spaces
    // that only become trailing once the ignorable units are dropped.
    let mut units: Vec<u16> = (0u16..0x500).collect();
    units.extend([
        0xD83E, 0xDD86, 0x200D, 0x200E, 0xFEFF, 0xFFFE, 0xFFFF, 0xDC00,
    ]);
    units.extend([0x20, 0, 0x20, 0xD800]);
    let hex = |units: &[u16]| -> String {
        units
            .iter()
            .flat_map(|u| u.to_le_bytes())
            .map(|b| format!("{b:02X}"))
            .collect()
    };
    // One value holding every unit, so misaligned matches would show.
    let key: String = session
        .db
        .query_row(
            &format!(
                "SELECT {} FROM (SELECT {{'__msduck_utf16le': from_hex(?)}} AS c)",
                value::folded_unicode_key("c")
            ),
            [hex(&units)],
            |r| r.get(0),
        )
        .unwrap();
    let mut folded: Vec<u16> = units
        .iter()
        .filter(|u| !value::ignorable(**u))
        .map(|u| value::key_fold(*u))
        .collect();
    while folded.last() == Some(&0x20) {
        folded.pop();
    }
    assert_eq!(key, hex(&folded));
    assert!(!folded.contains(&0) && folded.len() > 0x400);
    for (unit, lower) in [
        (0x41u16, 0x61u16),
        (0xC9, 0xE9),
        (0x100, 0x101),
        (0x139, 0x13A),
    ] {
        assert_eq!(value::key_fold(unit), lower, "{unit:04X}");
    }
}

fn checks(session: &mut Session) {
    fails(
        session,
        "INSERT dbo.people VALUES (N'ANN', 'x', N'm1')",
        2627,
        "Violation of PRIMARY KEY constraint 'pk_people'. Cannot insert duplicate key in object 'dbo.people'. The duplicate key value is (ANN).",
    );
    fails(
        session,
        "INSERT dbo.people VALUES (N'bob', 'CODE  ', N'm2')",
        2627,
        "Violation of UNIQUE KEY constraint 'uq_people_code'. Cannot insert duplicate key in object 'dbo.people'. The duplicate key value is (CODE  ).",
    );
    fails(
        session,
        "INSERT dbo.people VALUES (N'cy', 'other', N'mail  ')",
        2601,
        "Cannot insert duplicate key row in object 'dbo.people' with unique index 'ux_people_mail'. The duplicate key value is (mail  ).",
    );
    // A case-sensitive column keeps case-sensitive keys.
    ok(
        session,
        "INSERT dbo.people VALUES (N'dee', 'other', N'MAIL')",
    );
    ok(session, "DELETE dbo.people WHERE name = N'DEE'");
}

#[test]
fn case_insensitive_keys_survive_restart_and_write_ahead_log_replay() {
    let directory =
        std::env::temp_dir().join(format!("msduck-default-collation-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("collation.duckdb");
    let path = path.to_str().unwrap();
    {
        let server = Server::open(path).unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        session
            .db
            .execute_batch("PRAGMA disable_checkpoint_on_shutdown")
            .unwrap();
        ok(
            &mut session,
            "CREATE TABLE dbo.people (name nvarchar(40) NOT NULL CONSTRAINT pk_people PRIMARY KEY, code varchar(10) NULL CONSTRAINT uq_people_code UNIQUE, mail nvarchar(40) COLLATE Latin1_General_CS_AS NULL)
             CREATE UNIQUE INDEX ux_people_mail ON dbo.people(mail)
             INSERT dbo.people VALUES (N'Ann', 'code', N'mail')",
        );
        checks(&mut session);
    }
    for _ in 0..2 {
        let server = Server::open(path).unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        checks(&mut session);
        assert_eq!(count(&session, "SELECT count(*) FROM dbo.people"), 1);
        assert_eq!(
            count(
                &session,
                "SELECT count(*) FROM main.__msduck_declared_columns WHERE collation_name = 'Latin1_General_CS_AS'"
            ),
            1
        );
    }
    let _ = std::fs::remove_dir_all(&directory);
}
