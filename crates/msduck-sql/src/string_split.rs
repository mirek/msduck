//! Deterministic STRING_SPLIT binding and UTF-16 token rules.
//!
//! The caller supplies declared argument types and runtime UTF-16 units. This
//! module neither queries a catalog nor infers result metadata from row values.
use sqlparser::ast::{
    DataType, Expr, FunctionArg, FunctionArgExpr, TableFactor, UnaryOperator, Value,
};

/// This pure adapter covers bounded values; larger SQL Server MAX values need
/// a streaming root adapter rather than eager token materialization.
pub const MAX_CAPTURED_UNITS: usize = 1_048_576;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub number: u32,
    pub state: u8,
    pub class: u8,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Sql(Diagnostic),
    Unsupported(&'static str),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeclaredType {
    Character {
        unicode: bool,
        max_bytes: u16,
        nullable: bool,
        collation: Option<String>,
    },
    Other(String),
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValueColumn {
    pub unicode: bool,
    pub max_bytes: u16,
    pub nullable: bool,
    pub collation: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    /// Unknown declarations remain unknown until catalog/parameter binding.
    pub value: Option<ValueColumn>,
    pub ordinal: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub units: Vec<u16>,
    /// SQL Server's ordinal column is a non-null BIGINT, starting at one.
    pub ordinal: Option<u64>,
}

fn sql(number: u32, state: u8, message: impl Into<String>) -> Error {
    Error::Sql(Diagnostic {
        number,
        state,
        class: 16,
        message: message.into(),
    })
}

fn invalid_type(kind: &str, position: u8) -> Error {
    sql(
        8116,
        1,
        format!(
            "Argument data type {kind} is invalid for argument {position} of string_split function."
        ),
    )
}

pub fn is_string_split(factor: &TableFactor) -> bool {
    matches!(factor, TableFactor::Table { name, args: Some(_), .. }
        if name.0.len() == 1 && name.0[0].as_ident().is_some_and(|id| id.value.eq_ignore_ascii_case("string_split")))
}

/// Bind the known SQL Server table-function shape. `source` and `separator`
/// come from explicit declarations, never from a runtime value sample.
pub fn bind(
    factor: &TableFactor,
    source: &DeclaredType,
    separator: &DeclaredType,
) -> Result<Option<Binding>, Error> {
    if !is_string_split(factor) {
        return Ok(None);
    }
    let TableFactor::Table {
        args: Some(arguments),
        ..
    } = factor
    else {
        unreachable!()
    };
    if arguments.args.len() < 2 {
        return Err(sql(
            313,
            3,
            "An insufficient number of arguments were supplied for the procedure or function STRING_SPLIT.",
        ));
    }
    if arguments.args.len() > 3 {
        return Err(sql(
            8144,
            3,
            "Procedure or function STRING_SPLIT has too many arguments specified.",
        ));
    }
    if arguments.settings.is_some() {
        return Err(Error::Unsupported("STRING_SPLIT settings are not captured"));
    }
    let expressions = arguments
        .args
        .iter()
        .map(|arg| match arg {
            FunctionArg::Unnamed(FunctionArgExpr::Expr(expr)) => Ok(expr),
            _ => Err(Error::Unsupported(
                "STRING_SPLIT requires positional scalar arguments",
            )),
        })
        .collect::<Result<Vec<_>, _>>()?;
    if let DeclaredType::Other(kind) = source {
        return Err(invalid_type(kind, 1));
    }
    if let DeclaredType::Other(kind) = separator {
        return Err(invalid_type(kind, 2));
    }
    let ordinal = if expressions.len() == 3 {
        ordinal(expressions[2])?
    } else {
        false
    };
    let value = match (source, separator) {
        (
            DeclaredType::Character {
                unicode: source_unicode,
                max_bytes,
                nullable,
                collation,
            },
            DeclaredType::Character {
                unicode: separator_unicode,
                ..
            },
        ) => {
            let unicode = *source_unicode || *separator_unicode;
            let max_bytes = if unicode && !source_unicode && *max_bytes != u16::MAX {
                max_bytes.checked_mul(2).ok_or(Error::Unsupported(
                    "STRING_SPLIT output width exceeds TDS limit",
                ))?
            } else {
                *max_bytes
            };
            Some(ValueColumn {
                unicode,
                max_bytes,
                nullable: *nullable,
                collation: collation.clone(),
            })
        }
        _ => None,
    };
    Ok(Some(Binding { value, ordinal }))
}

fn ordinal(expr: &Expr) -> Result<bool, Error> {
    match expr {
        Expr::Nested(inner) => ordinal(inner),
        Expr::Cast {
            expr,
            data_type: DataType::Bit(_),
            ..
        } => ordinal(expr),
        Expr::Value(value) => match &value.value {
            Value::Null => Ok(false),
            Value::Number(value, _) if value.contains('.') => Err(invalid_type("numeric", 3)),
            Value::Number(value, _) => ordinal_integer(value),
            _ => Err(Error::Unsupported(
                "STRING_SPLIT ordinal literal type is not captured",
            )),
        },
        Expr::UnaryOp {
            op: UnaryOperator::Minus,
            expr,
        } => {
            if let Expr::Value(value) = expr.as_ref()
                && let Value::Number(value, _) = &value.value
            {
                return ordinal_integer(&format!("-{value}"));
            }
            Err(Error::Unsupported(
                "STRING_SPLIT ordinal unary expression is not captured",
            ))
        }
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) => Err(sql(
            8748,
            1,
            "The enable_ordinal argument for string_split only supports constant values (not variables or columns).",
        )),
        _ => Err(Error::Unsupported(
            "STRING_SPLIT ordinal expression is not captured",
        )),
    }
}

fn ordinal_integer(value: &str) -> Result<bool, Error> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        _ if value.parse::<i64>().is_ok() => Err(sql(
            4199,
            1,
            format!("Argument value {value} is invalid for argument 3 of string_split function."),
        )),
        _ => Err(Error::Unsupported(
            "STRING_SPLIT ordinal integer range is not captured",
        )),
    }
}

/// Split already-evaluated UTF-16 units. The caller converts ANSI bytes using
/// the declared code page; this function preserves even isolated surrogate units.
pub fn split_units(
    input: Option<&[u16]>,
    separator: Option<&[u16]>,
    ordinal: bool,
) -> Result<Vec<Token>, Error> {
    let Some([delimiter]) = separator else {
        return Err(sql(
            214,
            11,
            "Procedure expects parameter 'separator' of type 'nchar(1)/nvarchar(1)'.",
        ));
    };
    let Some(input) = input else {
        return Ok(Vec::new());
    };
    if input.len() > MAX_CAPTURED_UNITS {
        return Err(Error::Unsupported(
            "STRING_SPLIT input exceeds deterministic materialization bound",
        ));
    }
    let mut rows = Vec::new();
    let mut begin = 0;
    for (index, unit) in input.iter().enumerate() {
        if unit == delimiter {
            rows.push(Token {
                units: input[begin..index].to_vec(),
                ordinal: ordinal.then_some(rows.len() as u64 + 1),
            });
            begin = index + 1;
        }
    }
    rows.push(Token {
        units: input[begin..].to_vec(),
        ordinal: ordinal.then_some(rows.len() as u64 + 1),
    });
    Ok(rows)
}
