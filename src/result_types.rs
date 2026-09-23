//! Acquire result metadata and adapt logical result shapes to TDS descriptors.
use crate::tds::Type;
use msduck_core::{
    character::{Family, Length},
    money::MoneyType,
};
use msduck_sql::result_types::ResultType;
use sqlparser::ast::*;

pub use msduck_sql::result_types::lower_fixed_results;

fn wire(kind: ResultType) -> Type {
    match kind {
        ResultType::Time(scale) => Type::Time(scale),
        ResultType::Money(MoneyType::Money) => Type::Money(8),
        ResultType::Money(MoneyType::SmallMoney) => Type::Money(4),
        ResultType::Character {
            family: Family::Nvarchar,
            length: Length::Max,
        } => Type::Text,
        ResultType::Character { family, length } => {
            let width = match length {
                Length::Bounded(width) => width,
                Length::Max => u16::MAX,
            };
            match family {
                Family::Char => Type::Char(width),
                Family::Varchar => Type::Varchar(width),
                Family::Nchar => Type::Nchar(width),
                Family::Nvarchar => Type::Nvarchar(width),
            }
        }
    }
}
pub fn projection(statement: &Statement) -> Vec<Option<Type>> {
    msduck_sql::result_types::projection(statement)
        .into_iter()
        .map(|kind| kind.map(wire))
        .collect()
}
pub fn isnull_type(first: &Expr, replacement: &Expr) -> Option<Type> {
    msduck_sql::result_types::isnull_type(first, replacement).map(wire)
}

pub fn bound_projection(
    db: &duckdb::Connection,
    statement: &Statement,
    parameters: &std::collections::HashMap<String, crate::parameter::Parameter>,
) -> duckdb::Result<Vec<Option<Type>>> {
    struct Parameters<'a>(&'a std::collections::HashMap<String, crate::parameter::Parameter>);
    impl VisitorMut for Parameters<'_> {
        type Break = ();
        fn post_visit_expr(&mut self, expr: &mut Expr) -> std::ops::ControlFlow<()> {
            if let Expr::Identifier(id) = expr
                && let Some(parameter) = self.0.get(&id.value.to_lowercase())
                && (matches!(parameter.data_type, msduck_core::types::Type::Character(_))
                    || matches!(parameter.ast_type(), DataType::Time(..))
                    || msduck_sql::money_cast::money_type(&parameter.ast_type()).is_some())
            {
                *expr = Expr::Cast {
                    kind: CastKind::Cast,
                    expr: Box::new(Expr::Value(Value::Null.into())),
                    data_type: parameter.ast_type(),
                    format: None,
                };
            }
            std::ops::ControlFlow::Continue(())
        }
    }
    let mut typed = statement.clone();
    let _ = VisitMut::visit(&mut typed, &mut Parameters(parameters));
    let mut types = projection(&typed);
    if let Statement::Query(query) = &typed
        && let Some(fields) =
            crate::query_catalog::projection_with_parameters(db, query, parameters)?
    {
        fill_fields(&mut types, &fields);
    }
    Ok(types)
}

/// Wire overrides from logical declarations, independent of a physical query.
pub fn from_fields(fields: &[crate::query_catalog::Field]) -> Vec<Option<Type>> {
    let mut types = Vec::new();
    fill_fields(&mut types, fields);
    types
}
fn fill_fields(types: &mut Vec<Option<Type>>, fields: &[crate::query_catalog::Field]) {
    if types.is_empty() {
        types.resize(fields.len(), None);
    }
    if types.len() == fields.len() {
        for (kind, field) in types.iter_mut().zip(fields) {
            if kind.is_some() {
                continue;
            }
            let Some(info) = &field.info else {
                continue;
            };
            match info.system_type_id {
                Some(60) => *kind = Some(Type::Money(8)),
                Some(122) => *kind = Some(Type::Money(4)),
                Some(41) => {
                    if let Some(scale) = info.scale.filter(|s| (0..=7).contains(s)) {
                        *kind = Some(Type::Time(scale));
                    }
                }
                Some(id @ (165 | 173)) => {
                    if let Some(bytes) = info.max_length.filter(|n| (1..=8000).contains(n)) {
                        *kind = Some(if id == 173 {
                            Type::FixedBinary(bytes as u16)
                        } else {
                            Type::Varbinary(bytes as u16)
                        });
                    }
                }
                Some(id @ (167 | 175)) => {
                    if let Some(bytes) = info.max_length {
                        if (1..=8000).contains(&bytes) {
                            *kind = Some(if id == 175 {
                                Type::Char(bytes as u16)
                            } else {
                                Type::Varchar(bytes as u16)
                            });
                        } else if id == 167 && bytes == -1 {
                            *kind = Some(Type::Varchar(u16::MAX));
                        }
                    }
                }
                Some(id @ (231 | 239)) => {
                    if let Some(bytes) = info
                        .max_length
                        .filter(|n| (2..=8000).contains(n) && n % 2 == 0)
                    {
                        *kind = Some(if id == 239 {
                            Type::Nchar((bytes / 2) as u16)
                        } else {
                            Type::Nvarchar((bytes / 2) as u16)
                        });
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ops::ControlFlow;
    #[test]
    fn conditional_padding_preserves_branch_evaluation_across_chunks() {
        struct Lower;
        impl VisitorMut for Lower {
            type Break = String;
            fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<String> {
                super::lower_fixed_results(expr);
                crate::ncharacter::lower(expr).unwrap();
                crate::varchar::lower(expr).unwrap();
                ControlFlow::Continue(())
            }
        }
        for kind in ["CHAR", "NCHAR"] {
            let source="SELECT CASE WHEN i%2=0 THEN CAST(nextval('padding_calls') AS NCHAR(4)) ELSE CAST(NULL AS NCHAR(6)) END FROM range(6000) r(i)".replace("NCHAR",kind);
            let mut sql =
                sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::MsSqlDialect {}, &source)
                    .unwrap()
                    .remove(0);
            assert!(matches!(
                VisitMut::visit(&mut sql, &mut Lower),
                ControlFlow::Continue(())
            ));
            let db = duckdb::Connection::open_in_memory().unwrap();
            crate::scalar::register(&db).unwrap();
            db.execute_batch("CREATE SEQUENCE padding_calls START 1")
                .unwrap();
            let mut stmt = db.prepare(&sql.to_string()).unwrap();
            let rows = stmt
                .query_map([], |r| r.get::<_, Option<String>>(0))
                .unwrap();
            for (index, row) in rows.enumerate() {
                assert_eq!(
                    row.unwrap(),
                    if index % 2 == 0 {
                        Some(format!("{:<6}", index / 2 + 1))
                    } else {
                        None
                    }
                );
            }
            let calls: i64 = db
                .query_row("SELECT currval('padding_calls')", [], |r| r.get(0))
                .unwrap();
            assert_eq!(calls, 3000);
        }
    }
}
