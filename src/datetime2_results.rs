//! Common exact temporal result types for conditional expressions.
use sqlparser::ast::*;

pub use msduck_sql::expression_metadata::conditional::candidate;

pub use msduck_sql::expression_metadata::conditional::values;

pub fn lower(expr: &mut Expr) {
    if !candidate(expr) {
        return;
    }
    let parameters = std::collections::HashMap::new();
    let offset_scale = values(expr)
        .into_iter()
        .filter_map(|v| crate::datetimeoffset_compare::scale(v, &parameters))
        .max();
    let datetime_scale = values(expr)
        .into_iter()
        .filter_map(|v| crate::datetime2_compare::scale(v, &parameters))
        .max();
    let time_scale = values(expr)
        .into_iter()
        .filter_map(crate::time_results::scale)
        .max();
    let Some(scale) = offset_scale.or(datetime_scale).or(time_scale) else {
        return;
    };
    let cast = |value: Expr| {
        if offset_scale.is_some() {
            crate::datetimeoffset_cast::convert(value, scale)
        } else if datetime_scale.is_some() {
            crate::datetime2_cast::convert(value, scale)
        } else {
            crate::assignment::convert(
                value,
                &DataType::Time(Some(u64::from(scale)), TimezoneInfo::None),
                false,
            )
        }
    };
    let convert = |value: &mut Expr| *value = cast(value.clone());
    if let Expr::Function(f) = expr {
        for name in ["ISNULL", "__msduck_isnull"] {
            if let Ok(Some([first, replacement])) = crate::isnull::binary_args(f, name) {
                *expr = crate::engine::binary_function(
                    "coalesce",
                    cast(first.clone()),
                    cast(replacement.clone()),
                );
                return;
            }
        }
    }
    match expr {
        Expr::Case {
            conditions,
            else_result,
            ..
        } => {
            for branch in conditions {
                convert(&mut branch.result);
            }
            if let Some(value) = else_result {
                convert(value);
            }
        }
        Expr::Function(f) if f.name.to_string().eq_ignore_ascii_case("COALESCE") => {
            if let FunctionArguments::List(args) = &mut f.args {
                for arg in &mut args.args {
                    if let FunctionArg::Unnamed(FunctionArgExpr::Expr(value)) = arg {
                        convert(value);
                    }
                }
            }
        }
        _ => {} // IIF/CHOOSE are lowered to CASE before this post-visit pass.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn offset_conditionals_preserve_payloads_and_lazy_evaluation_across_chunks() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        db.execute_batch("CREATE SEQUENCE offset_first; CREATE SEQUENCE offset_replacement")
            .unwrap();
        let sql = rewritten(
            "SELECT ISNULL(__msduck_datetimeoffset_cast_3(CASE WHEN i%2=0 THEN printf('2024-01-01T12:00:%02d.1234567+05:30',nextval('offset_first')%60) ELSE NULL END),__msduck_datetimeoffset_cast_7(printf('2024-01-01T01:30:%02d.1234567-05:00',nextval('offset_replacement')%60))) AS d FROM range(6000) r(i)",
        );
        db.execute_batch(&format!("CREATE TABLE offset_conditional AS {sql}"))
            .unwrap();
        assert_eq!(db.query_row("SELECT count(*) FROM offset_conditional WHERE d.__msduck_datetimeoffset_3%10000=0 AND d.__msduck_offset_minutes IN (330,-300)",[],|r|r.get::<_,i64>(0)).unwrap(),6000);
        for name in ["offset_first", "offset_replacement"] {
            assert_eq!(
                db.query_row("SELECT currval(?)", [name], |r| r.get::<_, i64>(0))
                    .unwrap(),
                3000
            );
        }
        let sql = rewritten(
            "SELECT COALESCE(CASE WHEN i%3=0 THEN __msduck_datetimeoffset_cast_3('2024-01-01T12:00:00.1234567+05:30') ELSE NULL END,CASE WHEN i%3=1 THEN __msduck_datetimeoffset_cast_7('2024-01-01T01:30:00.1234567-05:00') ELSE NULL END) AS d FROM range(6000) r(i)",
        );
        let mut statement = db
            .prepare(&format!(
                "SELECT d.__msduck_datetimeoffset_7,d.__msduck_offset_minutes FROM ({sql}) s"
            ))
            .unwrap();
        let rows = statement
            .query_map([], |r| {
                Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, Option<i16>>(1)?))
            })
            .unwrap()
            .collect::<duckdb::Result<Vec<_>>>()
            .unwrap();
        let utc = crate::datetime2::DateTime2::parse_iso("2024-01-01T06:30:00.1234567").unwrap();
        assert_eq!(rows.len(), 6000);
        for (i, row) in rows.into_iter().enumerate() {
            assert_eq!(
                row,
                match i % 3 {
                    0 => (Some(utc.round(3).unwrap().ticks()), Some(330)),
                    1 => (Some(utc.ticks()), Some(-300)),
                    _ => (None, None),
                }
            );
        }
    }

    #[test]
    fn isnull_evaluates_each_needed_operand_once() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        db.execute_batch(
            "CREATE SEQUENCE first_calls START 1; CREATE SEQUENCE replacement_calls START 1",
        )
        .unwrap();
        let sql = rewritten(
            "SELECT count(ISNULL(__msduck_datetime2_cast_3(CASE WHEN i%2=0 THEN printf('2026-01-01T12:34:%02d.123',nextval('first_calls')%60) ELSE NULL END),__msduck_datetime2_cast_7(printf('2026-01-01T12:34:%02d.1234567',nextval('replacement_calls')%60)))) FROM range(6000) r(i)",
        );
        assert_eq!(
            db.query_row(&sql, [], |r| r.get::<_, i64>(0)).unwrap(),
            6000
        );
        for name in ["first_calls", "replacement_calls"] {
            assert_eq!(
                db.query_row(&format!("SELECT currval('{name}')"), [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                3000
            );
        }
    }
    fn rewritten(sql: &str) -> String {
        struct Lower;
        impl VisitorMut for Lower {
            type Break = ();
            fn post_visit_expr(&mut self, expr: &mut Expr) -> std::ops::ControlFlow<()> {
                super::lower(expr);
                std::ops::ControlFlow::Continue(())
            }
        }
        use sqlparser::{dialect::GenericDialect, parser::Parser};
        let mut statement = Parser::parse_sql(&GenericDialect {}, sql)
            .unwrap()
            .remove(0);
        let _ = VisitMut::visit(&mut statement, &mut Lower);
        statement.to_string()
    }
    #[test]
    fn conditional_struct_validity_and_precision_survive_chunks() {
        let sql = "SELECT struct_extract(COALESCE(CASE WHEN i%3=0 THEN __msduck_datetime2_cast_3('2026-01-01T00:00:00.1234567') ELSE NULL END,CASE WHEN i%3=1 THEN __msduck_datetime2_cast_7('2026-01-01T00:00:00.1234567') ELSE NULL END),'__msduck_datetime2_7') FROM range(6000) r(i)";
        let statement = rewritten(sql);
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        let precise =
            crate::datetime2::DateTime2::parse_iso("2026-01-01T00:00:00.1234567").unwrap();
        let mut statement = db.prepare(&statement).unwrap();
        let rows = statement
            .query_map([], |r| r.get::<_, Option<i64>>(0))
            .unwrap()
            .collect::<duckdb::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(rows.len(), 6000);
        for (i, row) in rows.into_iter().enumerate() {
            assert_eq!(
                row,
                match i % 3 {
                    0 => Some(precise.round(3).unwrap().ticks()),
                    1 => Some(precise.ticks()),
                    _ => None,
                }
            );
        }
    }
}
