//! Native integration coverage for SQL_VARIANT plans.
pub use msduck_sql::variant_compare::*;

#[cfg(test)]
mod tests {
    fn translated(sql: &str) -> String {
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
        let mut statement =
            sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::DuckDbDialect {}, sql)
                .unwrap()
                .remove(0);
        let _ = sqlparser::ast::VisitMut::visit(&mut statement, &mut Lower);
        statement.to_string()
    }
    #[test]
    fn comparisons_keep_null_truth_tables_and_single_evaluation() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let db = server.connection().unwrap();
        let a = "__msduck_pack_integer_variant(CAST(CASE WHEN i%3=0 THEN NULL ELSE i%3 END AS SMALLINT))";
        let b = "__msduck_pack_integer_variant(CAST(CASE WHEN (i//3)%3=0 THEN NULL ELSE (i//3)%3 END AS BIGINT))";
        let sql = translated(&format!(
            "SELECT {a}={b},{a}<>{b},{a}<{b},{a}<={b},{a}>{b},{a}>={b},{a} IS DISTINCT FROM {b},{a} IS NOT DISTINCT FROM {b} FROM range(6000) r(i)"
        ));
        let mut stmt = db.prepare(&sql).unwrap();
        for (i, row) in stmt
            .query_map([], |r| {
                (0..8)
                    .map(|j| r.get::<_, Option<bool>>(j))
                    .collect::<duckdb::Result<Vec<_>>>()
            })
            .unwrap()
            .enumerate()
        {
            let a = if i % 3 == 0 { None } else { Some(i % 3) };
            let b = if (i / 3) % 3 == 0 {
                None
            } else {
                Some((i / 3) % 3)
            };
            let pair = a.zip(b);
            assert_eq!(
                row.unwrap(),
                vec![
                    pair.map(|(a, b)| a == b),
                    pair.map(|(a, b)| a != b),
                    pair.map(|(a, b)| a < b),
                    pair.map(|(a, b)| a <= b),
                    pair.map(|(a, b)| a > b),
                    pair.map(|(a, b)| a >= b),
                    Some(a != b),
                    Some(a == b)
                ]
            );
        }
        db.execute_batch("CREATE SEQUENCE compare_calls").unwrap();
        let sql = translated(
            "SELECT count(*) FROM range(6000) WHERE __msduck_pack_integer_variant(CASE WHEN nextval('compare_calls')%2=0 THEN 42 ELSE NULL END)=__msduck_pack_integer_variant(CAST(42 AS SMALLINT))",
        );
        assert_eq!(
            db.query_row(&sql, [], |r| r.get::<_, i64>(0)).unwrap(),
            3000
        );
        assert_eq!(
            db.query_row("SELECT currval('compare_calls')", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            6000
        );
    }
}
