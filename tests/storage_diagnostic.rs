use msduck::{engine::Session, server::Server};
#[test]
fn rejected_character_writes_are_atomic_and_continue_the_batch() {
    for write in [
        "INSERT dbo.short_write VALUES(1,'y'),(2,'long')",
        "UPDATE dbo.short_write SET s='long'",
    ] {
        let server = Server::open(":memory:").unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        assert!(session.batch_response("CREATE TABLE dbo.short_write(id INT,s VARCHAR(1)); INSERT dbo.short_write VALUES(0,'x')",&Default::default(),false,None).1);
        let (_, ok) = session.batch_response(
            &format!("{write}; INSERT dbo.short_write VALUES(3,'z')"),
            &Default::default(),
            false,
            None,
        );
        assert!(!ok, "failed write must remain visible");
        let rows: Vec<(i32, String)> = session
            .db
            .prepare("SELECT id,s FROM dbo.short_write ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<duckdb::Result<_>>()
            .unwrap();
        assert_eq!(rows, vec![(0, "x".into()), (3, "z".into())], "{write}");
    }
}

#[test]
fn contextual_storage_evaluates_sources_once_across_vectors() {
    let server = Server::open(":memory:").unwrap();
    let session = Session::new(server.connection().unwrap()).unwrap();
    session
        .db
        .execute_batch("CREATE SEQUENCE contextual_store_calls")
        .unwrap();
    let wrong:i64=session.db.query_row("SELECT count(*) FROM (SELECT i,__msduck_store_context_varchar(__msduck_pack_unicode(CASE WHEN nextval('contextual_store_calls')%17=0 THEN NULL ELSE 'Ā' END),1,'master.dbo.t','s') v FROM range(6000) t(i)) WHERE v IS DISTINCT FROM CASE WHEN (i+1)%17=0 THEN NULL ELSE 'A' END",[],|r|r.get(0)).unwrap();
    assert_eq!(wrong, 0);
    assert_eq!(
        session
            .db
            .query_row("SELECT currval('contextual_store_calls')", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        6000
    );
}
