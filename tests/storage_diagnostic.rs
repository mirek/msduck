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

#[test]
fn validation_data_keeps_prior_writes_and_transaction_committable() {
    let server = Server::open(":memory:").unwrap();
    let session = Session::new(server.connection().unwrap()).unwrap();
    session.db.execute_batch("CREATE TABLE transaction_probe(i INTEGER); BEGIN TRANSACTION; INSERT INTO transaction_probe VALUES(1)").unwrap();
    let rows:Vec<(Option<Vec<u8>>,Option<String>)>=session.db.prepare("SELECT v.value,v.error FROM (SELECT __msduck_check_store_nchar(__msduck_pack_unicode(s),1,'master.dbo.t','s') v FROM (VALUES ('a'),('ab'),(NULL)) t(s))").unwrap().query_map([],|r|Ok((r.get(0)?,r.get(1)?))).unwrap().collect::<duckdb::Result<_>>().unwrap();
    assert_eq!(rows[0], (Some(vec![97, 0]), None));
    assert_eq!(rows[1].0, None);
    assert!(
        rows[1]
            .1
            .as_ref()
            .unwrap()
            .starts_with("__msduck_truncated_utf16:")
    );
    assert_eq!(rows[2], (None, None));
    session
        .db
        .execute_batch("INSERT INTO transaction_probe VALUES(2); COMMIT")
        .unwrap();
    assert_eq!(
        session
            .db
            .query_row("SELECT count(*) FROM transaction_probe", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
}

#[test]
fn validation_materialization_does_not_repeat_volatile_sources() {
    let server = Server::open(":memory:").unwrap();
    let session = Session::new(server.connection().unwrap()).unwrap();
    session.db.execute_batch("CREATE SEQUENCE staged_store_calls; BEGIN TRANSACTION; CREATE TEMP TABLE staged_store AS SELECT __msduck_check_store_varchar(__msduck_pack_unicode(CASE WHEN nextval('staged_store_calls')%2=0 THEN 'ab' ELSE 'Ā' END),1,'master.dbo.t','s') v FROM range(6000)").unwrap();
    let (good, bad): (i64, i64) = session
        .db
        .query_row(
            "SELECT count(v.value),count(v.error) FROM staged_store",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((good, bad), (3000, 3000));
    assert_eq!(
        session
            .db
            .query_row("SELECT currval('staged_store_calls')", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        6000
    );
    session
        .db
        .execute_batch("DROP TABLE staged_store; COMMIT")
        .unwrap();
}

#[test]
fn public_explicit_transaction_survives_rejected_insert_and_retains_prior_work() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    assert!(session.batch_response("CREATE TABLE dbo.transaction_text(i INT,s VARCHAR(1)); BEGIN TRAN; INSERT dbo.transaction_text VALUES(1,'a')",&Default::default(),false,None).1);
    assert!(!session.batch_response("INSERT dbo.transaction_text VALUES(2,'b'),(3,'long'); INSERT dbo.transaction_text VALUES(4,'c')",&Default::default(),false,None).1);
    assert_eq!(session.transactions, 1);
    assert!(
        session
            .batch_response("COMMIT", &Default::default(), false, None)
            .1
    );
    let rows: Vec<i32> = session
        .db
        .prepare("SELECT i FROM dbo.transaction_text ORDER BY i")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<duckdb::Result<_>>()
        .unwrap();
    assert_eq!(rows, vec![1, 4]);
    assert_eq!(session.db.query_row("SELECT count(*) FROM duckdb_tables() WHERE database_name='temp' AND table_name LIKE '__msduck_checked_insert_%'",[],|r|r.get::<_,i64>(0)).unwrap(),0);
}

#[test]
fn staged_insert_binds_parameters_and_preserves_unicode_and_null_storage() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    let sql = "CREATE TABLE dbo.staged_parameters(a VARCHAR(1),u NVARCHAR(3)); DECLARE @a NVARCHAR(1)=N'Ā'; DECLARE @u NVARCHAR(3)=N'🦆'; BEGIN TRAN; INSERT dbo.staged_parameters VALUES(@a,@u),(NULL,NULL); COMMIT";
    let (tokens, ok) = session.batch_response(sql, &Default::default(), false, None);
    assert!(ok, "{tokens:?}");
    let rows: Vec<(Option<String>, Option<Vec<u8>>)> = session
        .db
        .prepare("SELECT a,u.__msduck_utf16le FROM dbo.staged_parameters ORDER BY a NULLS LAST")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<duckdb::Result<_>>()
        .unwrap();
    assert_eq!(
        rows,
        vec![
            (Some("A".into()), Some(vec![0x3e, 0xd8, 0x86, 0xdd])),
            (None, None)
        ]
    );
}

#[test]
fn rejected_update_preserves_the_transaction_and_all_original_rows() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    assert!(session.batch_response("CREATE TABLE dbo.update_tx(i INT,s VARCHAR(1),n INT); INSERT dbo.update_tx VALUES(1,'a',10),(2,'b',20); BEGIN TRAN; INSERT dbo.update_tx VALUES(3,'c',30)",&Default::default(),false,None).1);
    assert!(
        !session
            .batch_response(
                "UPDATE dbo.update_tx SET s=CASE WHEN i=2 THEN 'long' ELSE 'x' END,n=n+1 WHERE i<3",
                &Default::default(),
                false,
                None
            )
            .1
    );
    assert_eq!(session.transactions, 1);
    let (tokens,ok)=session.batch_response("DECLARE @id INT=1; DECLARE @s NVARCHAR(1)=N'Ā'; UPDATE dbo.update_tx SET s=@s,n=n+1 WHERE i=@id; COMMIT",&Default::default(),false,None);
    assert!(ok, "{tokens:?}");
    let rows: Vec<(i32, String, i32)> = session
        .db
        .prepare("SELECT i,s,n FROM dbo.update_tx ORDER BY i")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<duckdb::Result<_>>()
        .unwrap();
    assert_eq!(
        rows,
        vec![
            (1, "A".into(), 11),
            (2, "b".into(), 20),
            (3, "c".into(), 30)
        ]
    );
    assert_eq!(session.db.query_row("SELECT count(*) FROM duckdb_tables() WHERE database_name='temp' AND table_name LIKE '__msduck_checked_insert_%'",[],|r|r.get::<_,i64>(0)).unwrap(),0);
}

#[test]
fn staged_update_materializes_volatile_assignments_once() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    assert!(session.batch_response("CREATE TABLE dbo.update_once(i INT,s VARCHAR(1)); INSERT dbo.update_once VALUES(0,'a'),(0,'b'),(0,'c')",&Default::default(),false,None).1);
    session
        .db
        .execute_batch("CREATE SEQUENCE update_once_calls START 1")
        .unwrap();
    let (tokens, ok) = session.batch_response(
        "BEGIN TRAN; UPDATE dbo.update_once SET i=nextval('update_once_calls'),s='z'; COMMIT",
        &Default::default(),
        false,
        None,
    );
    assert!(ok, "{tokens:?}");
    assert_eq!(
        session
            .db
            .query_row("SELECT currval('update_once_calls')", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        3
    );
    let rows: Vec<i32> = session
        .db
        .prepare("SELECT i FROM dbo.update_once ORDER BY i")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<duckdb::Result<_>>()
        .unwrap();
    assert_eq!(rows, vec![1, 2, 3]);
}

#[test]
fn user_rowid_columns_do_not_become_physical_update_identifiers() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    assert!(session.batch_response("CREATE TABLE dbo.shadow_rowid(rowid INT,n INT,s VARCHAR(1)); INSERT dbo.shadow_rowid VALUES(5,0,'a'),(5,0,'b')",&Default::default(),false,None).1);
    session
        .db
        .execute_batch("CREATE SEQUENCE shadow_rowid_calls START 1")
        .unwrap();
    let (tokens,ok)=session.batch_response("BEGIN TRAN; UPDATE dbo.shadow_rowid SET n=nextval('shadow_rowid_calls'),s='z' WHERE rowid=5; COMMIT",&Default::default(),false,None);
    assert!(ok, "{tokens:?}");
    let rows: Vec<i32> = session
        .db
        .prepare("SELECT n FROM dbo.shadow_rowid ORDER BY n")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<duckdb::Result<_>>()
        .unwrap();
    assert_eq!(rows, vec![1, 2]);
}
