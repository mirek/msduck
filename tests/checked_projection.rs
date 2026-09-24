use msduck::{engine::Session, server::Server};
#[test]
fn caught_scalar_and_table_projection_faults_preserve_prior_and_later_writes() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    let (tokens,ok)=session.batch_response("CREATE TABLE dbo.checked_p(i INT,n BIGINT,d INT); INSERT dbo.checked_p VALUES(1,8,2),(2,8,0); CREATE TABLE dbo.checked_w(i INT); DECLARE @caught INT=0; BEGIN TRAN; INSERT dbo.checked_w VALUES(1); BEGIN TRY SELECT 42 AS a,1/0 AS b; END TRY BEGIN CATCH IF ERROR_NUMBER()<>8134 THROW 51000,'wrong scalar error',1; SET @caught=@caught+1; END CATCH; BEGIN TRY SELECT i,n/d AS result FROM dbo.checked_p; END TRY BEGIN CATCH IF ERROR_NUMBER()<>8134 THROW 51001,'wrong row error',1; SET @caught=@caught+1; END CATCH; IF @caught<>2 THROW 51002,'missing failure',1; INSERT dbo.checked_w VALUES(2); COMMIT",&Default::default(),false,None);
    assert!(ok, "{tokens:?}");
    assert_eq!(session.transactions, 0);
    assert_eq!(
        session
            .db
            .query_row("SELECT SUM(i)::INTEGER FROM dbo.checked_w", [], |r| r
                .get::<_, i32>(0))
            .unwrap(),
        3
    );
}

#[test]
fn multiple_correlated_scalar_plans_return_native_outcomes() {
    use sqlparser::{ast::Statement, parser::Parser};
    let server = Server::open(":memory:").unwrap();
    let db = server.connection().unwrap();
    for sql in [
        "SELECT 42 AS a,1/0 AS b,7 AS c",
        "SELECT NULL/0 AS n,CAST(NULL AS BIGINT)+1 AS b",
    ] {
        let Statement::Query(q) = Parser::parse_sql(&msduck_sql::dialect::ServerDialect, sql)
            .unwrap()
            .remove(0)
        else {
            panic!()
        };
        let p = msduck_sql::checked_projection::plan(&q, &Default::default()).unwrap();
        let mut statement = db.prepare(&p.query.to_string()).unwrap();
        let mut rows = statement.query([]).unwrap();
        assert!(rows.next().unwrap().is_some(), "{sql}");
    }
}

#[test]
fn multi_chunk_public_projection_keeps_exact_bigint_tokens_and_empty_metadata() {
    use msduck::tds::{self, Column, Type};
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    assert!(
        session
            .batch_response(
                "CREATE TABLE dbo.projection_chunks(i INT,n BIGINT,d INT)",
                &Default::default(),
                false,
                None
            )
            .1
    );
    session.db.execute_batch("INSERT INTO dbo.projection_chunks SELECT i::INTEGER,(i*2)::BIGINT,2 FROM range(6000) t(i)").unwrap();
    let columns = [Column {
        name: "answer".into(),
        kind: Type::Int(8),
        properties: msduck_core::result::Properties::expression(true),
        collation: None,
    }];
    for empty in [false, true] {
        let filter = if empty { "WHERE i<0" } else { "" };
        let sql =
            format!("SELECT (n+2)/d AS answer FROM dbo.projection_chunks {filter} ORDER BY i");
        let (actual, ok) = session.batch_response(&sql, &Default::default(), false, None);
        assert!(ok);
        let mut expected = Vec::new();
        tds::metadata(&mut expected, &columns).unwrap();
        let count = if empty { 0 } else { 6000 };
        for i in 1..=count {
            expected.push(0xd1);
            expected.push(8);
            expected.extend_from_slice(&(i as i64).to_le_bytes());
        }
        tds::done(&mut expected, 0xfd, 16, 0xc1, count);
        assert_eq!(actual, expected, "empty={empty}");
    }
}
