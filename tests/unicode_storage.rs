//! Declared Unicode storage must preserve SQL Server code units across writes.
use msduck::{engine::Session, server::Server};

fn execute(session: &mut Session, sql: &str) {
    assert!(
        session
            .batch_response(sql, &Default::default(), false, None)
            .1,
        "{sql}"
    );
}
fn bytes(session: &Session, sql: &str) -> Vec<Option<Vec<u8>>> {
    session
        .db
        .prepare(sql)
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<duckdb::Result<Vec<_>>>()
        .unwrap()
}

#[test]
fn ansi_storage_accepts_mixed_text_carriers_and_keeps_width_failure_atomic() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    execute(
        &mut session,
        "CREATE TABLE dbo.ansi_carriers(id INT,v VARCHAR(2),f CHAR(2)); INSERT INTO dbo.ansi_carriers VALUES(1,N'e',N'e'),(2,LEFT(N'€x',1),LEFT(N'€x',1)),(3,NULL,NULL)",
    );
    let rows = session
        .db
        .prepare("SELECT v,f FROM dbo.ansi_carriers ORDER BY id")
        .unwrap()
        .query_map([], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?,
                r.get::<_, Option<String>>(1)?,
            ))
        })
        .unwrap()
        .collect::<duckdb::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        rows,
        vec![
            (Some("e".into()), Some("e ".into())),
            (Some("€".into()), Some("€ ".into())),
            (None, None)
        ]
    );
    assert!(
        !session
            .batch_response(
                "INSERT INTO dbo.ansi_carriers VALUES(4,N'ok',N'ok'),(5,LEFT(N'abcx',3),N'x')",
                &Default::default(),
                false,
                None
            )
            .1
    );
    let count: i64 = session
        .db
        .query_row("SELECT count(*) FROM dbo.ansi_carriers", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 3);
    execute(
        &mut session,
        "UPDATE dbo.ansi_carriers SET v=LEFT(N'€x',1),f=LEFT(N'€x',1) WHERE id=1",
    );
    let row: (String, String) = session
        .db
        .query_row("SELECT v,f FROM dbo.ansi_carriers WHERE id=1", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(row, ("€".into(), "€ ".into()));
}

#[test]
fn declared_unicode_columns_preserve_raw_units_and_atomic_width_checks() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    execute(
        &mut session,
        "CREATE TABLE dbo.units(id INT,s NVARCHAR(3),f NCHAR(3) DEFAULT N'x')",
    );
    execute(
        &mut session,
        "INSERT INTO dbo.units(id,s) VALUES(1,N'ok'),(2,LEFT(N'🦆',1)),(3,RIGHT(N'🦆',1)),(4,NULL)",
    );
    assert_eq!(
        bytes(
            &session,
            "SELECT s.__msduck_utf16le FROM dbo.units ORDER BY id"
        ),
        vec![
            Some(vec![111, 0, 107, 0]),
            Some(vec![0x3e, 0xd8]),
            Some(vec![0x86, 0xdd]),
            None
        ]
    );
    assert_eq!(
        bytes(
            &session,
            "SELECT f.__msduck_utf16le FROM dbo.units WHERE id=2"
        ),
        vec![Some(vec![120, 0, 32, 0, 32, 0])]
    );
    execute(
        &mut session,
        "UPDATE dbo.units SET s=LEFT(N'🦆',1) WHERE id=1",
    );
    assert!(
        !session
            .batch_response(
                "UPDATE dbo.units SET s=N'abcd'",
                &Default::default(),
                false,
                None
            )
            .1
    );
    assert_eq!(
        bytes(
            &session,
            "SELECT s.__msduck_utf16le FROM dbo.units WHERE id=1"
        ),
        vec![Some(vec![0x3e, 0xd8])]
    );
    assert!(
        !session
            .batch_response(
                "INSERT INTO dbo.units(id,s) VALUES(5,N'x'),(6,N'abcd')",
                &Default::default(),
                false,
                None
            )
            .1
    );
    assert_eq!(
        session
            .db
            .query_row("SELECT count(*) FROM dbo.units", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        4
    );
    execute(&mut session, "UPDATE dbo.units SET s=N'ab   ' WHERE id=1");
    assert_eq!(
        bytes(
            &session,
            "SELECT s.__msduck_utf16le FROM dbo.units WHERE id=1"
        ),
        vec![Some(vec![97, 0, 98, 0, 32, 0])]
    );
}

#[test]
fn unicode_storage_reopens_and_alter_preserves_units_transactionally() {
    let path = std::env::temp_dir().join(format!(
        "msduck-unicode-store-{}-{}.duckdb",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    {
        let server = Server::open(path.to_str().unwrap()).unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        execute(
            &mut session,
            "CREATE TABLE dbo.saved(id INT,s NVARCHAR(3),f NCHAR(3) NOT NULL DEFAULT N'x');INSERT INTO dbo.saved(id,s) VALUES(1,LEFT(N'🦆',1))",
        );
        execute(
            &mut session,
            "BEGIN TRAN;ALTER TABLE dbo.saved ALTER COLUMN s NCHAR(4);ROLLBACK",
        );
        assert_eq!(
            bytes(&session, "SELECT s.__msduck_utf16le FROM dbo.saved"),
            vec![Some(vec![0x3e, 0xd8])]
        );
        execute(
            &mut session,
            "ALTER TABLE dbo.saved ALTER COLUMN s NCHAR(4)",
        );
    }
    {
        let server = Server::open(path.to_str().unwrap()).unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        assert_eq!(
            bytes(&session, "SELECT s.__msduck_utf16le FROM dbo.saved"),
            vec![Some(vec![0x3e, 0xd8, 32, 0, 32, 0, 32, 0])]
        );
        execute(
            &mut session,
            "INSERT INTO dbo.saved(id,s) VALUES(2,RIGHT(N'🦆',1))",
        );
        assert_eq!(
            bytes(
                &session,
                "SELECT f.__msduck_utf16le FROM dbo.saved WHERE id=2"
            ),
            vec![Some(vec![120, 0, 32, 0, 32, 0])]
        );
    }
    std::fs::remove_file(path).unwrap();
}

#[test]
fn adding_required_unicode_default_populates_existing_rows_atomically() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    execute(
        &mut session,
        "CREATE TABLE dbo.add_units(id INT); INSERT INTO dbo.add_units VALUES(1)",
    );
    execute(
        &mut session,
        "ALTER TABLE dbo.add_units ADD f NCHAR(3) NOT NULL DEFAULT N'x'",
    );
    assert_eq!(
        bytes(&session, "SELECT f.__msduck_utf16le FROM dbo.add_units"),
        vec![Some(vec![120, 0, 32, 0, 32, 0])]
    );
}

#[test]
fn explicit_unicode_alter_upgrades_legacy_utf8_without_losing_values() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    execute(
        &mut session,
        "CREATE TABLE dbo.legacy_units(s NVARCHAR(3));INSERT INTO dbo.legacy_units VALUES(N'🦆')",
    );
    // Simulate the previous physical layout while retaining the logical catalog.
    session
        .db
        .execute_batch(
            "ALTER TABLE dbo.legacy_units ALTER COLUMN s SET DATA TYPE VARCHAR USING '🦆'",
        )
        .unwrap();
    execute(
        &mut session,
        "ALTER TABLE dbo.legacy_units ALTER COLUMN s NCHAR(3)",
    );
    assert_eq!(
        bytes(&session, "SELECT s.__msduck_utf16le FROM dbo.legacy_units"),
        vec![Some(vec![0x3e, 0xd8, 0x86, 0xdd, 32, 0])]
    );
}

#[test]
fn guarded_unicode_add_rejects_duplicates_and_rolls_back_prior_actions() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    execute(
        &mut session,
        "CREATE TABLE dbo.duplicate_units(id INT,s NCHAR(3) DEFAULT N'a');INSERT INTO dbo.duplicate_units(id) VALUES(1)",
    );
    assert!(
        !session
            .batch_response(
                "ALTER TABLE dbo.duplicate_units ADD s NCHAR(3) NOT NULL DEFAULT N'b'",
                &Default::default(),
                false,
                None
            )
            .1
    );
    assert_eq!(
        bytes(
            &session,
            "SELECT s.__msduck_utf16le FROM dbo.duplicate_units"
        ),
        vec![Some(vec![97, 0, 32, 0, 32, 0])]
    );
    assert!(!session.batch_response("ALTER TABLE dbo.duplicate_units ADD fresh NCHAR(3) NOT NULL DEFAULT N'x',s NCHAR(3) NOT NULL DEFAULT N'y'",&Default::default(),false,None).1);
    assert_eq!(session.db.query_row("SELECT count(*) FROM information_schema.columns WHERE table_name='duplicate_units' AND column_name='fresh'",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    execute(
        &mut session,
        "BEGIN TRAN;ALTER TABLE dbo.duplicate_units ADD fresh NCHAR(3) NOT NULL DEFAULT N'x';ROLLBACK",
    );
    assert_eq!(session.db.query_row("SELECT count(*) FROM information_schema.columns WHERE table_name='duplicate_units' AND column_name='fresh'",[],|r|r.get::<_,i64>(0)).unwrap(),0);
}
