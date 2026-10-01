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
            }
        }
    }
    std::fs::remove_dir_all(directory).unwrap();
}
