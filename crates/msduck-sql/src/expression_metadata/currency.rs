//! Currency identity through known declarations and conditional result rules.
use super::{arithmetic, conditional, storage};
use crate::parameter::Parameter;
use msduck_core::money::MoneyType;
use sqlparser::ast::*;
use std::collections::HashMap;

pub fn kind(
    expr: &Expr,
    parameters: &HashMap<String, Parameter>,
    column: &impl Fn(&Expr) -> Option<DataType>,
) -> Option<MoneyType> {
    match result_type(expr, parameters, column)? {
        ResultType::Declaration(kind) => crate::money_cast::money_type(&kind),
        ResultType::Character => None,
    }
}

/// Common currency type for comparison operands, independent of result identity.
pub fn comparison_kind(
    left: &Expr,
    right: &Expr,
    parameters: &HashMap<String, Parameter>,
    column: &impl Fn(&Expr) -> Option<DataType>,
) -> Option<MoneyType> {
    comparison_list_kind([left, right], parameters, column)
}

/// A common comparison declaration for all known, non-NULL operands.
pub fn comparison_list_kind<'a>(
    values: impl IntoIterator<Item = &'a Expr>,
    parameters: &HashMap<String, Parameter>,
    column: &impl Fn(&Expr) -> Option<DataType>,
) -> Option<MoneyType> {
    let mut merged = None;
    for value in values {
        if conditional::literal_null(value) {
            continue;
        }
        let next = result_type(value, parameters, column)?;
        merged = Some(match merged {
            None => next,
            Some(previous) => common(previous, next)?,
        });
    }
    match merged? {
        ResultType::Declaration(kind) => crate::money_cast::money_type(&kind),
        ResultType::Character => None,
    }
}

pub fn declaration(kind: MoneyType) -> DataType {
    DataType::Custom(
        ObjectName::from(vec![Ident::new(match kind {
            MoneyType::Money => "money",
            MoneyType::SmallMoney => "smallmoney",
        })]),
        vec![],
    )
}

// Character width/encoding does not affect currency precedence. Keep that
// category explicit instead of inventing a character declaration or capacity.
#[derive(PartialEq)]
enum ResultType {
    Declaration(DataType),
    Character,
}
fn classify(kind: DataType) -> ResultType {
    if crate::character_storage::is_character(&kind) {
        ResultType::Character
    } else {
        ResultType::Declaration(kind)
    }
}
fn common(left: ResultType, right: ResultType) -> Option<ResultType> {
    use ResultType::*;
    match (left, right) {
        (Character, Character) => Some(Character),
        (Character, Declaration(kind)) | (Declaration(kind), Character) => {
            arithmetic::set_type(&kind, &kind).map(classify)
        }
        (Declaration(left), Declaration(right)) => {
            if crate::money_cast::money_type(&left).is_some() && matches!(right, DataType::Bit(_)) {
                return Some(Declaration(left));
            }
            if crate::money_cast::money_type(&right).is_some() && matches!(left, DataType::Bit(_)) {
                return Some(Declaration(right));
            }
            arithmetic::set_type(&left, &right).map(classify)
        }
    }
}
fn result_type(
    expr: &Expr,
    parameters: &HashMap<String, Parameter>,
    column: &impl Fn(&Expr) -> Option<DataType>,
) -> Option<ResultType> {
    if let Expr::UnaryOp {
        op: UnaryOperator::Plus | UnaryOperator::Minus,
        expr: value,
    } = expr
        && let Some(kind) = kind(value, parameters, column)
    {
        return Some(ResultType::Declaration(declaration(kind)));
    }
    if let Expr::BinaryOp {
        left,
        right,
        op:
            BinaryOperator::Plus
            | BinaryOperator::Minus
            | BinaryOperator::Multiply
            | BinaryOperator::Divide
            | BinaryOperator::Modulo,
    } = expr
        && let Some(kind) = comparison_kind(left, right, parameters, column)
    {
        return Some(ResultType::Declaration(declaration(kind)));
    }
    if let Expr::Function(function) = expr {
        let name = function.name.to_string().to_ascii_lowercase();
        if let Some(kind) = crate::money_arithmetic::result(&name) {
            return Some(ResultType::Declaration(declaration(kind)));
        }
        if matches!(name.as_str(), "__msduck_sum_money" | "__msduck_avg_money") {
            return Some(ResultType::Declaration(declaration(MoneyType::Money)));
        }
        if matches!(name.as_str(), "sum" | "avg" | "min" | "max")
            && let FunctionArguments::List(args) = &function.args
            && let [FunctionArg::Unnamed(FunctionArgExpr::Expr(value))] = args.args.as_slice()
            && let Some(source) = kind(value, parameters, column)
        {
            return Some(ResultType::Declaration(declaration(
                if matches!(name.as_str(), "sum" | "avg") {
                    MoneyType::Money
                } else {
                    source
                },
            )));
        }
    }
    if matches!(expr, Expr::Subquery(_)) {
        return column(expr).map(classify);
    }
    if let Expr::Nested(inner) = expr {
        return result_type(inner, parameters, column);
    }
    if let Expr::BinaryOp {
        left,
        op: BinaryOperator::Plus | BinaryOperator::StringConcat,
        right,
    } = expr
        && result_type(left, parameters, column) == Some(ResultType::Character)
        && result_type(right, parameters, column) == Some(ResultType::Character)
    {
        return Some(ResultType::Character);
    }
    if !conditional::candidate(expr) {
        return storage::kind(expr, parameters, column).map(classify);
    }
    // values selects only the first argument for NULLIF/ISNULL, except a
    // literal-NULL ISNULL first argument. Conditions and CHOOSE's index do not
    // influence the result type. Unknown operands remain a barrier.
    let mut merged = None;
    for value in conditional::values(expr) {
        if conditional::literal_null(value) {
            continue;
        }
        let next = result_type(value, parameters, column)?;
        merged = Some(match merged {
            None => next,
            Some(previous) => common(previous, next)?,
        });
    }
    merged
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
    fn aggregate_promotion_keeps_currency_identity_and_unknown_barriers() {
        for (sql, expected) in [
            ("SUM(CAST(1 AS SMALLMONEY))", Some(MoneyType::Money)),
            ("AVG(DISTINCT CAST(1 AS MONEY))", Some(MoneyType::Money)),
            ("MIN(CAST(1 AS SMALLMONEY))", Some(MoneyType::SmallMoney)),
            ("MAX(CAST(1 AS MONEY))", Some(MoneyType::Money)),
            (
                "COALESCE(SUM(CAST(1 AS SMALLMONEY)),'$2')",
                Some(MoneyType::Money),
            ),
            ("SUM(CAST(1 AS DECIMAL(19,4)))", None),
            ("AVG(unknown_value)", None),
        ] {
            assert_eq!(
                kind(&expr(sql), &HashMap::new(), &|_| None),
                expected,
                "{sql}"
            );
        }
    }
    #[test]
    fn conditional_precedence_and_first_argument_identity() {
        for (sql, expected) in [
            (
                "CASE WHEN 1=1 THEN CAST(1 AS SMALLMONEY) ELSE CAST(2 AS MONEY) END",
                Some(MoneyType::Money),
            ),
            (
                "COALESCE(NULL,CAST(NULL AS SMALLMONEY),1)",
                Some(MoneyType::SmallMoney),
            ),
            (
                "ISNULL(CAST(NULL AS SMALLMONEY),CAST(1 AS MONEY))",
                Some(MoneyType::SmallMoney),
            ),
            ("ISNULL(NULL,CAST(1 AS MONEY))", Some(MoneyType::Money)),
            (
                "NULLIF(CAST(1 AS MONEY),CAST(1 AS DECIMAL(19,4)))",
                Some(MoneyType::Money),
            ),
            ("IIF(1=1,CAST(1 AS MONEY),CAST(2 AS DECIMAL(19,4)))", None),
            ("CHOOSE(1,CAST(1 AS MONEY),CAST(2 AS FLOAT))", None),
            ("COALESCE(CAST(NULL AS MONEY),unknown_value)", None),
            (
                "CASE WHEN 1=1 THEN unknown_value ELSE CAST(1 AS MONEY) END",
                None,
            ),
            ("COALESCE(NULL,NULL)", None),
        ] {
            assert_eq!(
                kind(&expr(sql), &HashMap::new(), &|_| None),
                expected,
                "{sql}"
            );
        }
    }
    #[test]
    fn explicit_column_lookup_preserves_unknown_barriers() {
        let known = |e: &Expr| match e {
            Expr::Identifier(id) if id.value == "m" => Some(declaration(MoneyType::Money)),
            _ => None,
        };
        assert_eq!(
            kind(&expr("CHOOSE(1,NULL,m)"), &HashMap::new(), &known),
            Some(MoneyType::Money)
        );
        assert_eq!(
            kind(&expr("COALESCE(m,other)"), &HashMap::new(), &known),
            None
        );
        assert_eq!(
            kind(&expr("ISNULL(m,other)"), &HashMap::new(), &known),
            Some(MoneyType::Money)
        );
    }
}
