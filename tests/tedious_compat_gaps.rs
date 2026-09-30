//! In-process coverage for issue #697: ALTER DATABASE to an unchanged value
//! with other sessions, and computed columns across a restart. Client-level
//! behavior is covered by tests/tedious_compat_gaps.test.mjs.
use msduck::engine::Session;
use msduck::server::Server;

fn run(session: &mut Session, sql: &str) -> bool {
    session
        .batch_response(sql, &Default::default(), false, None)
        .1
}

fn rcsi(session: &Session, name: &str) -> bool {
    session
        .db
        .query_row(
            "SELECT is_read_committed_snapshot_on FROM sys.databases WHERE name=?",
            [name],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn unchanged_read_committed_snapshot_needs_no_exclusive_access() {
    let server = Server::open(":memory:").unwrap();
    let mut a = Session::new(server.connection().unwrap()).unwrap();
    let mut b = Session::new(server.connection().unwrap()).unwrap();
    assert!(run(&mut a, "CREATE DATABASE probe_db"));
    assert!(run(
        &mut a,
        "ALTER DATABASE probe_db SET READ_COMMITTED_SNAPSHOT ON WITH ROLLBACK IMMEDIATE"
    ));
    b.use_database("probe_db").unwrap();
    a.use_database("probe_db").unwrap();
    for sql in [
        "ALTER DATABASE CURRENT SET READ_COMMITTED_SNAPSHOT ON",
        "ALTER DATABASE probe_db SET READ_COMMITTED_SNAPSHOT ON WITH NO_WAIT",
        "ALTER DATABASE probe_db SET MULTI_USER",
        "ALTER DATABASE probe_db SET READ_COMMITTED_SNAPSHOT ON, MULTI_USER",
    ] {
        assert!(run(&mut a, sql), "{sql}");
    }
    // A changed value still needs the other session gone.
    assert!(!run(
        &mut a,
        "ALTER DATABASE probe_db SET READ_COMMITTED_SNAPSHOT OFF WITH NO_WAIT"
    ));
    assert!(!run(
        &mut a,
        "ALTER DATABASE CURRENT SET READ_COMMITTED_SNAPSHOT OFF"
    ));
    assert!(rcsi(&a, "probe_db"));
    // The other session was not terminated.
    assert!(run(&mut b, "SELECT 1"));
    drop(b);
    assert!(run(
        &mut a,
        "ALTER DATABASE CURRENT SET READ_COMMITTED_SNAPSHOT OFF"
    ));
    assert!(!rcsi(&a, "probe_db"));
}

#[test]
fn computed_columns_survive_restart() {
    let directory = std::env::temp_dir().join(format!(
        "msduck-computed-columns-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("primary.duckdb");
    let path = path.to_str().unwrap();
    let computed = |session: &Session| -> Vec<(String, bool)> {
        session
            .db
            .prepare("SELECT name,is_computed FROM sys.columns WHERE object_id=__msduck_object_id('dbo.probe','U') ORDER BY column_id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    {
        let server = Server::open(path).unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        assert!(run(
            &mut session,
            "CREATE TABLE probe (id int NOT NULL PRIMARY KEY, name varchar(20) NULL, upper_name AS UPPER(name) PERSISTED)"
        ));
        assert!(run(&mut session, "INSERT INTO probe VALUES (1, 'a')"));
    }
    let server = Server::open(path).unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    assert_eq!(
        computed(&session),
        [
            ("id".into(), false),
            ("name".into(), false),
            ("upper_name".into(), true)
        ]
    );
    assert!(run(
        &mut session,
        "INSERT INTO probe (id, name) VALUES (2, 'b')"
    ));
    assert!(!run(
        &mut session,
        "UPDATE probe SET upper_name = 'X' WHERE id = 1"
    ));
    let rows: Vec<(i32, String)> = session
        .db
        .prepare("SELECT id, upper_name FROM probe ORDER BY id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(rows, [(1, "A".into()), (2, "B".into())]);
    // A table recreated with the same name has no stale computed columns.
    assert!(run(&mut session, "DROP TABLE probe"));
    assert!(run(
        &mut session,
        "CREATE TABLE probe (id int, name varchar(20), upper_name varchar(20))"
    ));
    assert!(computed(&session).iter().all(|(_, computed)| !computed));
    drop(session);
    drop(server);
    std::fs::remove_dir_all(&directory).unwrap();
}

fn column<T: duckdb::types::FromSql>(session: &Session, sql: &str) -> Vec<T> {
    session
        .db
        .prepare(sql)
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn stored_definitions_read_the_clock_when_used() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    assert!(run(
        &mut session,
        "CREATE TABLE stamped (id int, created datetime2(7) DEFAULT SYSUTCDATETIME(), legacy datetime DEFAULT GETDATE(), zoned datetimeoffset(7) DEFAULT SYSDATETIMEOFFSET())"
    ));
    assert!(run(&mut session, "INSERT INTO stamped (id) VALUES (1)"));
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert!(run(&mut session, "INSERT INTO stamped (id) VALUES (2)"));
    let distinct: Vec<i64> = column(
        &session,
        "SELECT count(DISTINCT created) * 100 + count(DISTINCT legacy) FROM stamped",
    );
    assert_eq!(distinct, [202]);
    assert!(run(&mut session, "SELECT zoned FROM stamped"));
    // A view evaluates the clock per query, not at CREATE VIEW.
    assert!(run(
        &mut session,
        "CREATE VIEW clock AS SELECT SYSUTCDATETIME() AS now"
    ));
    let first: Vec<String> = column(&session, "SELECT CAST(now AS VARCHAR) FROM clock");
    std::thread::sleep(std::time::Duration::from_millis(50));
    let second: Vec<String> = column(&session, "SELECT CAST(now AS VARCHAR) FROM clock");
    assert_ne!(first, second);
    // A login name would be fixed to the creator; it fails explicitly.
    assert!(!run(
        &mut session,
        "CREATE TABLE owned (id int, who nvarchar(128) DEFAULT SUSER_SNAME())"
    ));
}

#[test]
fn computed_column_checks_and_dropped_columns() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    // Date part arguments are not column references.
    assert!(run(
        &mut session,
        "CREATE TABLE due (d date, next_day AS DATEADD(day, 1, d), age AS DATEDIFF(day, d, d))"
    ));
    assert!(run(
        &mut session,
        "INSERT INTO due (d) VALUES ('2026-09-30')"
    ));
    let next: Vec<String> = column(&session, "SELECT CAST(next_day AS VARCHAR) FROM due");
    assert_eq!(next.len(), 1);
    assert!(next[0].starts_with("2026-10-01"), "{next:?}");
    // A dropped computed column does not mark a later column of that name.
    assert!(run(&mut session, "CREATE TABLE t (a int, c AS a + 1)"));
    assert!(run(&mut session, "ALTER TABLE t DROP COLUMN c"));
    assert!(run(&mut session, "ALTER TABLE t ADD c int"));
    assert!(run(&mut session, "INSERT INTO t (a, c) VALUES (1, 2)"));
    assert!(run(&mut session, "UPDATE t SET c = 3"));
    let computed: Vec<bool> = column(
        &session,
        "SELECT is_computed FROM sys.columns WHERE object_id=__msduck_object_id('dbo.t','U') ORDER BY column_id",
    );
    assert_eq!(computed, [false, false]);
}
