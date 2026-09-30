//! ALTER DATABASE options, SINGLE_USER holds and the session registry with
//! in-process sessions. Termination over real connections is covered by
//! tests/alter_database_sessions.test.mjs.
use msduck::engine::Session;
use msduck::server::Server;

fn run(session: &mut Session, sql: &str) -> bool {
    session
        .batch_response(sql, &Default::default(), false, None)
        .1
}

fn options(session: &Session, name: &str) -> Option<(u8, String, bool)> {
    session
        .db
        .query_row(
            "SELECT user_access,user_access_desc,is_read_committed_snapshot_on FROM sys.databases WHERE name=?",
            [name],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .ok()
}

#[test]
fn sessions_get_the_lowest_free_spid_and_are_listed() {
    let server = Server::open(":memory:").unwrap();
    let a = Session::new(server.connection().unwrap()).unwrap();
    let b = Session::new(server.connection().unwrap()).unwrap();
    assert_eq!((a.spid(), b.spid()), (51, 52));
    drop(a);
    let c = Session::new(server.connection().unwrap()).unwrap();
    assert_eq!(c.spid(), 51);
    let listed: Vec<(i16, String, i16)> =
        c.db.prepare(
            "SELECT session_id,status,database_id FROM sys.dm_exec_sessions ORDER BY session_id",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        listed,
        [(51, "sleeping".into(), 1), (52, "sleeping".into(), 1)]
    );
}

#[test]
fn options_persist_across_restart() {
    let directory = std::env::temp_dir().join(format!(
        "msduck-alter-database-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("primary.duckdb");
    let path = path.to_str().unwrap();
    {
        let server = Server::open(path).unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        assert!(run(&mut session, "CREATE DATABASE probe_db"));
        assert_eq!(
            options(&session, "probe_db"),
            Some((0, "MULTI_USER".into(), false))
        );
        assert!(run(
            &mut session,
            "ALTER DATABASE [probe_db] SET READ_COMMITTED_SNAPSHOT ON, RESTRICTED_USER WITH ROLLBACK IMMEDIATE"
        ));
        assert_eq!(
            options(&session, "probe_db"),
            Some((2, "RESTRICTED_USER".into(), true))
        );
    }
    let server = Server::open(path).unwrap();
    let session = Session::new(server.connection().unwrap()).unwrap();
    assert_eq!(
        options(&session, "probe_db"),
        Some((2, "RESTRICTED_USER".into(), true))
    );
    assert_eq!(
        options(&session, "master"),
        Some((0, "MULTI_USER".into(), false))
    );
    drop(session);
    drop(server);
    std::fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn single_user_holds_follow_the_issuing_session() {
    let server = Server::open(":memory:").unwrap();
    let mut issuer = Session::new(server.connection().unwrap()).unwrap();
    let mut other = Session::new(server.connection().unwrap()).unwrap();
    assert!(run(&mut issuer, "CREATE DATABASE probe_db"));
    assert!(run(&mut issuer, "ALTER DATABASE probe_db SET SINGLE_USER"));
    // Only the issuer may enter, alter or drop the held database.
    assert!(other.use_database("probe_db").is_err());
    assert!(!run(&mut other, "DROP DATABASE probe_db"));
    assert!(!run(&mut other, "ALTER DATABASE probe_db SET MULTI_USER"));
    issuer.use_database("probe_db").unwrap();
    issuer.use_database("master").unwrap();
    // Setting another user access mode releases the hold.
    assert!(run(&mut issuer, "ALTER DATABASE probe_db SET MULTI_USER"));
    other.use_database("probe_db").unwrap();
    other.use_database("master").unwrap();
    // Disconnecting releases it too; the next session to enter holds it.
    assert!(run(&mut issuer, "ALTER DATABASE probe_db SET SINGLE_USER"));
    drop(issuer);
    other.use_database("probe_db").unwrap();
    let mut third = Session::new(server.connection().unwrap()).unwrap();
    assert!(third.use_database("probe_db").is_err());
    other.use_database("master").unwrap();
    assert!(run(&mut third, "DROP DATABASE probe_db"));
    assert_eq!(options(&third, "probe_db"), None);
}

#[test]
fn no_wait_refuses_while_another_session_uses_the_database() {
    let server = Server::open(":memory:").unwrap();
    let mut a = Session::new(server.connection().unwrap()).unwrap();
    let mut b = Session::new(server.connection().unwrap()).unwrap();
    assert!(run(&mut a, "CREATE DATABASE probe_db"));
    b.use_database("probe_db").unwrap();
    assert!(!run(
        &mut a,
        "ALTER DATABASE probe_db SET READ_COMMITTED_SNAPSHOT ON WITH NO_WAIT"
    ));
    assert_eq!(
        options(&a, "probe_db"),
        Some((0, "MULTI_USER".into(), false))
    );
}
