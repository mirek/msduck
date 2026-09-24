use msduck_sql::{batch, drop_index, drop_index_syntax};
use sqlparser::ast::{Statement, Visit, Visitor};
use std::ops::ControlFlow;

#[test]
fn every_captured_request_survives_batch_parsing_or_retains_its_syntax_error() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../../reference/drop-index.json")).unwrap();
    for case in fixture["results"].as_array().unwrap() {
        let sql = case["sql"].as_str().unwrap();
        match drop_index::parse(sql) {
            Ok(expected) => {
                let actual = batch::parse(sql).unwrap();
                assert_eq!(actual.len(), 1, "{sql}");
                assert_eq!(
                    drop_index_syntax::request(&actual[0]),
                    Some(expected),
                    "{sql}"
                );
            }
            Err(drop_index::Error::Sql(expected)) => {
                let actual = batch::parse(sql).unwrap_err();
                let actual = actual
                    .downcast_ref::<msduck_core::diagnostic::SqlError>()
                    .unwrap();
                assert_eq!(
                    (
                        actual.number,
                        actual.state,
                        actual.severity,
                        &actual.message
                    ),
                    (
                        expected.number,
                        expected.state,
                        expected.class,
                        &expected.message
                    )
                );
            }
            Err(error) => panic!("unhandled reference request {sql}: {error:?}"),
        }
    }
}

#[test]
fn nested_and_adjacent_drops_preserve_all_targets_and_syntax_errors() {
    struct Requests(Vec<drop_index::Request>);
    impl Visitor for Requests {
        type Break = ();
        fn pre_visit_statement(&mut self, statement: &Statement) -> ControlFlow<()> {
            if let Some(request) = drop_index_syntax::request(statement) {
                self.0.push(request);
            }
            ControlFlow::Continue(())
        }
    }
    let drop = "DROP INDEX IF EXISTS [odd.index] ON [alt].[odd.table] WITH(ONLINE=OFF), dbo.a.ix";
    for sql in [
        format!("{drop}; SELECT 7"),
        format!("IF 1=1 BEGIN {drop} SELECT 7 END"),
        format!("BEGIN TRY {drop} END TRY BEGIN CATCH SELECT 7 END CATCH"),
    ] {
        let statements = batch::parse(&sql).unwrap();
        let mut requests = Requests(vec![]);
        let _ = statements.visit(&mut requests);
        assert_eq!(requests.0, vec![drop_index::parse(drop).unwrap()], "{sql}");
    }
    for sql in [
        "DROP INDEX ix",
        "IF 1=1 BEGIN DROP INDEX ix END",
        "BEGIN TRY DROP INDEX ix END TRY BEGIN CATCH SELECT 1 END CATCH",
    ] {
        let error = batch::parse(sql).unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<msduck_core::diagnostic::SqlError>()
                .unwrap()
                .number,
            159,
            "{sql}"
        );
    }
    assert!(batch::parse("DROP INDEX ix ON dbo.a WITH(ONLINE=ON)").is_err());
    assert!(drop_index_syntax::request(&batch::parse("SELECT 1").unwrap()[0]).is_none());
}
