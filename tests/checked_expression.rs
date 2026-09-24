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

#[test]
fn correlated_expression_plans_keep_per_row_diagnostics_and_single_source_evaluation() {
    use msduck_core::catalog::TypeMetadata;
    use msduck_sql::binding_scope::{Field, Scope, Source};
    use msduck_sql::checked_expression::plan_in_scope;
    use sqlparser::parser::Parser;

    let expression = Parser::new(&msduck_sql::dialect::ServerDialect)
        .try_with_sql("((source.n+1)/source.d)%2")
        .unwrap()
        .parse_expr()
        .unwrap();
    let scope = Scope {
        rows: vec![Some(vec![Source {
            qualifiers: vec!["source".into()],
            fields: [("n", 127), ("d", 56)]
                .into_iter()
                .map(|(name, system_type_id)| Field {
                    name: name.into(),
                    info: Some(TypeMetadata {
                        system_type_id: Some(system_type_id),
                        ..Default::default()
                    }),
                    collation: None,
                    json_fragment: false,
                    properties: Default::default(),
                })
                .collect(),
        }])],
        ..Default::default()
    };
    let plan = plan_in_scope(&expression, &scope).unwrap();
    let server = Server::open(":memory:").unwrap();
    let db = server.connection().unwrap();
    db.execute_batch("CREATE SEQUENCE checked_source START 1; CREATE TABLE checked_prior(i INTEGER); BEGIN; INSERT INTO checked_prior VALUES(7)").unwrap();
    let sql = format!(
        "WITH source AS MATERIALIZED (
           SELECT i, nextval('checked_source') AS occurrence,
             CAST(CASE i%4 WHEN 1 THEN NULL WHEN 3 THEN 9223372036854775807 ELSE 7 END AS BIGINT) AS n,
             CAST(CASE i%4 WHEN 1 THEN 0 WHEN 2 THEN 0 ELSE 2 END AS INTEGER) AS d
           FROM range(6000) t(i)
         )
         SELECT source.i,source.occurrence,p.value,p.error_number,p.error_state,p.error_severity,p.error_message
         FROM source CROSS JOIN LATERAL ({}) p ORDER BY source.i",
        plan.query
    );
    let mut statement = db.prepare(&sql).unwrap();
    let mut rows = statement.query([]).unwrap();
    let mut count = 0;
    while let Some(row) = rows.next().unwrap() {
        let i: i64 = row.get(0).unwrap();
        let occurrence: i64 = row.get(1).unwrap();
        let value: Option<i64> = row.get(2).unwrap();
        let number: Option<i32> = row.get(3).unwrap();
        let state: Option<u8> = row.get(4).unwrap();
        let severity: Option<u8> = row.get(5).unwrap();
        let message: Option<String> = row.get(6).unwrap();
        assert_eq!(i, count);
        assert_eq!(occurrence, i + 1);
        match i % 4 {
            0 => assert_eq!(
                (value, number, state, severity, message),
                (Some(0), None, None, None, None)
            ),
            1 => assert_eq!(
                (value, number, state, severity, message),
                (None, None, None, None, None)
            ),
            2 => assert_eq!(
                (value, number, state, severity, message.as_deref()),
                (
                    None,
                    Some(8134),
                    Some(1),
                    Some(16),
                    Some("Divide by zero error encountered.")
                )
            ),
            _ => assert_eq!(
                (value, number, state, severity, message.as_deref()),
                (
                    None,
                    Some(8115),
                    Some(2),
                    Some(16),
                    Some("Arithmetic overflow error converting expression to data type bigint.")
                )
            ),
        }
        count += 1;
    }
    assert_eq!(count, 6000);
    drop(rows);
    drop(statement);
    let mut empty = db.prepare(&sql.replace("range(6000)", "range(0)")).unwrap();
    let mut batches = empty.query_arrow([]).unwrap();
    assert_eq!(
        batches.get_schema().field(2).data_type(),
        &duckdb::arrow::datatypes::DataType::Int64
    );
    assert!(batches.next().is_none());
    drop(batches);
    drop(empty);
    assert_eq!(
        db.query_row("SELECT currval('checked_source')", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        6000
    );
    db.execute_batch("INSERT INTO checked_prior VALUES(8); COMMIT")
        .unwrap();
    assert_eq!(
        db.query_row("SELECT SUM(i)::INTEGER FROM checked_prior", [], |r| r
            .get::<_, i32>(0))
            .unwrap(),
        15
    );
}
