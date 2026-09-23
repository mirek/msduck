//! Currency precedence for comparisons; no value evaluation or I/O.
use crate::{expression_metadata::currency, money_cast, parameter::Parameter};
use sqlparser::ast::*;
use std::collections::HashMap;

pub fn lower(
    expr: &mut Expr,
    parameters: &HashMap<String, Parameter>,
    column: &impl Fn(&Expr) -> Option<DataType>,
) {
    let values: Vec<&mut Expr> = match expr {
        Expr::BinaryOp {
            left,
            right,
            op:
                BinaryOperator::Eq
                | BinaryOperator::NotEq
                | BinaryOperator::Lt
                | BinaryOperator::LtEq
                | BinaryOperator::Gt
                | BinaryOperator::GtEq,
        }
        | Expr::IsDistinctFrom(left, right)
        | Expr::IsNotDistinctFrom(left, right) => vec![left, right],
        Expr::Between {
            expr, low, high, ..
        } => vec![expr, low, high],
        Expr::InList { expr, list, .. } => std::iter::once(expr.as_mut())
            .chain(list.iter_mut())
            .collect(),
        Expr::Case {
            operand: Some(operand),
            conditions,
            ..
        } => std::iter::once(operand.as_mut())
            .chain(conditions.iter_mut().map(|branch| &mut branch.condition))
            .collect(),
        _ => return,
    };
    let Some(kind) =
        currency::comparison_list_kind(values.iter().map(|value| &**value), parameters, column)
    else {
        return;
    };
    for value in values {
        money_cast::coerce(value, kind);
    }
}

/// Coerce the complete one-column subquery result after its ordering, DISTINCT
/// and row limits, using the binder's explicit output declaration.
pub fn subquery(
    value: &mut Expr,
    query: &mut Box<Query>,
    right_type: &DataType,
    parameters: &HashMap<String, Parameter>,
    column: &impl Fn(&Expr) -> Option<DataType>,
) {
    let right = Expr::Cast {
        kind: CastKind::Cast,
        expr: Box::new(Expr::Value(Value::Null.into())),
        data_type: right_type.clone(),
        format: None,
    };
    let Some(kind) = currency::comparison_kind(value, &right, parameters, column) else {
        return;
    };
    money_cast::coerce(value, kind);
    if money_cast::money_type(right_type) == Some(kind) {
        return;
    }
    // set_coercion scans the entire original query for generated-name collisions.
    let Statement::Query(placeholder) =
        sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::GenericDialect {}, "SELECT NULL")
            .expect("constant query skeleton")
            .remove(0)
    else {
        unreachable!()
    };
    let source = std::mem::replace(query, placeholder);
    let mut body = Box::new(SetExpr::Query(source));
    crate::set_coercion::wrap(
        &mut body,
        &[String::new()],
        &[Some(currency::declaration(kind))],
    );
    query.body = body;
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parse(sql: &str) -> Expr {
        sqlparser::parser::Parser::new(&crate::dialect::ServerDialect)
            .try_with_sql(sql)
            .unwrap()
            .parse_expr()
            .unwrap()
    }
    #[test]
    fn subquery_wrapper_preserves_complete_query_and_avoids_outer_name_capture() {
        let Statement::Query(mut query) = crate::batch::parse("SELECT DISTINCT TOP(2) __msduck_numeric_set_source.t FROM t WHERE id=outer_id ORDER BY t").unwrap().remove(0) else { unreachable!() };
        let original = query.clone();
        let mut value = parse("CAST(12 AS MONEY)");
        let text = DataType::Varchar(Some(CharacterLength::IntegerLength {
            length: 30,
            unit: None,
        }));
        subquery(&mut value, &mut query, &text, &HashMap::new(), &|_| None);
        let SetExpr::Select(select) = query.body.as_ref() else {
            unreachable!()
        };
        let TableFactor::Derived {
            subquery: nested,
            alias: Some(alias),
            ..
        } = &select.from[0].relation
        else {
            unreachable!()
        };
        assert_ne!(alias.name.value, "__msduck_numeric_set_source");
        let SetExpr::Query(source) = nested.body.as_ref() else {
            unreachable!()
        };
        assert_eq!(source, &original);
        let once = query.clone();
        subquery(
            &mut value,
            &mut query,
            &currency::declaration(msduck_core::money::MoneyType::Money),
            &HashMap::new(),
            &|_| None,
        );
        assert_eq!(query, once);
        let mut unknown = parse("unknown_value");
        subquery(&mut unknown, &mut query, &text, &HashMap::new(), &|_| None);
        assert_eq!(query, once);
    }
    #[test]
    fn multi_operand_comparisons_preserve_shape_results_and_unknown_barriers() {
        for sql in [
            "CAST(nextval('n') AS MONEY) NOT BETWEEN '$1' AND '$2'",
            "CAST(nextval('n') AS MONEY) NOT IN ('$1', '$2', NULL)",
            "CASE CAST(nextval('n') AS MONEY) WHEN '$1' THEN N'one' WHEN '$2' THEN N'two' ELSE N'other' END",
        ] {
            let mut value = parse(sql);
            let original = value.clone();
            lower(&mut value, &HashMap::new(), &|_| None);
            assert_ne!(value, original);
            assert_eq!(value.to_string().matches("nextval").count(), 1);
            if let Expr::Case {
                conditions,
                else_result,
                ..
            } = &value
            {
                let Expr::Case {
                    conditions: original_conditions,
                    else_result: original_else,
                    ..
                } = &original
                else {
                    unreachable!()
                };
                assert_eq!(else_result, original_else);
                assert_eq!(
                    conditions.iter().map(|b| &b.result).collect::<Vec<_>>(),
                    original_conditions
                        .iter()
                        .map(|b| &b.result)
                        .collect::<Vec<_>>()
                );
            }
            let once = value.clone();
            lower(&mut value, &HashMap::new(), &|_| None);
            assert_eq!(value, once);
        }
        for sql in [
            "CAST(1 AS MONEY) IN ('$1', unknown_value)",
            "CAST(1 AS MONEY) BETWEEN '$1' AND CAST(2 AS DECIMAL(30,5))",
            "CASE CAST(1 AS MONEY) WHEN CAST(1 AS FLOAT) THEN 1 ELSE 0 END",
            "CAST(1 AS MONEY) IN (SELECT unknown_value FROM t)",
        ] {
            let mut value = parse(sql);
            let original = value.clone();
            lower(&mut value, &HashMap::new(), &|_| None);
            assert_eq!(value, original);
        }
    }
    #[test]
    fn comparisons_retain_operators_nulls_try_and_producer_count() {
        for sql in [
            "CAST(nextval('n') AS MONEY) = '$2'",
            "CAST(nextval('n') AS SMALLMONEY) <> 2",
            "'$2' < CAST(nextval('n') AS MONEY)",
            "CAST(nextval('n') AS MONEY) <= '$2'",
            "CAST(nextval('n') AS MONEY) > '$2'",
            "CAST(nextval('n') AS MONEY) >= '$2'",
            "TRY_CAST('bad' AS MONEY) IS NOT DISTINCT FROM NULL",
            "CAST(NULL AS MONEY) IS DISTINCT FROM '$2'",
        ] {
            let mut value = parse(sql);
            lower(&mut value, &HashMap::new(), &|_| None);
            let once = value.clone();
            lower(&mut value, &HashMap::new(), &|_| None);
            assert_eq!(value, once);
            assert_eq!(
                value.to_string().matches("nextval").count(),
                sql.matches("nextval").count()
            );
            if sql.starts_with("TRY_CAST") {
                assert!(value.to_string().contains("TRY_CAST('bad' AS MONEY)"));
            }
        }
    }
    #[test]
    fn snapshot_types_convert_columns_and_unknown_or_higher_types_block_narrowing() {
        let money = |e: &Expr| {
            matches!(e, Expr::Identifier(id) if id.value == "m")
                .then(|| currency::declaration(msduck_core::money::MoneyType::Money))
        };
        let mut value = parse("m = '$2'");
        lower(&mut value, &HashMap::new(), &money);
        assert!(value.to_string().contains("CAST('$2' AS money)"));
        for sql in [
            "m = unknown_value",
            "m = CAST(1 AS DECIMAL(30,5))",
            "m = CAST(1 AS FLOAT)",
            "m + '$2'",
        ] {
            let mut value = parse(sql);
            let original = value.clone();
            lower(&mut value, &HashMap::new(), &money);
            assert_eq!(value, original);
        }
    }
}
