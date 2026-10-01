//! FORMAT(value, format [, culture]).
//!
//! The call becomes `CAST(__msduck_format(value, format, culture) AS
//! nvarchar(4000))` before binding, so the result is typed nvarchar(4000)
//! like SQL Server's. Arguments whose types are known when the statement is
//! compiled (literals, variables, CAST and CONVERT) are checked then, with
//! SQL Server's error numbers; the native function checks the rest.
use super::{arguments, call, name, sql_error, syntax_error};
use crate::engine::Parameter;
use anyhow::{Result, bail};
use sqlparser::ast::{CastKind, CharacterLength, DataType, Expr, Value as Literal};
use std::collections::HashMap;

/// The SQL Server type name of an expression whose type is evident without
/// binding, or `None`.
pub(super) fn static_type(expr: &Expr, parameters: &HashMap<String, Parameter>) -> Option<String> {
    match expr {
        Expr::Nested(inner) => static_type(inner, parameters),
        Expr::Value(value) => Some(
            match &value.value {
                Literal::Null => "NULL",
                Literal::SingleQuotedString(_) => "varchar",
                Literal::NationalStringLiteral(_) => "nvarchar",
                Literal::HexStringLiteral(_) => "varbinary",
                Literal::Boolean(_) => "bit",
                Literal::Number(text, _) => {
                    if text.contains(['e', 'E']) {
                        "float"
                    } else if text.contains('.') || text.parse::<i32>().is_err() {
                        "numeric"
                    } else {
                        "int"
                    }
                }
                _ => return None,
            }
            .into(),
        ),
        Expr::Cast { data_type, .. }
        | Expr::Convert {
            data_type: Some(data_type),
            ..
        } => type_name(data_type),
        Expr::Identifier(ident)
            if ident.value.starts_with('@') && !ident.value.starts_with("@@") =>
        {
            type_name(&parameters.get(&ident.value.to_lowercase())?.ast_type())
        }
        Expr::Function(function) => match name(function)?.as_str() {
            "GETDATE" | "GETUTCDATE" | "CURRENT_TIMESTAMP" => Some("datetime".into()),
            "SYSDATETIME" | "SYSUTCDATETIME" => Some("datetime2".into()),
            "SYSDATETIMEOFFSET" => Some("datetimeoffset".into()),
            "NEWID" => Some("uniqueidentifier".into()),
            _ => None,
        },
        _ => None,
    }
}

/// The SQL Server name of a declared type.
pub(super) fn type_name(data_type: &DataType) -> Option<String> {
    Some(
        match data_type {
            DataType::Bit(_) => "bit",
            DataType::TinyInt(_) => "tinyint",
            DataType::SmallInt(_) => "smallint",
            DataType::Int(_) | DataType::Integer(_) => "int",
            DataType::BigInt(_) => "bigint",
            DataType::Decimal(_) | DataType::Numeric(_) | DataType::Dec(_) => "numeric",
            DataType::Float(_) | DataType::Double(_) | DataType::DoublePrecision => "float",
            DataType::Real => "real",
            DataType::Varchar(_) | DataType::CharacterVarying(_) => "varchar",
            DataType::Char(_) | DataType::Character(_) => "char",
            DataType::Nvarchar(_) => "nvarchar",
            DataType::Text => "text",
            DataType::Varbinary(_) => "varbinary",
            DataType::Binary(_) => "binary",
            DataType::Date => "date",
            DataType::Datetime(_) => "datetime",
            DataType::Time(..) => "time",
            DataType::Uuid => "uniqueidentifier",
            DataType::Custom(name, _) => {
                let name = name.to_string().to_ascii_lowercase();
                let name = name.trim_matches(['[', ']', '"']);
                return Some(match name {
                    "nchar" | "ntext" | "money" | "smallmoney" | "smalldatetime"
                    | "datetimeoffset" | "datetime2" | "uniqueidentifier" | "sql_variant"
                    | "xml" | "sysname" | "image" => {
                        if name == "sysname" {
                            "nvarchar".into()
                        } else {
                            name.to_owned()
                        }
                    }
                    _ => return None,
                });
            }
            _ => return None,
        }
        .into(),
    )
}

/// A NULL known before execution: a NULL literal, a CAST of one, or a
/// variable holding NULL.
fn null_value(expr: &Expr, parameters: &HashMap<String, Parameter>) -> bool {
    match expr {
        Expr::Nested(inner) => null_value(inner, parameters),
        Expr::Value(value) => matches!(value.value, Literal::Null),
        Expr::Cast { expr, .. } | Expr::Convert { expr, .. } => null_value(expr, parameters),
        Expr::Identifier(ident) if ident.value.starts_with('@') => parameters
            .get(&ident.value.to_lowercase())
            .is_some_and(|p| matches!(p.value, msduck_core::value::Value::Null)),
        _ => false,
    }
}

fn invalid(type_name: &str, position: usize) -> anyhow::Error {
    sql_error(
        8116,
        1,
        format!(
            "Argument data type {type_name} is invalid for argument {position} of format function."
        ),
    )
}

pub(super) fn rewrite(expr: &mut Expr, parameters: &HashMap<String, Parameter>) -> Result<()> {
    let Expr::Function(function) = expr else {
        return Ok(());
    };
    if name(function).as_deref() != Some("FORMAT") {
        return Ok(());
    }
    let Some(args) = arguments(function) else {
        bail!("unsupported FORMAT modifiers");
    };
    if !(2..=3).contains(&args.len()) {
        return Err(syntax_error(
            189,
            1,
            "The format function requires 2 to 3 arguments.",
        ));
    }
    if let Some(kind) = static_type(args[0], parameters)
        && matches!(
            kind.as_str(),
            "NULL"
                | "bit"
                | "varchar"
                | "char"
                | "nvarchar"
                | "nchar"
                | "text"
                | "ntext"
                | "varbinary"
                | "binary"
                | "image"
                | "uniqueidentifier"
                | "sql_variant"
                | "xml"
        )
    {
        return Err(invalid(&kind, 1));
    }
    for (position, arg) in args.iter().enumerate().skip(1) {
        if let Some(kind) = static_type(arg, parameters)
            && !matches!(kind.as_str(), "varchar" | "char" | "nvarchar" | "nchar")
            && !(position == 2 && kind == "NULL")
        {
            return Err(invalid(&kind, position + 1));
        }
    }
    // A constant culture is checked when the statement compiles.
    if let Some(culture) = args.get(2) {
        let text = match culture {
            Expr::Value(value) => match &value.value {
                Literal::SingleQuotedString(text) | Literal::NationalStringLiteral(text) => {
                    Some(Some(text.clone()))
                }
                Literal::Null => Some(None),
                _ => None,
            },
            _ => None,
        };
        if let Some(text) = text {
            let shown = text.clone().unwrap_or_else(|| "NULL".into());
            match text.as_deref().map(super::dotnet::culture) {
                Some(Ok(_)) => {}
                Some(Err(super::dotnet::CultureError::Unsupported)) => {
                    bail!("unsupported FORMAT culture {shown}")
                }
                _ => {
                    return Err(sql_error(
                        9818,
                        1,
                        format!(
                            "The culture parameter '{shown}' provided in the function call is not supported."
                        ),
                    ));
                }
            }
        }
    }
    let culture = match args.get(2) {
        Some(culture) => (*culture).clone(),
        // The session language is us_english.
        None => Expr::value(Literal::SingleQuotedString("en-US".into())),
    };
    // DuckDB returns NULL for a NULL argument without calling the function;
    // a NULL format means the general format, so pass an empty one.
    let format = if null_value(args[1], parameters) {
        Expr::value(Literal::SingleQuotedString(String::new()))
    } else {
        args[1].clone()
    };
    let native = call("__msduck_format", vec![args[0].clone(), format, culture]);
    *expr = Expr::Cast {
        kind: CastKind::Cast,
        expr: Box::new(native),
        data_type: DataType::Nvarchar(Some(CharacterLength::IntegerLength {
            length: 4000,
            unit: None,
        })),
        format: None,
    };
    Ok(())
}
