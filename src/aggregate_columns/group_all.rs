#[cfg(test)]
mod tests {
    use sqlparser::ast::*;
    use sqlparser::parser::Parser;
    use std::ops::ControlFlow;
    #[test]
    fn group_all_evaluates_predicate_once_across_chunks() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let db = server.connection().unwrap();
        db.execute_batch("CREATE TABLE dbo.group_all_native AS SELECT i%3 AS g,i AS v FROM range(6000) r(i); CREATE SEQUENCE group_all_calls").unwrap();
        let tokens=msduck_sql::group_all::tokens(crate::dialect::tokenize("SELECT g,COUNT(*) AS n,SUM(v) AS s FROM dbo.group_all_native WHERE nextval('group_all_calls')%2=0 GROUP BY ALL g").unwrap());
        let mut statement = Parser::new(&crate::dialect::ServerDialect)
            .with_tokens_with_locations(tokens)
            .parse_statement()
            .unwrap();
        crate::aggregate_columns::annotate(&db, &mut statement, &Default::default()).unwrap();
        struct Lower;
        impl VisitorMut for Lower {
            type Break = String;
            fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<String> {
                match crate::aggregate::mark(expr, &Default::default()) {
                    Ok(()) => ControlFlow::Continue(()),
                    Err(error) => ControlFlow::Break(error),
                }
            }
        }
        assert!(VisitMut::visit(&mut statement, &mut Lower).is_continue());
        let sql = format!("SELECT COUNT(*),SUM(n),SUM(s) FROM ({statement}) q");
        assert_eq!(
            db.query_row(&sql, [], |r| Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?
            )))
            .unwrap(),
            (3, 3000, 9000000)
        );
        assert_eq!(
            db.query_row("SELECT currval('group_all_calls')", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            6000
        );
    }
}
