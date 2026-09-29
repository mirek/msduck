//! Exact division lowering with contextual bare-integer operand declarations.
use crate::{
    expression_metadata::{arithmetic, storage},
    parameter::Parameter,
};
use msduck_core::types::Type;
use sqlparser::ast::*;
use std::collections::HashMap;

/// A bare integer token converted alongside a decimal uses its minimum
/// precision. A CAST or a bound INT retains the declared INT precision ten.
/// This is intentionally limited to direct literals; general expression
/// binding belongs to the shared metadata pass.
pub fn bare_integer_decimal_type(expr: &Expr) -> Option<DataType> {
    let number = match expr {
        Expr::Value(value) => match &value.value {
            Value::Number(number, _) => number,
            _ => return None,
        },
        Expr::Nested(value)
        | Expr::UnaryOp {
            op: UnaryOperator::Plus | UnaryOperator::Minus,
            expr: value,
        } => return bare_integer_decimal_type(value),
        _ => return None,
    };
    if !matches!(
        storage::numeric_literal_type(number),
        Some(DataType::Int(_))
    ) {
        return None;
    }
    let precision = number.trim_start_matches('0').len().max(1) as u64;
    Some(DataType::Decimal(ExactNumberInfo::PrecisionAndScale(
        precision, 0,
    )))
}

fn decimal(kind: &DataType) -> bool {
    matches!(kind, DataType::Decimal(_) | DataType::Numeric(_))
}

fn operand(kind: &DataType) -> Option<DataType> {
    let (precision, scale) = match crate::sql_type::declaration(kind).ok()? {
        Type::Decimal(d) => (d.precision(), d.scale()),
        Type::TinyInt => (3, 0),
        Type::SmallInt => (5, 0),
        Type::Int => (10, 0),
        Type::BigInt => (19, 0),
        Type::SmallMoney => (10, 4),
        Type::Money => (19, 4),
        _ => return None,
    };
    Some(DataType::Decimal(ExactNumberInfo::PrecisionAndScale(
        precision.into(),
        scale.into(),
    )))
}
fn cast(value: Expr, data_type: DataType) -> Expr {
    Expr::Cast {
        kind: CastKind::Cast,
        expr: Box::new(value),
        data_type,
        format: None,
    }
}
pub fn lower(
    expr: &mut Expr,
    parameters: &HashMap<String, Parameter>,
    column: &impl Fn(&Expr) -> Option<DataType>,
) {
    let Expr::BinaryOp {
        left,
        op: BinaryOperator::Divide,
        right,
    } = expr
    else {
        return;
    };
    let Some(mut left_kind) = storage::kind(left, parameters, column) else {
        return;
    };
    let Some(mut right_kind) = storage::kind(right, parameters, column) else {
        return;
    };
    if decimal(&right_kind) {
        left_kind = bare_integer_decimal_type(left).unwrap_or(left_kind);
    }
    if decimal(&left_kind) {
        right_kind = bare_integer_decimal_type(right).unwrap_or(right_kind);
    }
    let Some(result) = arithmetic::decimal_type(&BinaryOperator::Divide, &left_kind, &right_kind)
    else {
        return;
    };
    let Ok(Type::Decimal(decimal)) = crate::sql_type::declaration(&result) else {
        return;
    };
    let (Some(left_kind), Some(right_kind)) = (operand(&left_kind), operand(&right_kind)) else {
        return;
    };
    let mut function = crate::expr::binary_function(
        &format!("__msduck_decimal_divide_{}", decimal.scale()),
        cast(*left.clone(), left_kind),
        cast(*right.clone(), right_kind),
    );
    if let Expr::Function(f) = &mut function
        && let FunctionArguments::List(args) = &mut f.args
    {
        args.args.push(FunctionArg::Unnamed(FunctionArgExpr::Expr(
            crate::expr::number(decimal.precision()),
        )));
    }
    *expr = cast(function, result);
}
