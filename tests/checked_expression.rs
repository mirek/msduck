use msduck::{engine::Session, server::Server};
#[test]
fn nested_arithmetic_condition_keeps_prior_writes_and_can_commit() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    let (tokens,ok)=session.batch_response("CREATE TABLE dbo.scalar_recovery(i INT); DECLARE @n BIGINT=7; DECLARE @d INT=0; BEGIN TRAN; INSERT dbo.scalar_recovery VALUES(1); BEGIN TRY IF ((@n+1)/@d)%2>=0 INSERT dbo.scalar_recovery VALUES(99); END TRY BEGIN CATCH IF ERROR_NUMBER()<>8134 THROW 51000,'wrong error',1; IF XACT_STATE()<>1 THROW 51001,'unusable transaction',1; END CATCH; INSERT dbo.scalar_recovery VALUES(2); COMMIT",&Default::default(),false,None);
    assert!(ok, "{tokens:?}");
    assert_eq!(session.transactions, 0);
    let rows = session
        .db
        .prepare("SELECT i FROM dbo.scalar_recovery ORDER BY i")
        .unwrap()
        .query_map([], |r| r.get::<_, i32>(0))
        .unwrap()
        .collect::<duckdb::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(rows, vec![1, 2]);
}
#[test]
fn overflow_is_not_hidden_by_null_predicate_and_scalar_assignment_recovers() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    let (tokens,ok)=session.batch_response("BEGIN TRAN; BEGIN TRY IF (2147483647+1) IS NULL THROW 51000,'overflow became NULL',1; END TRY BEGIN CATCH IF ERROR_NUMBER()<>8115 THROW 51001,'wrong overflow',1; END CATCH; DECLARE @n INT=9; BEGIN TRY SET @n=7/0; END TRY BEGIN CATCH IF ERROR_NUMBER()<>8134 THROW 51002,'wrong division',1; END CATCH; IF @n<>9 THROW 51003,'assignment changed',1; COMMIT",&Default::default(),false,None);
    assert!(ok, "{tokens:?}");
}
#[test]
fn successful_checked_conditions_retain_width_and_null_semantics() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    let (tokens,ok)=session.batch_response("DECLARE @b BIGINT=2147483647; DECLARE @expected BIGINT=2147483648; IF @b+1<>@expected THROW 51000,'narrowed result',1; IF (NULL/0) IS NOT NULL THROW 51001,'NULL became error',1; IF -(-7%3)<>1 THROW 51002,'wrong remainder',1; DECLARE @n BIGINT=CAST(2147483647+0 AS BIGINT)+1; IF @n<>@expected THROW 51003,'wrong assigned width',1",&Default::default(),false,None);
    assert!(ok, "{tokens:?}");
}

#[test]
fn literal_null_comparisons_suppress_faults_but_null_parameters_do_not() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    let (tokens,ok)=session.batch_response("DECLARE @d INT=0; DECLARE @missing INT=NULL; DECLARE @caught INT=0; BEGIN TRAN; BEGIN TRY IF NULL=(1/@d) THROW 51000,'selected NULL comparison',1; IF (1/@d)=NULL THROW 51001,'selected NULL comparison',1; END TRY BEGIN CATCH THROW 51002,'literal NULL did not suppress fault',1; END CATCH; BEGIN TRY IF @missing=(1/@d) THROW 51003,'selected parameter comparison',1; END TRY BEGIN CATCH IF ERROR_NUMBER()<>8134 THROW 51004,'wrong parameter comparison error',1; SET @caught=1; END CATCH; IF @caught<>1 THROW 51005,'parameter suppressed fault',1; COMMIT",&Default::default(),false,None);
    assert!(ok, "{tokens:?}");
    assert_eq!(session.transactions, 0);
}
