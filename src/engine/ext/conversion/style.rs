//! Styled CONVERT and TRY_CONVERT.
//!
//! The built-in conversions handle the styles of currency and of binary to
//! Unicode text. Every other style reaches `lower` after translation, where
//! the converted value is already a backend expression:
//!
//! - to character types, `__msduck_conversion_text` formats the value by its
//!   backend type (date/time, binary and float styles) and the ordinary
//!   width rules of the target then apply;
//! - to binary types, `__msduck_conversion_binary` reads hexadecimal text;
//! - to date/time types, `__msduck_conversion_temporal` parses the text
//!   with the style into ISO 8601, which the built-in conversion reads.
//!
//! Styles of other targets (numbers, bit, uniqueidentifier) do not change the
//! result in SQL Server and are dropped before binding.
use super::{call, format::static_type, sql_error, temporal};
use crate::engine::Parameter;
use anyhow::Result;
use sqlparser::ast::{
    BinaryLength, CastKind, CharacterLength, DataType, Expr, TimezoneInfo, Value as Literal,
};
use std::collections::HashMap;

/// What a conversion produces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Target {
    /// varchar, char, nvarchar and nchar (codes 0 to 3 for the native code).
    Character(i32),
    /// varbinary or binary: width (-1 for MAX) and whether it is fixed.
    Binary(i32, bool),
    Temporal(temporal::Target, u8),
    Other,
}

fn character_width(length: &Option<CharacterLength>) -> bool {
    !matches!(
        length,
        Some(CharacterLength::IntegerLength { length: 0, .. })
    )
}

pub(super) fn target(data_type: &DataType) -> Target {
    if let Ok(Some(scale)) = msduck_sql::datetime2_cast::scale(data_type) {
        return Target::Temporal(temporal::Target::DateTime2, scale);
    }
    if let Ok(Some(scale)) = msduck_sql::datetimeoffset_cast::scale(data_type) {
        return Target::Temporal(temporal::Target::DateTimeOffset, scale);
    }
    match data_type {
        DataType::Varchar(length) | DataType::CharacterVarying(length)
            if character_width(length) =>
        {
            Target::Character(0)
        }
        DataType::Char(length) | DataType::Character(length) if character_width(length) => {
            Target::Character(1)
        }
        DataType::Nvarchar(_) => Target::Character(2),
        DataType::Custom(name, _) if name.to_string().eq_ignore_ascii_case("nchar") => {
            Target::Character(3)
        }
        DataType::Varbinary(length) => Target::Binary(
            match length {
                Some(BinaryLength::Max) => -1,
                Some(BinaryLength::IntegerLength { length }) => *length as i32,
                None => 30,
            },
            false,
        ),
        DataType::Binary(length) => Target::Binary(length.map_or(30, |n| n as i32), true),
        DataType::Date => Target::Temporal(temporal::Target::Date, 0),
        DataType::Datetime(_) => Target::Temporal(temporal::Target::DateTime, 3),
        DataType::Custom(name, _) if name.to_string().eq_ignore_ascii_case("smalldatetime") => {
            Target::Temporal(temporal::Target::SmallDateTime, 0)
        }
        DataType::Time(scale, TimezoneInfo::None) => {
            Target::Temporal(temporal::Target::Time, scale.map_or(7, |s| s.min(7) as u8))
        }
        _ => Target::Other,
    }
}

/// The character length of a target type: -1 for MAX, 30 when omitted.
fn character_length(data_type: &DataType) -> i64 {
    let length = |length: &Option<CharacterLength>| match length {
        Some(CharacterLength::IntegerLength { length, .. }) => *length as i64,
        Some(CharacterLength::Max) => -1,
        None => 30,
    };
    match data_type {
        DataType::Varchar(l)
        | DataType::CharacterVarying(l)
        | DataType::Char(l)
        | DataType::Character(l)
        | DataType::Nvarchar(l) => length(l),
        DataType::Custom(_, args) => args.first().and_then(|a| a.parse().ok()).unwrap_or(30),
        _ => -1,
    }
}

fn character_name(code: i32) -> &'static str {
    ["varchar", "char", "nvarchar", "nchar"][code as usize]
}

/// A character literal: its type name and text.
fn literal_text(expr: &Expr) -> Option<(&'static str, String)> {
    match expr {
        Expr::Nested(inner) => literal_text(inner),
        Expr::Value(value) => match &value.value {
            Literal::SingleQuotedString(text) => Some(("varchar", text.clone())),
            Literal::NationalStringLiteral(text) => Some(("nvarchar", text.clone())),
            _ => None,
        },
        _ => None,
    }
}

/// The declared scale of a time value known before binding.
fn time_scale(expr: &Expr, parameters: &HashMap<String, Parameter>) -> Option<u64> {
    let data_type = match expr {
        Expr::Nested(inner) => return time_scale(inner, parameters),
        Expr::Cast { data_type, .. }
        | Expr::Convert {
            data_type: Some(data_type),
            ..
        } => data_type.clone(),
        Expr::Identifier(ident) if ident.value.starts_with('@') => {
            parameters.get(&ident.value.to_lowercase())?.ast_type()
        }
        _ => return None,
    };
    match data_type {
        DataType::Time(scale, TimezoneInfo::None) => Some(scale.unwrap_or(7).min(7)),
        _ => None,
    }
}

fn constant_style(expr: &Expr) -> Option<i32> {
    match expr {
        Expr::Nested(inner) => constant_style(inner),
        Expr::Value(value) => match &value.value {
            Literal::Number(text, _) => text.parse().ok(),
            _ => None,
        },
        _ => None,
    }
}

/// The source type of a date/time value known before binding.
fn temporal_source(name: &str) -> Option<temporal::Source> {
    Some(match name {
        "datetime" => temporal::Source::DateTime,
        "smalldatetime" => temporal::Source::SmallDateTime,
        "date" => temporal::Source::Date,
        "time" => temporal::Source::Time(7),
        "datetime2" => temporal::Source::DateTime2(7),
        "datetimeoffset" => temporal::Source::DateTimeOffset(7),
        _ => return None,
    })
}

/// Check styles whose source type is evident before binding, with SQL
/// Server's compile-time errors, and drop styles that cannot matter.
pub(super) fn validate(expr: &mut Expr, parameters: &HashMap<String, Parameter>) -> Result<()> {
    let Expr::Convert {
        expr: value,
        data_type: Some(data_type),
        styles,
        is_try,
        ..
    } = expr
    else {
        return Ok(());
    };
    if styles.len() != 1 {
        return Ok(());
    }
    let target = target(data_type);
    if target == Target::Other {
        // SQL Server ignores the style of numeric, bit and other targets.
        styles.clear();
        return Ok(());
    }
    // The fraction digits of a time value follow its declared scale, which
    // the backend value does not carry: pass it along when it is evident.
    if matches!(target, Target::Character(_))
        && let Some(scale) = time_scale(value, parameters)
    {
        styles.push(msduck_sql::expr::number(scale));
    }
    if *is_try {
        return Ok(());
    }
    if let (Some(style), Some(text)) =
        (styles.first().and_then(constant_style), literal_text(value))
    {
        match target {
            Target::Temporal(kind, _) => match temporal::parse(&text.1, style, kind) {
                Err(temporal::ParseError::Syntax) if kind == temporal::Target::SmallDateTime => {
                    return Err(sql_error(
                        295,
                        3,
                        "Conversion failed when converting character string to smalldatetime data type.",
                    ));
                }
                Err(temporal::ParseError::Syntax) => {
                    return Err(sql_error(241, 1, crate::datetime2_cast::CONVERSION));
                }
                Err(temporal::ParseError::Range) => {
                    return Err(sql_error(
                        242,
                        3,
                        format!(
                            "The conversion of a varchar data type to a {} data type resulted in an out-of-range value.",
                            if kind == temporal::Target::SmallDateTime {
                                "smalldatetime"
                            } else {
                                "datetime"
                            }
                        ),
                    ));
                }
                Ok(_) => {}
            },
            Target::Binary(_, fixed) => {
                if let Err(super::binary::Error::Syntax) =
                    super::binary::from_text(&text.1, style, None)
                {
                    return Err(sql_error(
                        8114,
                        5,
                        format!(
                            "Error converting data type {} to {}.",
                            text.0,
                            if fixed { "binary" } else { "varbinary" }
                        ),
                    ));
                }
            }
            _ => {}
        }
    }
    let (Some(style), Some(source)) = (
        styles.first().and_then(constant_style),
        static_type(value, parameters),
    ) else {
        return Ok(());
    };
    match target {
        Target::Character(code) => {
            if let Some(kind) = temporal_source(&source) {
                let sample = temporal::Value {
                    local: crate::datetime2::DateTime2::from_ticks(0).expect("valid ticks"),
                    offset: matches!(kind, temporal::Source::DateTimeOffset(_)).then_some(0),
                };
                match temporal::format(sample, kind, style) {
                    Err(temporal::FormatError::InvalidStyle) => {
                        return Err(sql_error(
                            281,
                            1,
                            format!(
                                "{style} is not a valid style number when converting from {source} to a character string."
                            ),
                        ));
                    }
                    Err(temporal::FormatError::NotApplicable) => {
                        return Err(sql_error(
                            8114,
                            5,
                            format!(
                                "Error converting data type {source} to {}.",
                                character_name(code)
                            ),
                        ));
                    }
                    _ => {}
                }
            }
            if matches!(source.as_str(), "varbinary" | "binary") && !(0..=2).contains(&style) {
                return Err(sql_error(
                    9809,
                    1,
                    format!(
                        "The style {style} is not supported for conversions from {source} to {}.",
                        character_name(code)
                    ),
                ));
            }
        }
        Target::Binary(_, fixed)
            if matches!(source.as_str(), "varchar" | "char" | "nvarchar" | "nchar")
                && !(0..=2).contains(&style) =>
        {
            return Err(sql_error(
                9809,
                1,
                format!(
                    "The style {style} is not supported for conversions from {source} to {}.",
                    if fixed { "binary" } else { "varbinary" }
                ),
            ));
        }
        _ => {}
    }
    Ok(())
}

fn number(n: impl ToString) -> Expr {
    msduck_sql::expr::number(n)
}

fn boolean(value: bool) -> Expr {
    Expr::value(Literal::Boolean(value))
}

fn style_argument(style: &Expr) -> Expr {
    Expr::Cast {
        kind: CastKind::Cast,
        expr: Box::new(style.clone()),
        data_type: DataType::Int(None),
        format: None,
    }
}

/// Lower a styled conversion the built-in rules left in place.
pub(super) fn lower(expr: &mut Expr) -> Result<(), String> {
    let Expr::Convert {
        is_try,
        expr: value,
        data_type: Some(data_type),
        charset: None,
        target_before_value,
        styles,
    } = expr
    else {
        return Ok(());
    };
    let (style, scale) = match styles.as_slice() {
        [style] => (style, number(-1)),
        [style, scale] => (style, scale.clone()),
        _ => return Ok(()),
    };
    let (is_try, value, data_type, target_before_value) = (
        *is_try,
        (**value).clone(),
        data_type.clone(),
        *target_before_value,
    );
    let style = style_argument(style);
    let width = character_length(&data_type);
    *expr = match target(&data_type) {
        Target::Character(code) => {
            let text = call(
                "__msduck_conversion_text",
                vec![
                    value,
                    style,
                    number(code),
                    boolean(is_try),
                    number(width),
                    scale,
                ],
            );
            let mut converted = Expr::Convert {
                is_try,
                expr: Box::new(text.clone()),
                data_type: Some(data_type),
                charset: None,
                target_before_value,
                styles: vec![],
            };
            crate::varchar::lower(&mut converted)?;
            crate::nvarchar::lower(&mut converted)?;
            crate::ncharacter::lower(&mut converted)?;
            if matches!(converted, Expr::Convert { .. }) {
                // nvarchar(max): no width to apply.
                text
            } else {
                converted
            }
        }
        Target::Binary(width, fixed) => call(
            "__msduck_conversion_binary",
            vec![value, style, number(width), boolean(fixed), boolean(is_try)],
        ),
        Target::Temporal(kind, scale) => {
            let code = match kind {
                temporal::Target::Date => 0,
                temporal::Target::DateTime => 1,
                temporal::Target::SmallDateTime => 2,
                temporal::Target::DateTime2 => 3,
                temporal::Target::DateTimeOffset => 4,
                temporal::Target::Time => 5,
            };
            let text = call(
                "__msduck_conversion_temporal",
                vec![value, style, number(code), boolean(is_try)],
            );
            match kind {
                temporal::Target::Date => call("__msduck_cast_date", vec![text]),
                temporal::Target::DateTime | temporal::Target::SmallDateTime => Expr::Cast {
                    kind: CastKind::Cast,
                    expr: Box::new(text),
                    data_type: DataType::Timestamp(None, TimezoneInfo::None),
                    format: None,
                },
                temporal::Target::DateTime2 => call(
                    &format!("{}cast_{scale}", msduck_sql::datetime2_cast::PREFIX),
                    vec![text],
                ),
                temporal::Target::DateTimeOffset => call(
                    &format!("{}cast_{scale}", msduck_sql::datetimeoffset_cast::PREFIX),
                    vec![text],
                ),
                temporal::Target::Time => call(
                    "__msduck_time_round",
                    vec![
                        call("__msduck_cast_time", vec![text]),
                        number(10u64.pow(9 - u32::from(scale))),
                    ],
                ),
            }
        }
        Target::Other => {
            return Err(format!("unsupported CONVERT style for {data_type}"));
        }
    };
    Ok(())
}
