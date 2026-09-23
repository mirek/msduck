#[cfg(test)]
mod tests {
    use sqlparser::{ast::*, parser::Parser};
    use std::ops::ControlFlow;
    #[test]
    fn set_comparisons_pad_vectorized_inputs() {
        struct Lower;
        impl VisitorMut for Lower {
            type Break = String;
            fn pre_visit_expr(&mut self, value: &mut Expr) -> ControlFlow<String> {
                crate::result_types::lower_fixed_results(value);
                crate::ncharacter::lower(value).unwrap();
                crate::varchar::lower(value).unwrap();
                ControlFlow::Continue(())
            }
        }
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        for kind in ["CHAR", "NCHAR"] {
            for (op, count) in [
                ("UNION", 2),
                ("UNION ALL", 12000),
                ("INTERSECT", 2),
                ("EXCEPT", 0),
            ] {
                let source = format!(
                    "SELECT CAST(CASE WHEN i%2=0 THEN 'a' ELSE NULL END AS NCHAR(2)) AS value FROM range(6000) r(i) {op} SELECT CAST(CASE WHEN i%2=0 THEN 'a' ELSE NULL END AS NCHAR(4)) FROM range(6000) r(i)"
                );
                let source = source.replace("NCHAR", kind);
                let mut sql = Parser::parse_sql(&sqlparser::dialect::MsSqlDialect {}, &source)
                    .unwrap()
                    .remove(0);
                msduck_sql::result_types::lower_fixed_sets(&mut sql);
                assert!(VisitMut::visit(&mut sql, &mut Lower).is_continue());
                let mut stmt = db.prepare(&sql.to_string()).unwrap();
                let rows = stmt
                    .query_map([], |r| r.get::<_, Option<String>>(0))
                    .unwrap();
                let mut actual = 0;
                for row in rows {
                    if let Some(value) = row.unwrap() {
                        assert_eq!(value, "a   ");
                    }
                    actual += 1;
                }
                assert_eq!(actual, count, "{op}");
            }
        }
    }
}
