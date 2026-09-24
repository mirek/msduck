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
