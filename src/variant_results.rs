//! Native integration coverage for SQL_VARIANT plans.
pub use msduck_sql::variant_results::*;

#[cfg(test)]
mod tests {
    #[test]
    fn coalesce_converts_only_needed_values_across_chunks() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let db = server.connection().unwrap();
        db.execute_batch("CREATE SEQUENCE first_calls; CREATE SEQUENCE replacement_calls")
            .unwrap();
        let mut statement=sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::DuckDbDialect {},"SELECT __msduck_variant_integer(coalesce(__msduck_pack_integer_variant(CASE WHEN nextval('first_calls')%2=0 THEN 42 ELSE NULL END),nextval('replacement_calls'))) FROM range(6000)").unwrap().remove(0);
        struct Lower;
        impl sqlparser::ast::VisitorMut for Lower {
            type Break = ();
            fn post_visit_expr(
                &mut self,
                expr: &mut sqlparser::ast::Expr,
            ) -> std::ops::ControlFlow<()> {
                super::lower(expr);
                std::ops::ControlFlow::Continue(())
            }
        }
        let _ = sqlparser::ast::VisitMut::visit(&mut statement, &mut Lower);
        let mut stmt = db.prepare(&statement.to_string()).unwrap();
        let rows = stmt
            .query_map([], |r| r.get::<_, i64>(0))
            .unwrap()
            .collect::<duckdb::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(rows.len(), 6000);
        assert_eq!(
            db.query_row(
                "SELECT currval('first_calls'),currval('replacement_calls')",
                [],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
            )
            .unwrap(),
            (6000, 3000)
        );
        assert_eq!(rows.iter().filter(|&&value| value == 42).count(), 3001);
    }
}
