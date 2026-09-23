//! Currency-to-character plans retain logical source identity before lowering.
use crate::{
    expression_metadata::{character, currency},
    parameter::Parameter,
};
use sqlparser::ast::*;
use std::collections::HashMap;

/// Format an implicit currency assignment before the destination applies its
/// storage width. An unbounded intermediate prevents expression truncation.
pub fn storage(value: Expr) -> Expr {
    let value = Expr::Cast {
        kind: CastKind::Cast,
        expr: Box::new(value),
        data_type: DataType::Decimal(ExactNumberInfo::PrecisionAndScale(19, 4)),
        format: None,
    };
    let mut result =
        crate::expr::binary_function("__msduck_money_format", value, crate::expr::number(0));
    if let Expr::Function(function) = &mut result
        && let FunctionArguments::List(args) = &mut function.args
    {
        for value in [-1, 0] {
            args.args.push(FunctionArg::Unnamed(FunctionArgExpr::Expr(
                crate::expr::number(value),
            )));
        }
    }
    result
}

pub fn candidate(expr: &Expr) -> bool {
    match expr {
        Expr::Cast { data_type, .. }
        | Expr::Convert {
            data_type: Some(data_type),
            ..
        } => crate::character_storage::is_character(data_type),
        _ => false,
    }
}

pub fn lower(
    expr: &mut Expr,
    parameters: &HashMap<String, Parameter>,
    column: &impl Fn(&Expr) -> Option<DataType>,
) -> Result<(), String> {
    let (value, target, trying, style) = match expr {
        Expr::Cast {
            expr: value,
            data_type,
            kind,
            format: None,
        } => (
            value,
            data_type,
            matches!(kind, CastKind::TryCast | CastKind::SafeCast),
            crate::expr::number(0),
        ),
        Expr::Convert {
            expr: value,
            data_type: Some(data_type),
            is_try,
            styles,
            charset: None,
            ..
        } if styles.len() <= 1 => (
            value,
            data_type,
            *is_try,
            styles
                .first()
                .cloned()
                .unwrap_or_else(|| crate::expr::number(0)),
        ),
        _ => return Ok(()),
    };
    if !crate::character_storage::is_character(target) {
        return Ok(());
    }
    let Some(source) = currency::kind(value, parameters, column) else {
        return Ok(());
    };
    let (family, width) = match target {
        DataType::Varchar(_) => (0, i32::from(character::varchar_cast_width(target)?)),
        DataType::Char(_) | DataType::Character(_) => {
            (1, i32::from(character::varchar_cast_width(target)?))
        }
        DataType::Nvarchar(_) => (
            2,
            character::nvarchar_cast_width(target)?
                .map(i32::from)
                .unwrap_or(-1),
        ),
        _ => match character::nchar_cast_width(target)? {
            Some(width) => (3, i32::from(width)),
            None => return Ok(()),
        },
    };
    let width = if width == i32::from(u16::MAX) {
        -1
    } else {
        width
    };
    let cast = |value, data_type| Expr::Cast {
        kind: CastKind::Cast,
        expr: Box::new(value),
        data_type,
        format: None,
    };
    let mut formatted = crate::expr::binary_function(
        if trying {
            "__msduck_try_money_format"
        } else {
            "__msduck_money_format"
        },
        cast(
            crate::money_cast::convert(*value.clone(), source, false),
            DataType::Decimal(ExactNumberInfo::PrecisionAndScale(19, 4)),
        ),
        cast(style, DataType::Int(None)),
    );
    if let Expr::Function(f) = &mut formatted
        && let FunctionArguments::List(args) = &mut f.args
    {
        for value in [width, family] {
            args.args.push(FunctionArg::Unnamed(FunctionArgExpr::Expr(
                crate::expr::number(value),
            )));
        }
    }
    // Retain the declared character result shape, including NULL and empty sets.
    *expr = cast(formatted, target.clone());
    Ok(())
}
