use msduck::{engine::Session, server::Server};

fn count(db: &duckdb::Connection, sql: &str) -> i64 {
    db.query_row(sql, [], |row| row.get(0)).unwrap()
}

#[test]
fn built_in_membership_is_a_disjoint_catalog_union() {
    let server = Server::open(":memory:").unwrap();
    let db = server.connection().unwrap();
    assert_eq!(count(&db, "SELECT count(*) FROM sys.objects"), 118);
    assert_eq!(count(&db, "SELECT count(*) FROM sys.system_objects"), 2624);
    assert_eq!(count(&db, "SELECT count(*) FROM sys.all_objects"), 2742);
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM sys.objects o JOIN sys.system_objects s USING(object_id)"
        ),
        0
    );
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM ((SELECT object_id FROM sys.all_objects EXCEPT SELECT object_id FROM sys.objects EXCEPT SELECT object_id FROM sys.system_objects))"
        ),
        0
    );
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM sys.all_objects WHERE parent_object_id <> 0 AND parent_object_id NOT IN (SELECT object_id FROM sys.all_objects)"
        ),
        0
    );
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM sys.system_objects WHERE object_id < 0 AND is_ms_shipped"
        ),
        2624
    );
    let help_id: i32 = db
        .query_row("SELECT __msduck_object_id('sys.sp_help','P')", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(help_id, -784136858);
    let help_name: String = db
        .query_row("SELECT __msduck_object_name(?)", [help_id], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(help_name, "sp_help");
}

#[test]
fn user_objects_join_the_union_transactionally() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    let run = |session: &mut Session, sql| {
        assert!(
            session
                .batch_response(sql, &Default::default(), false, None)
                .1,
            "{sql}"
        );
    };
    run(
        &mut session,
        "BEGIN TRAN; CREATE TABLE dbo.catalog_test(id INT)",
    );
    let id: i32 = session
        .db
        .query_row(
            "SELECT object_id FROM sys.all_objects WHERE name='catalog_test'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        count(
            &session.db,
            "SELECT count(*) FROM sys.objects WHERE name='catalog_test'"
        ),
        1
    );
    assert_eq!(
        count(
            &session.db,
            "SELECT count(*) FROM sys.system_objects WHERE name='catalog_test'"
        ),
        0
    );
    run(&mut session, "ROLLBACK");
    assert_eq!(
        count(
            &session.db,
            "SELECT count(*) FROM sys.all_objects WHERE name='catalog_test'"
        ),
        0
    );
    run(&mut session, "CREATE TABLE dbo.catalog_test(id INT)");
    let new_id: i32 = session
        .db
        .query_row(
            "SELECT object_id FROM sys.all_objects WHERE name='catalog_test'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_ne!(id, new_id);
    assert_eq!(
        count(&session.db, "SELECT count(*) FROM sys.all_objects"),
        2743
    );
}

#[test]
fn built_in_and_user_catalog_ids_survive_reopen() {
    let path = std::env::temp_dir().join(format!(
        "msduck-all-objects-{}-{}.duckdb",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    let (user_id, builtin_clock) = {
        let server = Server::open(path.to_str().unwrap()).unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        assert!(
            session
                .batch_response(
                    "CREATE TABLE dbo.catalog_persist(id INT)",
                    &Default::default(),
                    false,
                    None,
                )
                .1
        );
        let id = session
            .db
            .query_row(
                "SELECT object_id FROM sys.all_objects WHERE name='catalog_persist'",
                [],
                |row| row.get::<_, i32>(0),
            )
            .unwrap();
        let clock = session
            .db
            .query_row(
                "SELECT CAST(create_date AS VARCHAR) FROM sys.objects WHERE name='wpr_bucket_table'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap();
        (id, clock)
    };
    let server = Server::open(path.to_str().unwrap()).unwrap();
    let db = server.connection().unwrap();
    assert_eq!(count(&db, "SELECT count(*) FROM sys.all_objects"), 2743);
    assert_eq!(
        db.query_row(
            "SELECT object_id FROM sys.all_objects WHERE name='catalog_persist'",
            [],
            |row| row.get::<_, i32>(0),
        )
        .unwrap(),
        user_id
    );
    assert_eq!(
        db.query_row(
            "SELECT CAST(create_date AS VARCHAR) FROM sys.objects WHERE name='wpr_bucket_table'",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap(),
        builtin_clock
    );
    drop(db);
    drop(server);
    std::fs::remove_file(path).unwrap();
}
