#[path = "../src/index_catalog.rs"]
mod index_catalog;
use index_catalog::{Transaction, acquire, create, drop_index, register, sync};
use msduck::{engine::Session, server::Server};
use sqlparser::{ast::Statement, dialect::MsSqlDialect, parser::Parser};
fn setup() -> Session {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    let (response, ok) = session.batch_response(
        "CREATE TABLE dbo.a(id INT,value INT); CREATE TABLE dbo.b(id INT,value INT)",
        &Default::default(),
        false,
        None,
    );
    assert!(ok, "{response:?}");
    register(&session.db).unwrap();
    session
}
fn make(session: &Session, sql: &str) -> anyhow::Result<index_catalog::Index> {
    let Statement::CreateIndex(ast) = Parser::parse_sql(&MsSqlDialect {}, sql)?.remove(0) else {
        panic!()
    };
    let object_id = session.db.query_row(
        "SELECT __msduck_object_id(?,NULL)",
        [ast.table_name.to_string()],
        |r| r.get(0),
    )?;
    create(&session.db, object_id, &ast, Transaction::Owned)
}
#[test]
fn names_belong_to_tables_and_reused_ids_do_not_revive_bound_drops() {
    let s = setup();
    let a = make(&s, "CREATE INDEX ix ON dbo.a(id)").unwrap();
    let b = make(&s, "CREATE INDEX ix ON dbo.b(id)").unwrap();
    assert_ne!(a.backend_name, b.backend_name);
    assert_eq!(a.index_id, 2);
    assert_eq!(b.index_id, 2);
    drop_index(&s.db, &a, Transaction::Owned).unwrap();
    let replacement = make(&s, "CREATE INDEX ix ON dbo.a(value)").unwrap();
    assert_eq!(replacement.index_id, a.index_id);
    assert_ne!(replacement.incarnation, a.incarnation);
    assert!(drop_index(&s.db, &a, Transaction::Owned).is_err());
    let current = acquire(&s.db).unwrap();
    assert!(current.contains(&replacement));
    assert!(current.contains(&b));
}
#[test]
fn caller_rollback_restores_physical_and_logical_index_state() {
    let s = setup();
    let a = make(&s, "CREATE UNIQUE INDEX ix ON dbo.a(id)").unwrap();
    s.db.execute_batch("BEGIN TRANSACTION").unwrap();
    drop_index(&s.db, &a, Transaction::CallerOwned).unwrap();
    assert!(acquire(&s.db).unwrap().is_empty());
    s.db.execute_batch("ROLLBACK").unwrap();
    assert_eq!(acquire(&s.db).unwrap(), vec![a.clone()]);
    s.db.execute_batch("INSERT INTO dbo.a VALUES(1,1)").unwrap();
    assert!(s.db.execute_batch("INSERT INTO dbo.a VALUES(1,2)").is_err());
    let Statement::CreateIndex(ast) =
        Parser::parse_sql(&MsSqlDialect {}, "CREATE INDEX other ON dbo.b(value)")
            .unwrap()
            .remove(0)
    else {
        panic!()
    };
    let id: i32 =
        s.db.query_row("SELECT __msduck_object_id('dbo.b',NULL)", [], |r| r.get(0))
            .unwrap();
    s.db.execute_batch("BEGIN TRANSACTION").unwrap();
    create(&s.db, id, &ast, Transaction::CallerOwned).unwrap();
    s.db.execute_batch("ROLLBACK").unwrap();
    assert_eq!(acquire(&s.db).unwrap(), vec![a]);
}
#[test]
fn failed_create_and_recreated_tables_do_not_leave_live_catalog_records() {
    let mut s = setup();
    s.db.execute_batch("INSERT INTO dbo.a VALUES(1,1),(1,2)")
        .unwrap();
    assert!(make(&s, "CREATE UNIQUE INDEX bad ON dbo.a(id)").is_err());
    assert!(acquire(&s.db).unwrap().is_empty());
    assert!(make(&s, "CREATE INDEX unsupported ON dbo.a(id DESC)").is_err());
    let old = make(&s, "CREATE INDEX ix ON dbo.a(id)").unwrap();
    let (response, ok) = s.batch_response(
        "DROP TABLE dbo.a; CREATE TABLE dbo.a(id INT)",
        &Default::default(),
        false,
        None,
    );
    assert!(ok, "{response:?}");
    sync(&s.db).unwrap();
    assert!(acquire(&s.db).unwrap().is_empty());
    let new = make(&s, "CREATE INDEX ix ON dbo.a(id)").unwrap();
    assert_ne!(new.object_id, old.object_id);
    assert_ne!(new.backend_name, old.backend_name);
    assert!(drop_index(&s.db, &old, Transaction::Owned).is_err());
}

#[test]
fn unique_integer_keys_treat_null_as_one_key_distinct_from_zero() {
    let s = setup();
    make(&s, "CREATE UNIQUE INDEX ix ON dbo.a(id)").unwrap();
    s.db.execute_batch("INSERT INTO dbo.a VALUES(NULL,1),(0,2)")
        .unwrap();
    assert!(
        s.db.execute_batch("INSERT INTO dbo.a VALUES(NULL,3)")
            .is_err()
    );
    assert!(s.db.execute_batch("INSERT INTO dbo.a VALUES(0,4)").is_err());
    let count: i64 =
        s.db.query_row("SELECT count(*) FROM dbo.a", [], |r| r.get(0))
            .unwrap();
    assert_eq!(count, 2);
}

#[test]
fn persistent_logical_identity_survives_database_reopen() {
    let path = std::env::temp_dir().join(format!(
        "msduck-index-reopen-{}-{}.duckdb",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let expected;
    {
        let server = Server::open(path.to_str().unwrap()).unwrap();
        let mut s = Session::new(server.connection().unwrap()).unwrap();
        let (response, ok) = s.batch_response(
            "CREATE TABLE dbo.a(id INT)",
            &Default::default(),
            false,
            None,
        );
        assert!(ok, "{response:?}");
        register(&s.db).unwrap();
        expected = make(&s, "CREATE UNIQUE INDEX ix ON dbo.a(id)").unwrap();
    }
    {
        let server = Server::open(path.to_str().unwrap()).unwrap();
        let s = Session::new(server.connection().unwrap()).unwrap();
        register(&s.db).unwrap();
        assert_eq!(acquire(&s.db).unwrap(), vec![expected.clone()]);
        drop_index(&s.db, &expected, Transaction::Owned).unwrap();
        assert!(acquire(&s.db).unwrap().is_empty());
    }
    std::fs::remove_file(path).unwrap();
}

#[test]
fn unmanaged_indexes_migrate_atomically_before_complete_binding() {
    let s = setup();
    s.db.execute_batch(
        "CREATE INDEX legacy_a ON dbo.a(id); CREATE UNIQUE INDEX legacy_b ON dbo.b(id)",
    )
    .unwrap();
    assert!(index_catalog::acquire_complete(&s.db).is_err());
    index_catalog::reconcile(&s.db, Transaction::Owned).unwrap();
    let current = index_catalog::acquire_complete(&s.db).unwrap();
    assert_eq!(current.len(), 2);
    assert_eq!(
        current.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(),
        vec!["legacy_a", "legacy_b"]
    );
    assert!(
        current
            .iter()
            .all(|i| i.backend_name.starts_with("__msduck_index_"))
    );
    index_catalog::reconcile(&s.db, Transaction::Owned).unwrap();
    assert_eq!(index_catalog::acquire_complete(&s.db).unwrap(), current);
    s.db.execute_batch("INSERT INTO dbo.b VALUES(NULL,1)")
        .unwrap();
    assert!(
        s.db.execute_batch("INSERT INTO dbo.b VALUES(NULL,2)")
            .is_err()
    );
}
#[test]
fn incompatible_legacy_unique_data_and_constraints_remain_explicit() {
    let mut s = setup();
    s.db.execute_batch(
        "INSERT INTO dbo.a VALUES(NULL,1),(NULL,2); CREATE UNIQUE INDEX legacy ON dbo.a(id)",
    )
    .unwrap();
    assert!(index_catalog::reconcile(&s.db, Transaction::Owned).is_err());
    assert!(acquire(&s.db).unwrap().is_empty());
    let count: i64 =
        s.db.query_row(
            "SELECT count(*) FROM duckdb_indexes() WHERE index_name='legacy'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
    s.db.execute_batch("DROP INDEX dbo.legacy").unwrap();
    let (response, ok) = s.batch_response(
        "CREATE TABLE dbo.guarded(id INT PRIMARY KEY)",
        &Default::default(),
        false,
        None,
    );
    assert!(ok, "{response:?}");
    assert!(index_catalog::acquire_complete(&s.db).is_err());
}

fn catalog_rows(s: &Session, sql: &str) -> serde_json::Value {
    use duckdb::types::Value;
    let rows =
        s.db.prepare(sql)
            .unwrap()
            .query_map([], |r| {
                (0..r.as_ref().column_count())
                    .map(|i| {
                        let value: Value = r.get(i)?;
                        Ok(match value {
                            Value::Null => serde_json::Value::Null,
                            Value::Boolean(v) => serde_json::json!(v),
                            Value::TinyInt(v) => serde_json::json!(v),
                            Value::UTinyInt(v) => serde_json::json!(v),
                            Value::SmallInt(v) => serde_json::json!(v),
                            Value::Int(v) => serde_json::json!(v),
                            Value::BigInt(v) => serde_json::json!(v),
                            Value::Text(v) => serde_json::json!(v),
                            other => panic!("Unexpected catalog value {other:?}"),
                        })
                    })
                    .collect::<duckdb::Result<Vec<_>>>()
            })
            .unwrap()
            .collect::<duckdb::Result<Vec<_>>>()
            .unwrap();
    serde_json::json!(rows)
}
#[test]
fn supported_catalog_rows_match_captured_heap_index_and_column_snapshots() {
    let server = Server::open(":memory:").unwrap();
    let mut s = Session::new(server.connection().unwrap()).unwrap();
    register(&s.db).unwrap();
    index_catalog::publish_views(&s.db).unwrap();
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../reference/index-catalog.json")).unwrap();
    for case in fixture["results"].as_array().unwrap() {
        let id = case["id"].as_str().unwrap();
        if ![
            "schema",
            "heaps",
            "quoted-table",
            "ordinary",
            "same-name-other-table",
            "quoted-index",
        ]
        .contains(&id)
        {
            continue;
        }
        let sql = case["sql"].as_str().unwrap();
        if sql.starts_with("CREATE INDEX") || sql.starts_with("CREATE UNIQUE INDEX") {
            make(&s, sql).unwrap();
        } else {
            let (response, ok) = s.batch_response(sql, &Default::default(), false, None);
            assert!(ok, "{id}: {response:?}");
        }
        for (query, key) in [("indexSql", "indexes"), ("columnSql", "columns")] {
            assert_eq!(
                catalog_rows(&s, fixture[query].as_str().unwrap()),
                case[key]["sets"][0]["rows"],
                "{id}/{key}"
            );
        }
    }
    s.db.execute_batch("CREATE INDEX unmanaged ON dbo.a(value)")
        .unwrap();
    assert!(
        s.db.prepare("SELECT name FROM sys.indexes")
            .unwrap()
            .query([])
            .unwrap()
            .next()
            .is_err()
    );
}
