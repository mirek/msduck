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
