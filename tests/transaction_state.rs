use msduck::{engine::Session, server::Server};

#[test]
fn caught_truncation_dooms_transaction_and_rejects_commit_and_writes() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    let (tokens, ok) = session.batch_response(
        "CREATE TABLE dbo.doomed(i INT,s VARCHAR(1)); SET XACT_ABORT ON;
         BEGIN TRAN; INSERT dbo.doomed VALUES(1,'a');
         BEGIN TRY INSERT dbo.doomed VALUES(2,'long'); END TRY
         BEGIN CATCH
           IF XACT_STATE()<>-1 THROW 51000,'expected doomed state',1;
           BEGIN TRY COMMIT; END TRY BEGIN CATCH
             IF ERROR_NUMBER()<>3930 THROW 51001,'expected commit rejection',1;
           END CATCH;
           BEGIN TRY INSERT dbo.doomed VALUES(3,'b'); END TRY BEGIN CATCH
             IF ERROR_NUMBER()<>3930 THROW 51002,'expected write rejection',1;
           END CATCH;
           IF @@TRANCOUNT<>1 THROW 51003,'transaction disappeared',1;
           ROLLBACK;
         END CATCH;
         IF XACT_STATE()<>0 THROW 51004,'rollback did not clear state',1",
        &Default::default(),
        false,
        None,
    );
    assert!(ok, "{tokens:?}");
    assert_eq!(session.transactions, 0);
    assert_eq!(
        session
            .db
            .query_row("SELECT count(*) FROM dbo.doomed", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn doomed_batch_end_rolls_back_and_next_transaction_can_commit() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    let (_, ok) = session.batch_response(
        "CREATE TABLE dbo.batch_doom(i INT,s VARCHAR(1)); SET XACT_ABORT ON;
         BEGIN TRAN; INSERT dbo.batch_doom VALUES(1,'a');
         BEGIN TRY INSERT dbo.batch_doom VALUES(2,'long'); END TRY
         BEGIN CATCH SELECT XACT_STATE(); END CATCH",
        &Default::default(),
        false,
        None,
    );
    assert!(!ok);
    assert_eq!(session.last_error, 3998);
    assert_eq!(session.transactions, 0);
    let (tokens, ok) = session.batch_response(
        "BEGIN TRAN; INSERT dbo.batch_doom VALUES(3,'z'); COMMIT",
        &Default::default(),
        false,
        None,
    );
    assert!(ok, "{tokens:?}");
    assert_eq!(
        session
            .db
            .query_row("SELECT i FROM dbo.batch_doom", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        3
    );
}
