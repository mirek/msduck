//! Catalog bootstrap and persistent shared-module projections. Procedure DDL
//! is supplied by its separate extension; seed its documented backing store
//! here so catalog regressions do not depend on that extension landing first.
use msduck::{engine::Session, server::Server};

#[test]
fn procedure_schema_and_database_isolation_survive_restart() {
    let directory = std::env::temp_dir().join(format!(
        "msduck-catalog-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("primary.duckdb");
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("../reference/gaps-catalog.json")).unwrap();
    let expected = reference["runs"][0]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "empty schema procedures")
        .unwrap()["result"]["sets"][0]["columns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    for restart in 0..2 {
        let server = Server::open(path.to_str().unwrap()).unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        if restart == 0 {
            assert!(
                session
                    .batch_response(
                        "CREATE DATABASE catalog_isolated",
                        &Default::default(),
                        false,
                        None
                    )
                    .1
            );
        }
        for database in ["master", "catalog_isolated"] {
            session.use_database(database).unwrap();
            let mut statement = session
                .db
                .prepare("SELECT * FROM sys.procedures LIMIT 0")
                .unwrap();
            {
                let mut rows = statement.query([]).unwrap();
                assert!(rows.next().unwrap().is_none());
            }
            let columns = statement.column_names();
            assert_eq!(columns, expected, "{database}");
            drop(statement);
            if restart == 0 && database == "catalog_isolated" {
                assert!(session.batch_response(
                    "CREATE TABLE dbo.catalog_defaults(state BIT CONSTRAINT DF_catalog_defaults_state DEFAULT(1))",
                    &Default::default(), false, None
                ).1);
                session.db.execute(
                    "INSERT INTO main.__msduck_modules
                     (object_id,schema_id,name,type_code,definition,create_date,modify_date)
                     VALUES (CAST(nextval('main.__msduck_object_ids') AS INTEGER),1,?,'P',?,current_timestamp,current_timestamp)",
                    ["catalog_p", "CREATE PROCEDURE dbo.catalog_p AS SELECT 1"],
                ).unwrap();
            }
            let count: i64 = session
                .db
                .query_row(
                    "SELECT count(*) FROM sys.procedures WHERE name='catalog_p'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(count, i64::from(database == "catalog_isolated"));
            if database == "catalog_isolated" {
                let definition: String = session.db.query_row(
                    "SELECT main.__msduck_object_definition(object_id) FROM sys.procedures WHERE name='catalog_p'", [], |r| r.get(0)
                ).unwrap();
                assert_eq!(definition, "CREATE PROCEDURE dbo.catalog_p AS SELECT 1");
                let default: (i32, String, bool) = session.db.query_row(
                    "SELECT parent_column_id,definition,is_system_named FROM sys.default_constraints WHERE name='DF_catalog_defaults_state'",
                    [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                ).unwrap();
                let captured = reference["runs"][0]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|r| r["name"] == "defaults")
                    .unwrap()["result"]["sets"][0]["rows"][0][3]
                    .as_str()
                    .unwrap();
                assert_eq!(default, (1, captured.to_owned(), false));
                let definition: String = session.db.query_row(
                    "SELECT main.__msduck_object_definition(object_id) FROM sys.default_constraints WHERE name='DF_catalog_defaults_state'", [], |r| r.get(0)
                ).unwrap();
                assert_eq!(definition, captured);
            } else {
                let count: i64 = session.db.query_row("SELECT count(*) FROM sys.default_constraints WHERE name='DF_catalog_defaults_state'", [], |r| r.get(0)).unwrap();
                assert_eq!(count, 0);
            }
        }
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn default_catalog_creation_and_cleanup_share_the_ddl_transaction() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    let run = |session: &mut Session, sql: &str| {
        session
            .batch_response(sql, &Default::default(), false, None)
            .1
    };
    assert!(run(
        &mut session,
        "CREATE TABLE dbo.defaults_one(id INT CONSTRAINT DF_shared DEFAULT(1))"
    ));
    assert!(!run(
        &mut session,
        "CREATE TABLE dbo.defaults_failed(id INT CONSTRAINT DF_shared DEFAULT(1))"
    ));
    let failed: Option<i32> = session
        .db
        .query_row(
            "SELECT main.__msduck_object_id('dbo.defaults_failed','U')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(failed, None);
    assert!(run(
        &mut session,
        "BEGIN TRANSACTION; CREATE TABLE dbo.defaults_rolled_back(id INT CONSTRAINT DF_rolled_back DEFAULT(1)); ROLLBACK"
    ));
    assert_eq!(
        session
            .db
            .query_row(
                "SELECT count(*) FROM sys.default_constraints WHERE name='DF_rolled_back'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    let id: i32 = session
        .db
        .query_row(
            "SELECT object_id FROM sys.default_constraints WHERE name='DF_shared'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(run(&mut session, "DROP TABLE dbo.defaults_one"));
    let definition: Option<String> = session
        .db
        .query_row("SELECT main.__msduck_object_definition(?)", [id], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(definition, None);
    assert_eq!(
        session
            .db
            .query_row(
                "SELECT count(*) FROM sys.default_constraints WHERE name='DF_shared'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}
