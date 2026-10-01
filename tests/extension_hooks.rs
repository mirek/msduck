//! The extension hook scaffold (docs/extension-hooks.md) keeps existing
//! behavior: modules appear in sys.objects only once stored, and sessions
//! open, reset and close through the feature lifecycle hooks.
use msduck::engine::Session;
use msduck::server::Server;

fn run(session: &mut Session, sql: &str) -> bool {
    session
        .batch_response(sql, &Default::default(), false, None)
        .1
}

#[test]
fn catalogs_and_sessions_survive_the_hooks_across_databases_and_restart() {
    let directory = std::env::temp_dir().join(format!("msduck-ext-hooks-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("primary.duckdb");
    let path = path.to_str().unwrap();
    for _ in 0..2 {
        let server = Server::open(path).unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        assert!(run(
            &mut session,
            "IF DB_ID('hooks_db') IS NULL CREATE DATABASE hooks_db"
        ));
        for database in ["master", "hooks_db"] {
            session.use_database(database).unwrap();
            assert!(run(
                &mut session,
                "IF OBJECT_ID('dbo.t') IS NULL CREATE TABLE dbo.t(id INT)"
            ));
            let objects: i64 = session
                .db
                .query_row(
                    "SELECT count(*) FROM sys.all_objects WHERE name = 't' AND type = 'U '",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(objects, 1, "{database}");
            let modules: i64 = session
                .db
                .query_row("SELECT count(*) FROM main.__msduck_modules", [], |r| {
                    r.get(0)
                })
                .unwrap();
            assert_eq!(modules, 0, "{database}");
        }
        // A second session opens and closes through the lifecycle hooks.
        let other = Session::new(server.connection().unwrap()).unwrap();
        drop(other);
    }
    let _ = std::fs::remove_dir_all(&directory);
}
