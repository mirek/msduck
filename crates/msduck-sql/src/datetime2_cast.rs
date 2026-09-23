//! Deterministic DATETIME2 cast plans and tagged storage declarations.
use sqlparser::ast::*;

pub const PREFIX: &str = "__msduck_datetime2_";
pub fn field(scale: u8) -> String {
    format!("{PREFIX}{scale}")
}
pub fn field_scale(name: &str) -> Option<u8> {
    let suffix = name.strip_prefix(PREFIX)?;
    if suffix.len() != 1 {
        return None;
    }
    suffix.parse::<u8>().ok().filter(|s| *s <= 7)
}
pub use crate::temporal_scale::datetime2 as scale;
pub fn storage_type(scale: u8) -> DataType {
    DataType::Struct(
        vec![StructField {
            field_name: Some(Ident::new(field(scale))),
            field_type: DataType::BigInt(None),
            options: None,
        }],
        StructBracketKind::Parentheses,
    )
}
pub fn storage_scale(kind: &DataType) -> Option<u8> {
    if let DataType::Struct(fields, _) = kind
        && fields.len() == 1
        && fields[0].field_type == DataType::BigInt(None)
    {
        return field_scale(&fields[0].field_name.as_ref()?.value);
    }
    None
}
pub fn storage_kind(name: &str) -> Option<DataType> {
    use sqlparser::{dialect::DuckDbDialect, parser::Parser};
    let kind = Parser::new(&DuckDbDialect {})
        .try_with_sql(name)
        .ok()?
        .parse_data_type()
        .ok()?;
    storage_scale(&kind).map(storage_type)
}
pub fn convert(argument: Expr, scale: u8) -> Expr {
    crate::expr::unary_function(&format!("{PREFIX}cast_{scale}"), argument)
}
pub fn column(column: &mut ColumnDef) -> Result<(), String> {
    if let Some(scale) = scale(&column.data_type)? {
        for option in &mut column.options {
            if let ColumnOption::Default(value) = &mut option.option {
                *value = convert(value.clone(), scale);
            }
        }
    }
    Ok(())
}
pub fn lower(expr: &mut Expr) -> Result<(), String> {
    let (argument, kind, trying) = match expr {
        Expr::Cast {
            expr,
            data_type,
            kind,
            ..
        } => (
            expr,
            data_type,
            matches!(kind, CastKind::TryCast | CastKind::SafeCast),
        ),
        Expr::Convert {
            expr,
            data_type: Some(data_type),
            is_try,
            styles,
            ..
        } if styles.is_empty() => (expr, data_type, *is_try),
        _ => return Ok(()),
    };
    if let Some(scale) = scale(kind)? {
        *expr = crate::expr::unary_function(
            &format!("{PREFIX}{}{scale}", if trying { "try_" } else { "cast_" }),
            *argument.clone(),
        );
    }
    Ok(())
}
