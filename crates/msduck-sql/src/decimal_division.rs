//! Exact division lowering from explicit operand declarations.
use crate::{
    expression_metadata::{arithmetic, storage},
    parameter::Parameter,
};
use msduck_core::types::Type;
use sqlparser::ast::*;
use std::collections::HashMap;

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
    let Some(left_kind) = storage::kind(left, parameters, column) else {
        return;
    };
    let Some(right_kind) = storage::kind(right, parameters, column) else {
        return;
    };
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
