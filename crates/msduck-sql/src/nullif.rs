use sqlparser::ast::*;

pub use crate::expression_metadata::conditional::nullif_args as args;

pub fn lower(expr: &mut Expr) -> Result<(), String> {
    let Expr::Function(function) = expr else {
        return Ok(());
    };
    let Some([first, second]) = args(function)? else {
        return Ok(());
    };
    let original = first.clone();
    let left = first.clone();
    let right = second.clone();
    *expr = Expr::Case {
        case_token: helpers::attached_token::AttachedToken::empty(),
        end_token: helpers::attached_token::AttachedToken::empty(),
        operand: None,
        conditions: vec![CaseWhen {
            condition: Expr::BinaryOp {
                left: Box::new(left),
                op: BinaryOperator::Eq,
                right: Box::new(right),
            },
            result: Expr::Value(Value::Null.into()),
        }],
        else_result: Some(Box::new(original)),
    };
    Ok(())
}

/// Convert only comparison operands; the original first operand remains the result.
pub fn lower_currency(
    expr: &mut Expr,
    parameters: &std::collections::HashMap<String, crate::parameter::Parameter>,
    column: &impl Fn(&Expr) -> Option<DataType>,
) -> Result<(), String> {
    let Expr::Function(function) = expr else {
        return Ok(());
    };
    let Some([first, second]) = args(function)? else {
        return Ok(());
    };
    let Some(kind) =
        crate::expression_metadata::currency::comparison_kind(first, second, parameters, column)
    else {
        return Ok(());
    };
    lower(expr)?;
    let Expr::Case { conditions, .. } = expr else {
        unreachable!()
    };
    let Expr::BinaryOp { left, right, .. } = &mut conditions[0].condition else {
        unreachable!()
    };
    for value in [left, right] {
        crate::money_cast::coerce(value, kind);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    fn parse(sql: &str) -> Expr {
        sqlparser::parser::Parser::new(&crate::dialect::ServerDialect)
            .try_with_sql(sql)
            .unwrap()
            .parse_expr()
            .unwrap()
    }
    #[test]
    fn currency_comparisons_preserve_first_result_and_precedence_barriers() {
        for sql in [
            "NULLIF('$12',CAST(1 AS MONEY))",
            "NULLIF(1,CAST(2 AS SMALLMONEY))",
            "NULLIF(TRY_CAST('bad' AS MONEY),'$2')",
        ] {
            let mut expr = parse(sql);
            let Expr::Function(f) = &expr else {
                unreachable!()
            };
            let first = args(f).unwrap().unwrap()[0].clone();
            lower_currency(&mut expr, &HashMap::new(), &|_| None).unwrap();
            let Expr::Case {
                else_result,
                conditions,
                ..
            } = &expr
            else {
                panic!("{sql}")
            };
            assert_eq!(else_result.as_deref(), Some(&first));
            let Expr::BinaryOp { left, right, .. } = &conditions[0].condition else {
                unreachable!()
            };
            assert!(matches!(left.as_ref(), Expr::Cast { .. }));
            assert!(matches!(right.as_ref(), Expr::Cast { .. }));
            let once = expr.clone();
            lower_currency(&mut expr, &HashMap::new(), &|_| None).unwrap();
            assert_eq!(expr, once);
        }
        for sql in [
            "NULLIF(CAST(1 AS MONEY),CAST(1 AS DECIMAL(30,5)))",
            "NULLIF(CAST(1 AS SMALLMONEY),unknown_value)",
        ] {
            let mut expr = parse(sql);
            let original = expr.clone();
            lower_currency(&mut expr, &HashMap::new(), &|_| None).unwrap();
            assert_eq!(expr, original);
        }
    }
}
