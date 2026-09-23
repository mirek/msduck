//! Deterministic DATETIMEOFFSET cast plans and tagged storage declarations.
use sqlparser::ast::*;

pub const OFFSET: &str = "__msduck_offset_minutes";
pub const PREFIX: &str = "__msduck_datetimeoffset_";
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
pub use crate::temporal_scale::datetimeoffset as scale;
pub fn storage_type(scale: u8) -> DataType {
    DataType::Struct(
        vec![
            StructField {
                field_name: Some(Ident::new(field(scale))),
                field_type: DataType::BigInt(None),
                options: None,
            },
            StructField {
                field_name: Some(Ident::new(OFFSET)),
                field_type: DataType::SmallInt(None),
                options: None,
            },
        ],
        StructBracketKind::Parentheses,
    )
}
pub fn storage_scale(kind: &DataType) -> Option<u8> {
    if let DataType::Struct(fields, _) = kind
        && fields.len() == 2
        && fields[0].field_type == DataType::BigInt(None)
        && fields[1]
            .field_name
            .as_ref()
            .is_some_and(|name| name.value == OFFSET)
        && fields[1].field_type == DataType::SmallInt(None)
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
    canonicalize(expr);
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
    canonicalize(expr);
    Ok(())
}

// Annotation may add an already present cast. Collapse only equal scales;
// conversions through another scale must retain their rounding effects.
fn canonicalize(expr: &mut Expr) {
    fn argument(expr: &Expr) -> Option<(u8, &Expr)> {
        let Expr::Function(f) = expr else { return None };
        for trying in [false, true] {
            let prefix = format!("{PREFIX}{}", if trying { "try_" } else { "cast_" });
            if let Some(scale) = f
                .name
                .to_string()
                .strip_prefix(&prefix)
                .and_then(|s| s.parse::<u8>().ok())
                && scale <= 7
                && let Ok(Some(value)) = crate::function_args::unary(f, &f.name.to_string())
            {
                return Some((scale, value));
            }
        }
        None
    }
    if let Some((outer, inner)) = argument(expr)
        && let Some((scale, _)) = argument(inner)
        && outer == scale
    {
        *expr = inner.clone();
    }
}
