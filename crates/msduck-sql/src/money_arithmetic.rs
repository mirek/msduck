//! Exact currency arithmetic plans over explicit operand declarations.
use crate::{expression_metadata::currency, money_cast, parameter::Parameter};
use msduck_core::money::MoneyType;
use sqlparser::ast::*;
use std::collections::HashMap;

pub fn result(name: &str) -> Option<MoneyType> {
    let tail = name.strip_prefix("__msduck_")?;
    let (family, operation) = tail.split_once('_')?;
    if !matches!(
        operation,
        "add" | "subtract" | "multiply" | "divide" | "modulo"
    ) {
        return None;
    }
    match family {
        "money" => Some(MoneyType::Money),
        "smallmoney" => Some(MoneyType::SmallMoney),
        _ => None,
    }
}

pub fn lower(
    expr: &mut Expr,
    parameters: &HashMap<String, Parameter>,
    column: &impl Fn(&Expr) -> Option<DataType>,
) {
    let Some(kind) = currency::kind(expr, parameters, column) else {
        return;
    };
    let (operation, mut left, mut right) = match expr {
        Expr::BinaryOp { left, right, op } => {
            let name = match op {
                BinaryOperator::Plus => "add",
                BinaryOperator::Minus => "subtract",
                BinaryOperator::Multiply => "multiply",
                BinaryOperator::Divide => "divide",
                BinaryOperator::Modulo => "modulo",
                _ => return,
            };
            (name, *left.clone(), *right.clone())
        }
        Expr::UnaryOp {
            op: UnaryOperator::Minus,
            expr: value,
        } => ("subtract", crate::expr::number(0), *value.clone()),
        _ => return,
    };
    money_cast::coerce(&mut left, kind);
    money_cast::coerce(&mut right, kind);
    let family = if kind == MoneyType::Money {
        "money"
    } else {
        "smallmoney"
    };
    *expr = crate::expr::binary_function(&format!("__msduck_{family}_{operation}"), left, right);
    // Preserve the declaration when the backend folds a NULL callback result.
    money_cast::coerce(expr, kind);
}

#[cfg(test)]
mod tests {
    use super::*;
    fn expr(sql: &str) -> Expr {
        sqlparser::parser::Parser::new(&crate::dialect::ServerDialect)
            .try_with_sql(sql)
            .unwrap()
            .parse_expr()
            .unwrap()
    }
    #[test]
    fn plans_preserve_operand_occurrences_precedence_and_idempotence() {
        let mut value = expr("CAST(left_source() AS MONEY) + '$2'");
        lower(&mut value, &HashMap::new(), &|_| None);
        let once = value.clone();
        lower(&mut value, &HashMap::new(), &|_| None);
        assert_eq!(value, once);
        assert_eq!(value.to_string().matches("left_source()").count(), 1);
        assert_eq!(
            currency::kind(&value, &HashMap::new(), &|_| None),
            Some(MoneyType::Money)
        );
        for sql in [
            "CAST(1 AS MONEY)+CAST(2 AS DECIMAL(19,4))",
            "CAST(1 AS MONEY)+CAST(2 AS FLOAT)",
            "CAST(1 AS MONEY)+unknown_value",
            "'a'+'b'",
        ] {
            let mut value = expr(sql);
            let original = value.clone();
            lower(&mut value, &HashMap::new(), &|_| None);
            assert_eq!(value, original);
        }
    }
}
