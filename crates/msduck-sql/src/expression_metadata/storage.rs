//! Deterministic expression metadata rules shared by binding and lowering.
use super::temporal;
use crate::parameter::Parameter;
use sqlparser::ast::*;
use std::collections::HashMap;

pub fn string_escape_call(expr: &Expr) -> bool {
    matches!(expr, Expr::Function(f) if matches!(f.name.to_string().to_ascii_lowercase().as_str(), "string_escape" | "__msduck_string_escape"))
}

pub fn numeric_literal_type(number: &str) -> Option<DataType> {
    if number.contains(['e', 'E']) {
        return Some(DataType::Double(ExactNumberInfo::None));
    }
    if !number.contains('.') && number.parse::<i32>().is_ok() {
        return Some(DataType::Int(None));
    }
    let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
    if !whole
        .bytes()
        .chain(fraction.bytes())
        .all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let scale = fraction.len() as u64;
    let precision = (whole.trim_start_matches('0').len() as u64 + scale).max(1);
    (precision <= 38).then_some(DataType::Decimal(ExactNumberInfo::PrecisionAndScale(
        precision,
        scale as i64,
    )))
}

pub fn kind(
    expr: &Expr,
    parameters: &HashMap<String, Parameter>,
    column: &impl Fn(&Expr) -> Option<DataType>,
) -> Option<DataType> {
    if super::conditional::candidate(expr) {
        let characters = super::conditional::values(expr)
            .into_iter()
            .filter(|value| !super::conditional::literal_null(value))
            .map(|value| {
                match crate::sql_type::declaration(&kind(value, parameters, column)?).ok()? {
                    msduck_core::types::Type::Character(kind) => Some(kind),
                    _ => None,
                }
            })
            .collect::<Option<Vec<_>>>();
        if let Some(values) = characters
            && let Some(common) = crate::result_types::common_character(values.into_iter())
        {
            return Some(crate::sql_type::ast(msduck_core::types::Type::Character(
                common,
            )));
        }
        let mut values = super::conditional::values(expr)
            .into_iter()
            .filter(|value| !super::conditional::literal_null(value));
        let first = kind(values.next()?, parameters, column)?;
        let common = values.try_fold(first, |left, value| {
            super::arithmetic::set_type(&left, &kind(value, parameters, column)?)
        })?;
        if matches!(common, DataType::Decimal(_) | DataType::Numeric(_)) {
            return Some(common);
        }
    }
    match expr {
        Expr::BinaryOp { left, op, right }
            if matches!(
                op,
                BinaryOperator::Plus
                    | BinaryOperator::Minus
                    | BinaryOperator::Multiply
                    | BinaryOperator::Modulo
                    | BinaryOperator::Divide
            ) =>
        {
            super::arithmetic::decimal_type(
                op,
                &kind(left, parameters, column)?,
                &kind(right, parameters, column)?,
            )
        }
        Expr::Cast { data_type, .. }
        | Expr::Convert {
            data_type: Some(data_type),
            ..
        } => Some(data_type.clone()),
        Expr::Nested(value) | Expr::Collate { expr: value, .. } => kind(value, parameters, column),
        Expr::UnaryOp { op, expr } if matches!(op, UnaryOperator::Plus | UnaryOperator::Minus) => {
            let source = kind(expr, parameters, column)?;
            if *op == UnaryOperator::Minus
                && matches!(source, DataType::TinyInt(_) | DataType::UTinyInt)
            {
                Some(DataType::SmallInt(None))
            } else if fixed_width(&source).is_some() {
                Some(source)
            } else {
                None
            }
        }
        Expr::Identifier(id) if id.value.starts_with('@') => parameters
            .get(&id.value.to_lowercase())
            .map(|p| p.ast_type())
            .or_else(|| column(expr)),
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) => column(expr),
        Expr::Value(value) => Some(match &value.value {
            Value::SingleQuotedString(s) => {
                DataType::Varchar(Some(length(s.chars().count(), 8000)))
            }
            Value::NationalStringLiteral(s) => {
                DataType::Nvarchar(Some(length(s.encode_utf16().count(), 4000)))
            }
            Value::HexStringLiteral(s) => DataType::Varbinary(Some(if s.len() / 2 > 8000 {
                BinaryLength::Max
            } else {
                BinaryLength::IntegerLength {
                    length: s.len() as u64 / 2,
                }
            })),
            Value::Number(n, _) => numeric_literal_type(n)?,
            Value::Null => DataType::Int(None),
            _ => return None,
        }),
        value if retained_argument(value).is_some() => {
            variable_character(kind(retained_argument(value)?, parameters, column)?)
        }
        value if string_escape_call(value) => Some(DataType::Nvarchar(Some(CharacterLength::Max))),
        value if crate::for_json::unicode_result(value) => {
            Some(DataType::Nvarchar(Some(CharacterLength::Max)))
        }
        Expr::Function(f) => {
            if matches!(
                f.name.to_string().to_ascii_uppercase().as_str(),
                "MIN" | "MAX"
            ) && crate::aggregate::validate(f).is_ok()
                && let FunctionArguments::List(args) = &f.args
                && let [FunctionArg::Unnamed(FunctionArgExpr::Expr(value))] = args.args.as_slice()
                && let Some(source) = kind(value, parameters, column)
                && crate::character_storage::is_character(&source)
            {
                return Some(source);
            }
            if let Some(kind) = crate::replicate::result_type(expr, parameters, column)
                .or_else(|| crate::left_right::result_type(expr, parameters, column))
            {
                return Some(crate::sql_type::ast(msduck_core::types::Type::Character(
                    kind,
                )));
            }
            if let Some(result) = crate::session_function::error_type(f) {
                return Some(result);
            }
            if let Some(result) = decimal_aggregate(f, parameters, column) {
                return Some(result);
            }
            if let Some(scale) = temporal::timefromparts_scale(f) {
                Some(DataType::Time(Some(u64::from(scale)), TimezoneInfo::None))
            } else if let Some(scale) = temporal::datetime2fromparts_scale(f) {
                Some(DataType::Custom(
                    ObjectName::from(vec![Ident::new("datetime2")]),
                    vec![scale.to_string()],
                ))
            } else {
                temporal::datetimeoffsetfromparts_scale(f).map(|scale| {
                    DataType::Custom(
                        ObjectName::from(vec![Ident::new("datetimeoffset")]),
                        vec![scale.to_string()],
                    )
                })
            }
        }
        _ => None,
    }
}

/// Preserve DECIMAL aggregate declarations through derived tables and nested
/// grouped/window aggregates. Other aggregate families have separate rules.
pub fn decimal_aggregate(
    function: &Function,
    parameters: &HashMap<String, Parameter>,
    column: &impl Fn(&Expr) -> Option<DataType>,
) -> Option<DataType> {
    let name = function.name.to_string().to_ascii_lowercase();
    let lowered_average = name.starts_with("__msduck_avg_decimal_");
    if !lowered_average && !matches!(name.as_str(), "avg" | "sum" | "min" | "max") {
        return None;
    }
    let FunctionArguments::List(args) = &function.args else {
        return None;
    };
    let [FunctionArg::Unnamed(FunctionArgExpr::Expr(value))] = args.args.as_slice() else {
        return None;
    };
    let input = kind(value, parameters, column)?;
    let msduck_core::types::Type::Decimal(decimal) = crate::sql_type::declaration(&input).ok()?
    else {
        return None;
    };
    if matches!(name.as_str(), "min" | "max") {
        return Some(input);
    }
    let scale = if name == "avg" || lowered_average {
        decimal.scale().max(6)
    } else {
        decimal.scale()
    };
    Some(DataType::Decimal(ExactNumberInfo::PrecisionAndScale(
        38,
        i64::from(scale),
    )))
}

pub fn retained_argument(expr: &Expr) -> Option<&Expr> {
    match expr {
        Expr::Trim { expr, .. } => Some(expr),
        Expr::Function(f)
            if matches!(
                f.name.to_string().to_ascii_uppercase().as_str(),
                "UPPER" | "LOWER" | "LTRIM" | "RTRIM"
            ) =>
        {
            let FunctionArguments::List(args) = &f.args else {
                return None;
            };
            let FunctionArg::Unnamed(FunctionArgExpr::Expr(value)) = args.args.first()? else {
                return None;
            };
            Some(value)
        }
        _ => None,
    }
}

fn variable_character(kind: DataType) -> Option<DataType> {
    Some(match kind {
        DataType::Varchar(n)
        | DataType::Char(n)
        | DataType::Character(n)
        | DataType::CharacterVarying(n)
        | DataType::CharVarying(n) => DataType::Varchar(n),
        DataType::Nvarchar(n) => DataType::Nvarchar(n),
        DataType::Custom(name, args) if name.to_string().eq_ignore_ascii_case("nchar") => {
            DataType::Nvarchar(Some(CharacterLength::IntegerLength {
                length: match args.as_slice() {
                    [] => 30,
                    [n] => n.parse().ok()?,
                    _ => return None,
                },
                unit: None,
            }))
        }
        _ => return None,
    })
}

pub fn fixed_width(kind: &DataType) -> Option<u8> {
    crate::sql_type::declaration(kind).ok()?.scalar_bytes()
}

pub fn catalog_scalar(id: i32, precision: i32, scale: i32) -> Option<DataType> {
    Some(match id {
        40 => DataType::Date,
        61 => DataType::Datetime(None),
        58 => DataType::Custom(ObjectName::from(vec![Ident::new("smalldatetime")]), vec![]),
        106 | 108 => DataType::Decimal(ExactNumberInfo::PrecisionAndScale(
            precision as u64,
            i64::from(scale),
        )),
        59 => DataType::Real,
        62 => DataType::Float(ExactNumberInfo::Precision(precision as u64)),
        60 | 122 => DataType::Custom(
            ObjectName::from(vec![Ident::new(if id == 60 {
                "money"
            } else {
                "smallmoney"
            })]),
            vec![],
        ),
        _ => return None,
    })
}

fn length(n: usize, max: usize) -> CharacterLength {
    if n > max {
        CharacterLength::Max
    } else {
        CharacterLength::IntegerLength {
            length: n as u64,
            unit: None,
        }
    }
}
