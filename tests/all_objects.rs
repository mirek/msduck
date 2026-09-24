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
