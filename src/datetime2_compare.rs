//! Native integration coverage for the pure DATETIME2 comparison plan.
pub use msduck_sql::datetime2_compare::*;

#[cfg(test)]
mod tests {
    use sqlparser::ast::*;
    #[test]
    fn predicate_truth_tables_and_simple_case_across_chunks() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        let value = |index: &str, scale: u8| {
            format!(
                "__msduck_datetime2_cast_{scale}(CASE {index}%3 WHEN 0 THEN NULL WHEN 1 THEN '2024-01-01' ELSE '2024-01-02' END)"
            )
        };
        let (v, lo, hi) = (value("i", 3), value("i//3", 0), value("i//9", 7));
        // Inline typed operands exercise lowering independently of catalog lookup.
        let sql = format!(
            "SELECT {v} IS DISTINCT FROM {lo}, {v} IS NOT DISTINCT FROM {lo}, {v} BETWEEN {lo} AND {hi}, {v} NOT BETWEEN {lo} AND {hi}, {v} IN ({lo},{hi}), {v} NOT IN ({lo},{hi}), CASE {v} WHEN {lo} THEN 1 WHEN {hi} THEN 2 ELSE 3 END FROM range(6000) r(i)"
        );
        let mut statement = db.prepare(&translated(&sql)).unwrap();
        let rows = statement
            .query_map([], |r| {
                Ok((
                    r.get::<_, Option<bool>>(0)?,
                    r.get::<_, Option<bool>>(1)?,
                    r.get::<_, Option<bool>>(2)?,
                    r.get::<_, Option<bool>>(3)?,
                    r.get::<_, Option<bool>>(4)?,
                    r.get::<_, Option<bool>>(5)?,
                    r.get::<_, i32>(6)?,
                ))
            })
            .unwrap();
        let mut between_matches = 0;
        let mut list_matches = 0;
        for (i, row) in rows.enumerate() {
            let input = |n| if n % 3 == 0 { None } else { Some(n % 3) };
            let (v, lo, hi) = (input(i), input(i / 3), input(i / 9));
            let a = v.zip(lo).map(|(v, lo)| v >= lo);
            let b = v.zip(hi).map(|(v, hi)| v <= hi);
            let between = if a == Some(false) || b == Some(false) {
                Some(false)
            } else {
                a.zip(b).map(|(a, b)| a && b)
            };
            let a = v.zip(lo).map(|(v, lo)| v == lo);
            let b = v.zip(hi).map(|(v, hi)| v == hi);
            let member = if a == Some(true) || b == Some(true) {
                Some(true)
            } else {
                a.zip(b).map(|(a, b)| a || b)
            };
            let selected = if a == Some(true) {
                1
            } else if b == Some(true) {
                2
            } else {
                3
            };
            between_matches += i64::from(between == Some(true));
            list_matches += i64::from(member == Some(true));
            assert_eq!(
                row.unwrap(),
                (
                    Some(v != lo),
                    Some(v == lo),
                    between,
                    between.map(|v| !v),
                    member,
                    member.map(|v| !v),
                    selected
                ),
                "row {i}"
            );
        }
        for (predicate, expected) in [
            (format!("{v} BETWEEN {lo} AND {hi}"), between_matches),
            (format!("{v} IN ({lo},{hi})"), list_matches),
        ] {
            let count: i64 = db
                .query_row(
                    &translated(&format!(
                        "SELECT count(*) FROM range(6000) r(i) WHERE {predicate}"
                    )),
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(count, expected);
        }
    }
    fn translated(sql: &str) -> String {
        struct Compare;
        impl VisitorMut for Compare {
            type Break = ();
            fn post_visit_expr(&mut self, expr: &mut Expr) -> std::ops::ControlFlow<()> {
                super::lower(expr);
                std::ops::ControlFlow::Continue(())
            }
        }
        use sqlparser::{dialect::GenericDialect, parser::Parser};
        let mut stmt = Parser::parse_sql(&GenericDialect {}, sql)
            .unwrap()
            .remove(0);
        let _ = VisitMut::visit(&mut stmt, &mut Compare);
        stmt.to_string()
    }
    #[test]
    fn compares_all_scale_pairs_and_evaluates_each_operand_once() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        for left in 0..=7 {
            for right in 0..=7 {
                let count: i64 = db.query_row(&translated(&format!("SELECT count(*) FROM range(6000) r(i) WHERE (__msduck_datetime2_cast_{left}(CASE WHEN i%17=0 THEN NULL ELSE '2024-02-29T12:34:56' END) = __msduck_datetime2_cast_{right}('2024-02-29T12:34:56')) IS DISTINCT FROM CASE WHEN i%17=0 THEN NULL ELSE true END")), [], |r| r.get(0)).unwrap();
                assert_eq!(count, 0);
            }
        }
        db.execute_batch("CREATE SEQUENCE left_calls START 1; CREATE SEQUENCE right_calls START 1")
            .unwrap();
        let count: i64 = db.query_row(&translated("SELECT count(*) FROM range(6000) WHERE (__msduck_datetime2_cast_3(DATE '2024-01-01' + CAST(nextval('left_calls')%28 AS INT)) = __msduck_datetime2_cast_7(DATE '2024-01-01' + CAST(nextval('right_calls')%28 AS INT)))"), [], |r| r.get(0)).unwrap();
        assert_eq!(count, 6000);
        for name in ["left_calls", "right_calls"] {
            assert_eq!(
                db.query_row(&format!("SELECT currval('{name}')"), [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                6000
            );
        }
    }
}
