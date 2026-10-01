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

#[test]
fn default_expression_families_match_sql_server_catalog_text() {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("../reference/gaps-catalog.json")).unwrap();
    let records = reference["definitionProfile"]["runs"][0]
        .as_array()
        .unwrap();
    let record = |name| records.iter().find(|r| r["name"] == name).unwrap();
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    assert!(
        session
            .batch_response(
                record("setup default definitions")["sql"].as_str().unwrap(),
                &Default::default(),
                false,
                None
            )
            .1
    );
    let object_id: i32 = session
        .db
        .query_row(
            "SELECT main.__msduck_object_id(?,'U')",
            ["dbo.definition_defaults"],
            |r| r.get(0),
        )
        .unwrap();
    let rows = session
        .db
        .prepare("SELECT name,parent_column_id,definition,is_system_named FROM sys.default_constraints WHERE parent_object_id=? ORDER BY parent_column_id")
        .unwrap()
        .query_map([object_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i32>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, bool>(3)?,
            ))
        })
        .unwrap()
        .collect::<duckdb::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        serde_json::to_value(rows).unwrap(),
        record("default definitions")["result"]["sets"][0]["rows"]
    );
    // Original source remains distinct from serialized definitions, including
    // volatile function declarations. This checks text, not default execution.
    let (source, definition): (String,String) = session.db.query_row(
        "SELECT source_expression,definition FROM main.__msduck_default_constraints WHERE name='DF_definition_clock'", [], |r| Ok((r.get(0)?,r.get(1)?))
    ).unwrap();
    assert!(source.eq_ignore_ascii_case("(GETDATE())"));
    assert_eq!(definition, "(getdate())");
}

#[test]
fn key_objects_have_distinct_stable_ids_and_source_name_provenance() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    assert!(session.batch_response(
        "CREATE TABLE dbo.catalog_parent(a INT NOT NULL,b INT NOT NULL,label NVARCHAR(40),CONSTRAINT PK_catalog_parent PRIMARY KEY(a,b),CONSTRAINT UQ_catalog_parent_label UNIQUE(label)); CREATE TABLE dbo.catalog_child(a INT NOT NULL,b INT NOT NULL,CONSTRAINT PK_catalog_child PRIMARY KEY(a,b))",
        &Default::default(),false,None
    ).1);
    let rows = session.db.prepare("SELECT name,rtrim(type),type_desc,main.__msduck_object_name(parent_object_id) FROM sys.objects WHERE rtrim(type) IN ('PK','UQ') ORDER BY name").unwrap()
        .query_map([],|r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?)))
        .unwrap().collect::<duckdb::Result<Vec<_>>>().unwrap();
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("../reference/gaps-catalog.json")).unwrap();
    let expected = reference["runs"][0]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "constraint objects")
        .unwrap()["result"]["sets"][0]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r[1] == "PK" || r[1] == "UQ")
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        serde_json::to_value(rows).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
    let ids = || {
        session.db.prepare("SELECT object_id,parent_object_id,is_system_named,is_enforced FROM sys.key_constraints ORDER BY object_id").unwrap()
        .query_map([],|r|Ok((r.get::<_,i32>(0)?,r.get::<_,i32>(1)?,r.get::<_,Option<bool>>(2)?,r.get::<_,bool>(3)?))).unwrap().collect::<duckdb::Result<Vec<_>>>().unwrap()
    };
    let original = ids();
    assert_eq!(original.len(), 3);
    assert!(
        original
            .iter()
            .all(|(id, parent, named, enforced)| id != parent
                && *named == Some(false)
                && *enforced)
    );
    let before: i64 = session
        .db
        .query_row("SELECT currval('main.__msduck_object_ids')", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(ids(), original);
    assert_eq!(
        session
            .db
            .query_row("SELECT currval('main.__msduck_object_ids')", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        before
    );
    assert!(
        session
            .batch_response(
                "CREATE TABLE dbo.generated_key(a INT PRIMARY KEY)",
                &Default::default(),
                false,
                None
            )
            .1
    );
    assert_eq!(session.db.query_row("SELECT is_system_named FROM sys.key_constraints WHERE parent_object_id=main.__msduck_object_id('dbo.generated_key','U')",[],|r|r.get::<_,Option<bool>>(0)).unwrap(),Some(true));
    let identity = |session: &Session| {
        session
            .db
            .query_row(
                "SELECT main.__msduck_object_id('dbo.PK_catalog_child','PK')",
                [],
                |r| r.get::<_, Option<i32>>(0),
            )
            .unwrap()
    };
    let old = identity(&session).unwrap();
    assert!(session.batch_response("BEGIN TRANSACTION; DROP TABLE dbo.catalog_child; CREATE TABLE dbo.catalog_child(a INT,b INT,CONSTRAINT PK_catalog_child PRIMARY KEY(a,b))", &Default::default(),false,None).1);
    assert_ne!(identity(&session), Some(old));
    assert!(
        session
            .batch_response("ROLLBACK", &Default::default(), false, None)
            .1
    );
    assert_eq!(identity(&session), Some(old));
    assert!(session.batch_response("DROP TABLE dbo.catalog_child; CREATE TABLE dbo.catalog_child(a INT,b INT,CONSTRAINT PK_catalog_child PRIMARY KEY(a,b))", &Default::default(),false,None).1);
    assert_ne!(identity(&session), Some(old));
}

#[test]
fn legacy_key_backfill_preserves_parent_and_retains_unknown_name_provenance() {
    let directory = std::env::temp_dir().join(format!(
        "msduck-legacy-key-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("primary.duckdb");
    let mut original_parent = None;
    let mut backfilled = None;
    for restart in 0..3 {
        let server = Server::open(path.to_str().unwrap()).unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        if restart == 0 {
            assert!(
                session
                    .batch_response(
                        "CREATE TABLE legacy_key(a INT CONSTRAINT PK_legacy PRIMARY KEY)",
                        &Default::default(),
                        false,
                        None
                    )
                    .1
            );
            original_parent = Some(
                session
                    .db
                    .query_row(
                        "SELECT main.__msduck_object_id('dbo.legacy_key','U')",
                        [],
                        |r| r.get::<_, i32>(0),
                    )
                    .unwrap(),
            );
            // Model the pre-catalog layout: native/keys records existed, but
            // no SQL Server constraint identity or name provenance was stored.
            session
                .db
                .execute("DELETE FROM main.__msduck_key_objects", [])
                .unwrap();
        } else {
            let row: (i32,i32,Option<bool>) = session.db.query_row("SELECT object_id,parent_object_id,is_system_named FROM sys.key_constraints WHERE name='PK_legacy'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
            assert_eq!(Some(row.1), original_parent);
            assert_eq!(row.2, None);
            if let Some(expected) = backfilled {
                assert_eq!(row, expected);
            } else {
                backfilled = Some(row);
            }
        }
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn key_namespace_failures_preserve_the_callers_transaction_and_writes() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    let run = |session: &mut Session, sql: &str| {
        session
            .batch_response(sql, &Default::default(), false, None)
            .1
    };
    assert!(run(
        &mut session,
        "CREATE TABLE kept(id INT); CREATE TABLE defaults(id INT CONSTRAINT DF_shared DEFAULT(1)); BEGIN TRANSACTION; INSERT kept VALUES(42)"
    ));
    for (table, sql) in [
        (
            "failed_existing",
            "CREATE TABLE failed_existing(id INT CONSTRAINT DF_shared PRIMARY KEY)",
        ),
        (
            "failed_dupe",
            "CREATE TABLE failed_dupe(a INT,b INT,CONSTRAINT repeated PRIMARY KEY(a),CONSTRAINT repeated UNIQUE(b))",
        ),
        (
            "failed_default",
            "CREATE TABLE failed_default(a INT CONSTRAINT repeated DEFAULT(1),CONSTRAINT repeated PRIMARY KEY(a))",
        ),
        (
            "failed_self",
            "CREATE TABLE failed_self(a INT CONSTRAINT failed_self PRIMARY KEY)",
        ),
    ] {
        assert!(!run(&mut session, sql), "{table}");
        assert_eq!(session.transactions, 1);
        assert_eq!(
            session
                .db
                .query_row("SELECT main.__msduck_object_id(?,'U')", [table], |r| r
                    .get::<_, Option<
                    i32,
                >>(
                    0
                ))
                .unwrap(),
            None
        );
    }
    assert!(run(&mut session, "COMMIT"));
    assert_eq!(
        session
            .db
            .query_row("SELECT id FROM dbo.kept", [], |r| r.get::<_, i32>(0))
            .unwrap(),
        42
    );
}

#[test]
fn generated_key_names_avoid_existing_schema_objects_in_callers_transaction() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    let run = |session: &mut Session, sql: &str| {
        session
            .batch_response(sql, &Default::default(), false, None)
            .1
    };
    assert!(run(&mut session, "CREATE TABLE dbo.kept(id INT)"));
    let last_object: i64 = session
        .db
        .query_row("SELECT currval('main.__msduck_object_ids')", [], |r| {
            r.get(0)
        })
        .unwrap();
    let consumed_tag: i64 = session
        .db
        .query_row("SELECT nextval('main.__msduck_key_tags')", [], |r| r.get(0))
        .unwrap();
    // The blocker and target table each consume one object identity. The
    // target's first constraint consumes the next key tag.
    let candidate = msduck_sql::dialect::ext::keys::table::generated_name(
        true,
        "generated_target",
        (((last_object + 2) as u64) << 20) ^ (consumed_tag + 1) as u64,
    );
    assert!(run(
        &mut session,
        &format!("CREATE TABLE dbo.[{candidate}](id INT)")
    ));
    assert!(run(
        &mut session,
        "BEGIN TRANSACTION; INSERT INTO dbo.kept VALUES(42)"
    ));
    assert!(run(
        &mut session,
        "CREATE TABLE dbo.generated_target(id INT PRIMARY KEY)"
    ));
    assert_eq!(session.transactions, 1);
    let actual: String = session.db.query_row(
        "SELECT k.name FROM sys.key_constraints k JOIN sys.tables t ON t.object_id=k.parent_object_id WHERE t.name='generated_target'", [], |r| r.get(0)
    ).unwrap();
    assert_ne!(actual, candidate);
    let count: i64 = session
        .db
        .query_row(
            "SELECT count(*) FROM sys.objects WHERE name=?",
            [&candidate],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
    assert!(run(&mut session, "ROLLBACK"));
    let retained: i64 = session
        .db
        .query_row("SELECT count(*) FROM dbo.kept", [], |r| r.get(0))
        .unwrap();
    assert_eq!(retained, 0);
    let target: Option<i32> = session
        .db
        .query_row(
            "SELECT main.__msduck_object_id('dbo.generated_target','U')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(target, None);
}
