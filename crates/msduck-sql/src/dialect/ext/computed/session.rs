//! Session functions in stored column DEFAULTs.
//!
//! SQL Server evaluates a DEFAULT for each insert, in the inserting session.
//! The root adapter mirrors each session's login, client names and
//! `SESSION_CONTEXT` values into DuckDB variables of that session's
//! connection (variables are per connection). This module rewrites the
//! session functions of a DEFAULT into reads of those variables, so DuckDB
//! evaluates the stored default with the inserting session's values.
//!
//! - `SUSER_SNAME()`, `SUSER_NAME()`, `SYSTEM_USER` and `ORIGINAL_LOGIN()`
//!   read the login; `HOST_NAME()` and `APP_NAME()` the LOGIN7 client names.
//!   All are nullable `nvarchar(128)`.
//! - `SESSION_CONTEXT(N'key')` is a `sql_variant`. Inside an explicit
//!   `CAST`/`CONVERT` it converts from its base type, so the conversion is
//!   applied to each supported base type in turn and the stored kind selects
//!   one. A bare value converts implicitly to the column type, which SQL
//!   Server refuses with 257 when the table is created; so do ISNULL,
//!   COALESCE, NULLIF, IIF and CASE results built from it.
//! - `SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'key'), 'property')` with a
//!   constant property reads the stored kind (and, for nvarchar, the declared
//!   and total byte lengths) in its base type, so DEFAULT conditions compare
//!   and convert it. `SESSION_CONTEXT(...) IS [NOT] NULL` tests the kind.
use anyhow::{Result, bail};
use msduck_core::diagnostic::SqlError;
use sqlparser::ast::*;
use std::{collections::HashMap, ops::ControlFlow};

use crate::session_function::{self as rules, ContextValue, VariantFunction};

/// The login name.
pub const LOGIN: &str = "__msduck_session_login";
/// The LOGIN7 host name.
pub const HOST: &str = "__msduck_session_host";
/// The LOGIN7 application name.
pub const APP: &str = "__msduck_session_app";
const CONTEXT: &str = "__msduck_session_context_";

/// The variables holding one `SESSION_CONTEXT` key's value text and its base
/// type name. Keys that `keys_match` treats as the same share them.
pub fn context_variables(key: &str) -> (String, String) {
    let (lower, last) = rules::key_identity(key);
    let mut bytes = lower.into_bytes();
    bytes.push(0);
    if let Some(last) = last {
        bytes.extend(last.to_string().bytes());
    }
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    (format!("{CONTEXT}{hex}"), format!("{CONTEXT}{hex}_kind"))
}

/// The variables holding one `SESSION_CONTEXT` key's nvarchar declared
/// byte length and its `SQL_VARIANT_PROPERTY(..., 'TotalBytes')`. Only
/// nvarchar values set them; integer base types have fixed lengths.
pub fn context_length_variables(key: &str) -> (String, String) {
    let (value, _) = context_variables(key);
    (format!("{value}_max"), format!("{value}_total"))
}

/// The collation SQL Server reports for an nvarchar context value: the
/// server collation, which msduck reports everywhere.
pub const COLLATION: &str = "SQL_Latin1_General_CP1_CI_AS";

/// A `SQL_VARIANT_PROPERTY` result of a context value, in its base type:
/// `nvarchar(128)` text or `int`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Property {
    Text(&'static str),
    Int(i64),
}

/// `SQL_VARIANT_PROPERTY` of a value of base type `kind`, given its nvarchar
/// declared byte length and total bytes. `None` is NULL, including unknown
/// property names. Matches reference/gaps-computed.json.
pub fn property_of(kind: &'static str, property: &str, lengths: (i64, i64)) -> Option<Property> {
    let (max_length, precision, total) = match kind {
        "nvarchar" => (lengths.0, 0, lengths.1),
        "bit" => (1, 1, 3),
        "tinyint" => (1, 3, 3),
        "smallint" => (2, 5, 4),
        "int" => (4, 10, 6),
        "bigint" => (8, 19, 10),
        _ => return None,
    };
    Some(match property.trim_end().to_ascii_lowercase().as_str() {
        "basetype" => Property::Text(kind),
        "precision" => Property::Int(precision),
        "scale" => Property::Int(0),
        "maxlength" => Property::Int(max_length),
        "totalbytes" => Property::Int(total),
        "collation" if kind == "nvarchar" => Property::Text(COLLATION),
        _ => return None,
    })
}

/// `SQL_VARIANT_PROPERTY` of a stored context value.
pub fn context_property(value: &ContextValue, property: &str) -> Option<Property> {
    let (kind, _) = context_value(value)?;
    property_of(kind, property, context_lengths(value).unwrap_or((0, 0)))
}

/// An nvarchar value's declared byte length and its total bytes as a
/// sql_variant (eight bytes of header and collation, then UTF-16 data).
pub fn context_lengths(value: &ContextValue) -> Option<(i64, i64)> {
    match value {
        ContextValue::NVarChar { text, max_bytes } => Some((
            i64::from(*max_bytes),
            8 + 2 * text.encode_utf16().count() as i64,
        )),
        _ => None,
    }
}

/// A stored value as the variables hold it: its base type name and its
/// text. `None` for NULL, which leaves both variables unset.
pub fn context_value(value: &ContextValue) -> Option<(&'static str, String)> {
    Some(match value {
        ContextValue::Null => return None,
        ContextValue::Bit(value) => ("bit", if *value { "1" } else { "0" }.into()),
        ContextValue::TinyInt(value) => ("tinyint", value.to_string()),
        ContextValue::SmallInt(value) => ("smallint", value.to_string()),
        ContextValue::Int(value) => ("int", value.to_string()),
        ContextValue::BigInt(value) => ("bigint", value.to_string()),
        ContextValue::NVarChar { text, .. } => ("nvarchar", text.clone()),
    })
}

/// Base types a context value can have, with the declaration its text is
/// read back as. sp_set_session_context values are at most 8000 bytes.
fn base_types() -> [(&'static str, DataType); 6] {
    [
        (
            "nvarchar",
            DataType::Nvarchar(Some(CharacterLength::IntegerLength {
                length: 4000,
                unit: None,
            })),
        ),
        ("bit", DataType::Bit(None)),
        ("tinyint", DataType::TinyInt(None)),
        ("smallint", DataType::SmallInt(None)),
        ("int", DataType::Int(None)),
        ("bigint", DataType::BigInt(None)),
    ]
}

fn variable(name: &str) -> Expr {
    crate::expr::unary_function(
        "getvariable",
        Expr::Value(Value::SingleQuotedString(name.into()).into()),
    )
}

fn login_name_type() -> DataType {
    DataType::Nvarchar(Some(CharacterLength::IntegerLength {
        length: 128,
        unit: None,
    }))
}

/// A name variable as a nullable `nvarchar(128)`.
pub fn name_value(name: &str) -> Expr {
    Expr::Cast {
        kind: CastKind::Cast,
        expr: Box::new(variable(name)),
        data_type: login_name_type(),
        format: None,
    }
}

fn function_name(function: &Function) -> Option<&str> {
    match function.name.0.as_slice() {
        [ObjectNamePart::Identifier(id)] if id.quote_style.is_none() => Some(&id.value),
        _ => None,
    }
}

fn no_arguments(function: &Function) -> bool {
    match &function.args {
        FunctionArguments::List(list) => list.args.is_empty() && list.clauses.is_empty(),
        FunctionArguments::None => true,
        FunctionArguments::Subquery(_) => false,
    }
}

/// `HOST_NAME()` or `APP_NAME()`: the variable holding its value.
pub fn client_function(function: &Function) -> Option<&'static str> {
    let name = function_name(function)?;
    let variable = if name.eq_ignore_ascii_case("HOST_NAME") {
        HOST
    } else if name.eq_ignore_ascii_case("APP_NAME") {
        APP
    } else {
        return None;
    };
    (no_arguments(function) && function.over.is_none()).then_some(variable)
}

/// The login and client-name functions: the variable holding the value.
fn name_function(expr: &Expr) -> Option<&'static str> {
    match expr {
        Expr::Function(function) => {
            if let Some(variable) = client_function(function) {
                return Some(variable);
            }
            let name = function_name(function)?;
            let login =
                rules::is_login_name(function) || name.eq_ignore_ascii_case("ORIGINAL_LOGIN");
            (login && no_arguments(function) && function.over.is_none()).then_some(LOGIN)
        }
        Expr::Identifier(id) if id.quote_style.is_none() && rules::is_system_user(&id.value) => {
            Some(LOGIN)
        }
        _ => None,
    }
}

/// `HOST_NAME()` and `APP_NAME()` anywhere: lower to the session variables.
/// The value is read when DuckDB binds the statement, so views and defaults
/// see the session that uses them.
pub fn lower_client_name(expr: &mut Expr) {
    if let Expr::Function(function) = expr
        && let Some(variable) = client_function(function)
    {
        *expr = name_value(variable);
    }
}

fn session_context(expr: &Expr) -> Option<&Function> {
    match expr {
        Expr::Function(function)
            if rules::variant_function(function) == Some(VariantFunction::SessionContext) =>
        {
            Some(function)
        }
        Expr::Nested(inner) => session_context(inner),
        _ => None,
    }
}

/// The key of a `SESSION_CONTEXT(key)` call in a stored definition, which
/// must be a constant. `None` is a NULL key.
fn context_key(function: &Function) -> Result<Option<String>> {
    let FunctionArguments::List(list) = &function.args else {
        bail!("unsupported arguments in {function}");
    };
    let [FunctionArg::Unnamed(FunctionArgExpr::Expr(argument))] = list.args.as_slice() else {
        bail!(SqlError::syntax(
            174,
            1,
            "The session_context function requires 1 argument(s)."
        ));
    };
    if function.over.is_some() || function.filter.is_some() || !list.clauses.is_empty() {
        bail!("unsupported modifiers in {function}");
    }
    let argument = rules::name_argument(argument, &HashMap::new()).map_err(|_| {
        anyhow::anyhow!("unsupported non-constant SESSION_CONTEXT key in a DEFAULT")
    })?;
    Ok(rules::context_key(&argument)?.map(str::to_owned))
}

/// The conversion operand that is a SESSION_CONTEXT call, looking through the
/// parser's explicit integer conversion marker.
fn converted_context(expr: &mut Expr) -> Option<&mut Expr> {
    let (operand, target) = match expr {
        Expr::Cast {
            expr, data_type, ..
        } => (expr, &*data_type),
        Expr::Convert {
            expr,
            data_type: Some(data_type),
            ..
        } => (expr, &*data_type),
        _ => return None,
    };
    if crate::variant_pack::is_variant(target) {
        return None;
    }
    if crate::variant_cast::source(operand).is_some() {
        let Expr::Function(marker) = operand.as_mut() else {
            unreachable!("the marker is a function")
        };
        let FunctionArguments::List(list) = &mut marker.args else {
            unreachable!("the marker has one argument")
        };
        let [FunctionArg::Unnamed(FunctionArgExpr::Expr(inner))] = list.args.as_mut_slice() else {
            unreachable!("the marker has one argument")
        };
        return session_context(inner).is_some().then_some(inner);
    }
    session_context(operand)
        .is_some()
        .then_some(operand.as_mut())
}

/// `CAST`/`CONVERT(T, SESSION_CONTEXT(key))`: one conversion per base type,
/// selected by the stored kind; NULL when the key has no value.
fn lower_conversion(expr: &mut Expr) -> Result<bool> {
    let mut template = expr.clone();
    let Some(operand) = converted_context(&mut template) else {
        return Ok(false);
    };
    let Some(function) = session_context(operand) else {
        unreachable!("checked by converted_context")
    };
    let Some(key) = context_key(function)? else {
        *expr = Expr::Cast {
            kind: CastKind::Cast,
            expr: Box::new(Expr::Value(Value::Null.into())),
            data_type: match expr {
                Expr::Cast { data_type, .. } => data_type.clone(),
                Expr::Convert {
                    data_type: Some(data_type),
                    ..
                } => data_type.clone(),
                _ => unreachable!("checked by converted_context"),
            },
            format: None,
        };
        return Ok(true);
    };
    let (value, kind) = context_variables(&key);
    let conditions = base_types()
        .into_iter()
        .map(|(name, base)| {
            let mut branch = template.clone();
            *converted_context(&mut branch).expect("same shape as the template") = Expr::Cast {
                kind: CastKind::Cast,
                expr: Box::new(variable(&value)),
                data_type: base,
                format: None,
            };
            CaseWhen {
                condition: Expr::Value(Value::SingleQuotedString(name.into()).into()),
                result: branch,
            }
        })
        .collect();
    *expr = Expr::Case {
        case_token: sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
        end_token: sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
        operand: Some(Box::new(variable(&kind))),
        conditions,
        else_result: None,
    };
    Ok(true)
}

/// `SQL_VARIANT_PROPERTY(SESSION_CONTEXT(key), property)`: the context call
/// and the property argument.
fn variant_property(expr: &Expr) -> Option<(&Function, &Expr)> {
    let Expr::Function(function) = expr else {
        return None;
    };
    if !function_name(function)?.eq_ignore_ascii_case("SQL_VARIANT_PROPERTY")
        || function.over.is_some()
        || function.filter.is_some()
    {
        return None;
    }
    let FunctionArguments::List(list) = &function.args else {
        return None;
    };
    let [
        FunctionArg::Unnamed(FunctionArgExpr::Expr(value)),
        FunctionArg::Unnamed(FunctionArgExpr::Expr(property)),
    ] = list.args.as_slice()
    else {
        return None;
    };
    if !list.clauses.is_empty() {
        return None;
    }
    Some((session_context(value)?, property))
}

/// Whether a DEFAULT expression's result is a sql_variant read from session
/// state: SESSION_CONTEXT, SQL_VARIANT_PROPERTY of it, or ISNULL, COALESCE,
/// NULLIF, IIF or CASE results built from those. SQL Server refuses those for
/// a column of another type with 257 when the table is created.
fn variant_result(expr: &Expr) -> bool {
    match expr {
        Expr::Nested(inner) => variant_result(inner),
        Expr::Case {
            conditions,
            else_result,
            ..
        } => {
            conditions.iter().any(|when| variant_result(&when.result))
                || else_result.as_deref().is_some_and(variant_result)
        }
        Expr::Function(function) => {
            if session_context(expr).is_some() || variant_property(expr).is_some() {
                return true;
            }
            let Some(name) = function_name(function) else {
                return false;
            };
            let FunctionArguments::List(list) = &function.args else {
                return false;
            };
            let arguments: Vec<&Expr> = list
                .args
                .iter()
                .filter_map(|argument| match argument {
                    FunctionArg::Unnamed(FunctionArgExpr::Expr(expr)) => Some(expr),
                    _ => None,
                })
                .collect();
            let untyped_null = |expr: &&Expr| matches!(unnested(expr), Expr::Value(value) if matches!(value.value, Value::Null));
            let candidates: &[&Expr] = match name.to_ascii_uppercase().as_str() {
                // ISNULL(NULL, x) has the replacement's type.
                "ISNULL" if arguments.first().is_some_and(untyped_null) => &arguments,
                // ISNULL(CAST(... AS sql_variant), x) is a sql_variant; only
                // counted when x reads session state, so other defaults are
                // unaffected.
                "ISNULL"
                    if arguments
                        .first()
                        .is_some_and(|check| explicit_variant(check))
                        && arguments
                            .get(1)
                            .is_some_and(|replacement| variant_result(replacement)) =>
                {
                    return true;
                }
                "ISNULL" | "NULLIF" => arguments.get(..1).unwrap_or_default(),
                "COALESCE" => &arguments,
                "IIF" => arguments.get(1..).unwrap_or_default(),
                _ => &[],
            };
            candidates.iter().any(|candidate| variant_result(candidate))
        }
        _ => false,
    }
}

fn int_type() -> DataType {
    DataType::Int(None)
}

fn integer_variable(name: &str) -> Expr {
    Expr::Cast {
        kind: CastKind::Cast,
        expr: Box::new(variable(name)),
        data_type: int_type(),
        format: None,
    }
}

/// The operands of `expr` that a comparison or an explicit conversion
/// consumes by value, looking through parentheses and the parser's explicit
/// integer conversion marker. A sql_variant there converts from its base
/// type, so its base-type value can stand in for it; anywhere else (a write,
/// an assignment, a function argument) SQL Server keeps the sql_variant.
pub fn value_operands(expr: &mut Expr) -> Vec<&mut Expr> {
    let operands: Vec<&mut Expr> = match expr {
        Expr::BinaryOp {
            left,
            op:
                BinaryOperator::Eq
                | BinaryOperator::NotEq
                | BinaryOperator::Lt
                | BinaryOperator::LtEq
                | BinaryOperator::Gt
                | BinaryOperator::GtEq,
            right,
        }
        | Expr::IsDistinctFrom(left, right)
        | Expr::IsNotDistinctFrom(left, right) => vec![left.as_mut(), right.as_mut()],
        Expr::IsNull(operand) | Expr::IsNotNull(operand) => vec![operand.as_mut()],
        Expr::Between {
            expr, low, high, ..
        } => vec![expr.as_mut(), low.as_mut(), high.as_mut()],
        Expr::InList { expr, list, .. } => std::iter::once(expr.as_mut())
            .chain(list.iter_mut())
            .collect(),
        Expr::Case {
            operand: Some(operand),
            conditions,
            ..
        } => std::iter::once(operand.as_mut())
            .chain(conditions.iter_mut().map(|when| &mut when.condition))
            .collect(),
        Expr::Cast {
            expr: operand,
            data_type,
            ..
        }
        | Expr::Convert {
            expr: operand,
            data_type: Some(data_type),
            ..
        } if !crate::variant_pack::is_variant(data_type) => {
            if crate::variant_cast::source(operand).is_some() {
                let Expr::Function(marker) = operand.as_mut() else {
                    unreachable!("the marker is a function")
                };
                let FunctionArguments::List(list) = &mut marker.args else {
                    unreachable!("the marker has one argument")
                };
                let [FunctionArg::Unnamed(FunctionArgExpr::Expr(inner))] = list.args.as_mut_slice()
                else {
                    unreachable!("the marker has one argument")
                };
                vec![inner]
            } else {
                vec![operand.as_mut()]
            }
        }
        _ => vec![],
    };
    operands
        .into_iter()
        .map(|mut operand| {
            while let Expr::Nested(inner) = operand {
                operand = inner;
            }
            operand
        })
        .collect()
}

/// The declared type name of a constant or explicitly converted expression,
/// as conversion errors print it; `None` when it is not evident.
fn unnested(mut expr: &Expr) -> &Expr {
    while let Expr::Nested(inner) = expr {
        expr = inner;
    }
    expr
}

/// An explicit conversion to sql_variant.
fn explicit_variant(expr: &Expr) -> bool {
    match unnested(expr) {
        Expr::Cast { data_type, .. }
        | Expr::Convert {
            data_type: Some(data_type),
            ..
        } => crate::variant_pack::is_variant(data_type),
        _ => false,
    }
}

fn static_type_name(expr: &Expr) -> Option<String> {
    if explicit_variant(expr) {
        return None;
    }
    match expr {
        Expr::Nested(inner) => static_type_name(inner),
        Expr::Cast { data_type, .. }
        | Expr::Convert {
            data_type: Some(data_type),
            ..
        } => Some(type_name(data_type)),
        Expr::Value(value) => match &value.value {
            Value::NationalStringLiteral(_) => Some("nvarchar".into()),
            Value::SingleQuotedString(_) => Some("varchar".into()),
            // Integer constants are int when they fit, otherwise numeric, as
            // are decimal constants (captured from SQL Server).
            Value::Number(text, _) if text.bytes().all(|b| b.is_ascii_digit() || b == b'.') => {
                Some(
                    if text.parse::<i32>().is_ok() {
                        "int"
                    } else {
                        "numeric"
                    }
                    .into(),
                )
            }
            _ => None,
        },
        Expr::UnaryOp {
            op: UnaryOperator::Minus | UnaryOperator::Plus,
            expr,
        } if matches!(expr.as_ref(), Expr::Value(value) if matches!(value.value, Value::Number(..))) => {
            static_type_name(expr)
        }
        _ => None,
    }
}

fn implicit_variant_conversion(target: &str) -> anyhow::Error {
    SqlError::new(
        257,
        3,
        format!(
            "Implicit conversion from data type sql_variant to {target} is not allowed. Use the CONVERT function to run this query."
        ),
    )
    .into()
}

/// `ISNULL(check, replacement)` converts a sql_variant replacement to the
/// type of `check`, which SQL Server refuses with 257 when the table is
/// created. Only an evident `check` type is reported; others stay
/// unsupported.
fn check_isnull_replacement(expr: &Expr) -> Result<()> {
    let Expr::Function(function) = expr else {
        return Ok(());
    };
    if !function_name(function).is_some_and(|name| name.eq_ignore_ascii_case("ISNULL")) {
        return Ok(());
    }
    let FunctionArguments::List(list) = &function.args else {
        return Ok(());
    };
    if let [
        FunctionArg::Unnamed(FunctionArgExpr::Expr(check)),
        FunctionArg::Unnamed(FunctionArgExpr::Expr(replacement)),
    ] = list.args.as_slice()
        && variant_result(replacement)
        && !variant_result(check)
        && let Some(target) = static_type_name(check)
    {
        return Err(implicit_variant_conversion(&target));
    }
    Ok(())
}

/// `SQL_VARIANT_PROPERTY(SESSION_CONTEXT(key), property)` with a constant
/// property: the property of the stored kind, in its base type
/// (`nvarchar(128)` for BaseType and Collation, `int` otherwise). Applied
/// only to `value_operands`, where the base type is what SQL Server uses.
fn lower_property(expr: &mut Expr) -> Result<bool> {
    let Some((function, property)) = variant_property(expr) else {
        return Ok(false);
    };
    let key = context_key(function)?;
    let property = match rules::name_argument(property, &HashMap::new()) {
        Ok(rules::NameArgument::Text { text, .. }) => text,
        Ok(rules::NameArgument::Null) => None,
        _ => bail!("unsupported non-constant SQL_VARIANT_PROPERTY property in a DEFAULT"),
    };
    let data_type = match property
        .as_deref()
        .map(|p| p.trim_end().to_ascii_lowercase())
    {
        Some(name) if name == "basetype" || name == "collation" => login_name_type(),
        _ => int_type(),
    };
    let null = || Expr::Value(Value::Null.into());
    let value = match (key, property) {
        (Some(key), Some(property)) => {
            let (_, kind) = context_variables(&key);
            let (max, total) = context_length_variables(&key);
            let lowered = property.trim_end().to_ascii_lowercase();
            let conditions = base_types()
                .into_iter()
                .map(|(name, _)| {
                    let result = match (name, lowered.as_str()) {
                        ("nvarchar", "maxlength") => integer_variable(&max),
                        ("nvarchar", "totalbytes") => integer_variable(&total),
                        _ => match property_of(name, &property, (0, 0)) {
                            Some(Property::Text(text)) => {
                                Expr::Value(Value::SingleQuotedString(text.into()).into())
                            }
                            Some(Property::Int(value)) => crate::expr::number(value),
                            None => null(),
                        },
                    };
                    CaseWhen {
                        condition: Expr::Value(Value::SingleQuotedString(name.into()).into()),
                        result,
                    }
                })
                .collect();
            Expr::Case {
                case_token: sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
                end_token: sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
                operand: Some(Box::new(variable(&kind))),
                conditions,
                else_result: None,
            }
        }
        _ => null(),
    };
    *expr = Expr::Cast {
        kind: CastKind::Cast,
        expr: Box::new(value),
        data_type,
        format: None,
    };
    Ok(true)
}

/// `SESSION_CONTEXT(key) IS [NOT] NULL`: whether the key has a value.
fn lower_null_test(expr: &mut Expr) -> Result<bool> {
    let (Expr::IsNull(operand) | Expr::IsNotNull(operand)) = expr else {
        return Ok(false);
    };
    let Some(function) = session_context(operand) else {
        return Ok(false);
    };
    **operand = match context_key(function)? {
        Some(key) => variable(&context_variables(&key).1),
        None => Expr::Value(Value::Null.into()),
    };
    Ok(true)
}

/// SQL Server's lower-case name of a declared type, as conversion errors
/// print it.
fn type_name(kind: &DataType) -> String {
    let text = kind.to_string();
    text.split('(')
        .next()
        .unwrap_or(&text)
        .trim()
        .to_lowercase()
}

/// Rewrite the session functions of one column DEFAULT for a column of
/// type `target`. Returns whether anything changed.
pub fn rewrite_default(default: &mut Expr, target: &DataType) -> Result<bool> {
    let mut bare = default;
    while let Expr::Nested(inner) = bare {
        bare = inner;
    }
    let variant_target = crate::variant_pack::is_variant(target);
    if variant_target && session_context(bare).is_none() && variant_result(bare) {
        bail!("unsupported sql_variant DEFAULT expression over SESSION_CONTEXT");
    }
    if variant_result(bare) && !variant_target {
        return Err(implicit_variant_conversion(&type_name(target)));
    }
    struct Rewrite {
        changed: bool,
    }
    impl VisitorMut for Rewrite {
        type Break = anyhow::Error;
        fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<anyhow::Error> {
            if let Err(error) = check_isnull_replacement(expr) {
                return ControlFlow::Break(error);
            }
            for lower in [lower_conversion, lower_null_test] {
                match lower(expr) {
                    Ok(true) => {
                        self.changed = true;
                        return ControlFlow::Continue(());
                    }
                    Ok(false) => {}
                    Err(error) => return ControlFlow::Break(error),
                }
            }
            for operand in value_operands(expr) {
                match lower_property(operand) {
                    Ok(changed) => self.changed |= changed,
                    Err(error) => return ControlFlow::Break(error),
                }
            }
            if let Some(variable) = name_function(expr) {
                *expr = name_value(variable);
                self.changed = true;
            }
            ControlFlow::Continue(())
        }
        fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<anyhow::Error> {
            if session_context(expr).is_some() {
                return ControlFlow::Break(anyhow::anyhow!(
                    "unsupported SESSION_CONTEXT outside an explicit CAST or CONVERT in a DEFAULT"
                ));
            }
            ControlFlow::Continue(())
        }
    }
    let mut rewrite = Rewrite { changed: false };
    match bare.visit(&mut rewrite) {
        ControlFlow::Continue(()) => Ok(rewrite.changed),
        ControlFlow::Break(error) => Err(error),
    }
}

/// Rewrite the column DEFAULTs of a CREATE TABLE or ALTER TABLE ... ADD.
/// Returns whether any default reads session state.
pub fn rewrite_defaults(statement: &mut Statement) -> Result<bool> {
    let columns: Vec<&mut ColumnDef> = match statement {
        Statement::CreateTable(table) => table.columns.iter_mut().collect(),
        Statement::AlterTable(table) => table
            .operations
            .iter_mut()
            .filter_map(|operation| match operation {
                AlterTableOperation::AddColumn { column_def, .. } => Some(column_def),
                _ => None,
            })
            .collect(),
        _ => return Ok(false),
    };
    let mut changed = false;
    for column in columns {
        let target = column.data_type.clone();
        for option in &mut column.options {
            if let ColumnOption::Default(default) = &mut option.option {
                changed |= rewrite_default(default, &target)?;
            }
        }
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rewritten(sql: &str) -> Result<String> {
        let mut statement = crate::batch::parse(sql).unwrap().remove(0);
        rewrite_defaults(&mut statement)?;
        Ok(statement.to_string())
    }

    #[test]
    fn context_variables_follow_key_identity() {
        assert_eq!(context_variables("Email"), context_variables("eMail  "));
        assert_ne!(context_variables("email"), context_variables("EMAIL"));
        let (value, kind) = context_variables("foo");
        assert_eq!(value, "__msduck_session_context_666f6f006f");
        assert_eq!(kind, "__msduck_session_context_666f6f006f_kind");
        assert_eq!(
            context_value(&ContextValue::Bit(true)),
            Some(("bit", "1".into()))
        );
        assert_eq!(context_value(&ContextValue::Null), None);
    }

    #[test]
    fn name_functions_read_session_variables() {
        let sql = rewritten(
            "CREATE TABLE t (a nvarchar(128) DEFAULT (SUSER_SNAME()), b nvarchar(128) DEFAULT HOST_NAME(), c nvarchar(10) DEFAULT (CONVERT(nvarchar(10), APP_NAME())), d nvarchar(128) DEFAULT SYSTEM_USER, e nvarchar(128) DEFAULT (ORIGINAL_LOGIN()), f int DEFAULT (1))",
        )
        .unwrap();
        for expected in [
            "DEFAULT (CAST(getvariable('__msduck_session_login') AS NVARCHAR(128)))",
            "DEFAULT CAST(getvariable('__msduck_session_host') AS NVARCHAR(128))",
            "CAST(getvariable('__msduck_session_app') AS NVARCHAR(128))",
            "d NVARCHAR(128) DEFAULT CAST(getvariable('__msduck_session_login') AS NVARCHAR(128))",
            "f INT DEFAULT (1)",
        ] {
            assert!(sql.contains(expected), "{sql}");
        }
        // SUSER_SNAME(sid) is a lookup, not the session's login.
        assert!(
            rewritten("CREATE TABLE t (a nvarchar(128) DEFAULT (SUSER_SNAME(0x01)))")
                .unwrap()
                .contains("SUSER_SNAME(")
        );
    }

    #[test]
    fn context_conversions_select_the_stored_base_type() {
        let sql = rewritten(
            "CREATE TABLE t (v nvarchar(100) DEFAULT (CONVERT(nvarchar(100), SESSION_CONTEXT(N'foo'))))",
        )
        .unwrap();
        assert!(
            sql.contains("CASE getvariable('__msduck_session_context_666f6f006f_kind') WHEN 'nvarchar' THEN CONVERT(NVARCHAR(100), CAST(getvariable('__msduck_session_context_666f6f006f') AS NVARCHAR(4000))) WHEN 'bit' THEN CONVERT(NVARCHAR(100), CAST(getvariable('__msduck_session_context_666f6f006f') AS BIT))"),
            "{sql}"
        );
        assert!(sql.contains("WHEN 'bigint' THEN"), "{sql}");
        // The explicit integer conversion marker stays around each operand.
        let sql = rewritten("CREATE TABLE t (n int DEFAULT (CAST(SESSION_CONTEXT(N'n') AS int)))")
            .unwrap();
        assert!(
            sql.contains("WHEN 'int' THEN CAST(__msduck_explicit_integer_source(CAST(getvariable("),
            "{sql}"
        );
        let error =
            rewritten("CREATE TABLE t (n int DEFAULT (CAST(SESSION_CONTEXT(NULL) AS int)))")
                .unwrap_err();
        assert_eq!(error.downcast_ref::<SqlError>().unwrap().number, 8116);
        let mut statement = crate::batch::parse(
            "ALTER TABLE t ADD v varchar(5) NULL DEFAULT (CONVERT(varchar(5), SESSION_CONTEXT(N'k')))",
        )
        .unwrap()
        .remove(0);
        assert!(rewrite_defaults(&mut statement).unwrap());
        assert!(statement.to_string().contains("CASE getvariable("));
    }

    #[test]
    fn bare_and_unsupported_context_uses_are_refused() {
        let error =
            rewritten("CREATE TABLE t (v nvarchar(100) NULL DEFAULT (SESSION_CONTEXT(N'foo')))")
                .unwrap_err();
        let error = error.downcast_ref::<SqlError>().unwrap();
        assert_eq!(
            (error.number, error.state, error.message.as_str()),
            (
                257,
                3,
                "Implicit conversion from data type sql_variant to nvarchar is not allowed. Use the CONVERT function to run this query."
            )
        );
        // sql_variant results of ISNULL, COALESCE, CASE, IIF and
        // SQL_VARIANT_PROPERTY fail with the same 257 (reference case
        // session-default-positions).
        for sql in [
            "CREATE TABLE t (v nvarchar(10) DEFAULT (ISNULL(SESSION_CONTEXT(N'foo'), N'x')))",
            "CREATE TABLE t (v nvarchar(10) DEFAULT (COALESCE(N'x', SESSION_CONTEXT(N'foo'))))",
            "CREATE TABLE t (v nvarchar(10) DEFAULT (ISNULL(NULL, SESSION_CONTEXT(N'foo'))))",
            "CREATE TABLE t (v nvarchar(10) DEFAULT (ISNULL((NULL), SESSION_CONTEXT(N'foo'))))",
            "CREATE TABLE t (v nvarchar(10) DEFAULT (ISNULL(CAST(NULL AS sql_variant), SESSION_CONTEXT(N'foo'))))",
            "CREATE TABLE t (v nvarchar(10) DEFAULT (CASE WHEN 1 = 1 THEN SESSION_CONTEXT(N'foo') END))",
            "CREATE TABLE t (v nvarchar(10) DEFAULT IIF(1 = 1, N'x', SESSION_CONTEXT(N'foo')))",
            "CREATE TABLE t (v nvarchar(128) DEFAULT (SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'foo'), 'BaseType')))",
        ] {
            let error = rewritten(sql).unwrap_err();
            assert_eq!(
                error.downcast_ref::<SqlError>().unwrap().number,
                257,
                "{sql}"
            );
        }
        // ISNULL converts a sql_variant replacement to the check's type.
        for (sql, target) in [
            (
                "CREATE TABLE t (v int DEFAULT (ISNULL(CAST(NULL AS int), SESSION_CONTEXT(N'k'))))",
                "int",
            ),
            (
                "CREATE TABLE t (v nvarchar(10) DEFAULT (ISNULL(N'x', SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'k'), 'BaseType'))))",
                "nvarchar",
            ),
            (
                "CREATE TABLE t (v int DEFAULT (ISNULL(-1, SESSION_CONTEXT(N'k'))))",
                "int",
            ),
            (
                "CREATE TABLE t (v int DEFAULT (ISNULL(1.0, SESSION_CONTEXT(N'k'))))",
                "numeric",
            ),
            (
                "CREATE TABLE t (v bigint DEFAULT (ISNULL(3000000000, SESSION_CONTEXT(N'k'))))",
                "numeric",
            ),
        ] {
            let error = rewritten(sql).unwrap_err();
            let error = error.downcast_ref::<SqlError>().unwrap();
            assert_eq!(error.number, 257, "{sql}");
            assert!(error.message.contains(&format!("to {target} is")), "{sql}");
        }
        for (sql, message) in [
            (
                "CREATE TABLE t (v sql_variant DEFAULT (SESSION_CONTEXT(N'foo')))",
                "unsupported SESSION_CONTEXT outside an explicit CAST or CONVERT in a DEFAULT",
            ),
            (
                "CREATE TABLE t (v sql_variant DEFAULT (ISNULL(SESSION_CONTEXT(N'foo'), N'x')))",
                "unsupported sql_variant DEFAULT expression over SESSION_CONTEXT",
            ),
            (
                "CREATE TABLE t (v int DEFAULT (CASE WHEN SESSION_CONTEXT(N'foo') = 1 THEN 1 END))",
                "unsupported SESSION_CONTEXT outside an explicit CAST or CONVERT in a DEFAULT",
            ),
            (
                "CREATE TABLE t (v nvarchar(20) DEFAULT (UPPER(SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'foo'), 'BaseType'))))",
                "unsupported SESSION_CONTEXT outside an explicit CAST or CONVERT in a DEFAULT",
            ),
            (
                "CREATE TABLE t (v int DEFAULT (CONVERT(int, SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'foo'), name))))",
                "unsupported non-constant SQL_VARIANT_PROPERTY property in a DEFAULT",
            ),
        ] {
            assert_eq!(rewritten(sql).unwrap_err().to_string(), message, "{sql}");
        }
        let error =
            rewritten("CREATE TABLE t (v int DEFAULT (CONVERT(int, SESSION_CONTEXT('foo'))))")
                .unwrap_err();
        assert_eq!(error.downcast_ref::<SqlError>().unwrap().number, 8116);
    }

    #[test]
    fn variant_properties_and_null_tests_read_the_stored_kind() {
        let sql = rewritten(
            "CREATE TABLE items (id int, value nvarchar(100) DEFAULT CASE WHEN SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'foo'), 'BaseType') = N'nvarchar' THEN CONVERT(nvarchar(100), SESSION_CONTEXT(N'foo')) ELSE NULL END)",
        )
        .unwrap();
        assert!(
            sql.contains("CASE WHEN CAST(CASE getvariable('__msduck_session_context_666f6f006f_kind') WHEN 'nvarchar' THEN 'nvarchar' WHEN 'bit' THEN 'bit'"),
            "{sql}"
        );
        assert!(
            sql.contains(" AS NVARCHAR(128)) = N'nvarchar' THEN CASE getvariable("),
            "{sql}"
        );
        let sql = rewritten(
            "CREATE TABLE t (b bit DEFAULT (CASE WHEN SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'foo'), 'BaseType') IS NULL THEN 1 ELSE 0 END))",
        )
        .unwrap();
        assert!(sql.contains("AS NVARCHAR(128)) IS NULL THEN 1"), "{sql}");
        let sql = rewritten(
            "CREATE TABLE t (m int DEFAULT (CONVERT(int, SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'foo'), 'MaxLength'))), b bit DEFAULT (CASE WHEN SESSION_CONTEXT(N'foo') IS NOT NULL THEN 1 ELSE 0 END), n nvarchar(10) DEFAULT (CONVERT(nvarchar(10), SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'foo'), NULL))))",
        )
        .unwrap();
        assert!(
            sql.contains("WHEN 'nvarchar' THEN CAST(getvariable('__msduck_session_context_666f6f006f_max') AS INT) WHEN 'bit' THEN 1 WHEN 'tinyint' THEN 1 WHEN 'smallint' THEN 2 WHEN 'int' THEN 4 WHEN 'bigint' THEN 8 END AS INT)"),
            "{sql}"
        );
        assert!(
            sql.contains("CASE WHEN getvariable('__msduck_session_context_666f6f006f_kind') IS NOT NULL THEN 1"),
            "{sql}"
        );
        assert!(
            sql.contains("CONVERT(NVARCHAR(10), CAST(NULL AS INT))"),
            "{sql}"
        );
    }

    #[test]
    fn properties_follow_the_reference_capture() {
        let text = ContextValue::NVarChar {
            text: "bar".into(),
            max_bytes: 6,
        };
        assert_eq!(
            context_property(&text, "BaseType"),
            Some(Property::Text("nvarchar"))
        );
        assert_eq!(context_property(&text, "MaxLength"), Some(Property::Int(6)));
        assert_eq!(
            context_property(&text, "totalbytes"),
            Some(Property::Int(14))
        );
        assert_eq!(
            context_property(&text, "Collation"),
            Some(Property::Text(COLLATION))
        );
        assert_eq!(
            context_property(&ContextValue::BigInt(5), "Precision"),
            Some(Property::Int(19))
        );
        assert_eq!(
            context_property(&ContextValue::Bit(true), "TotalBytes"),
            Some(Property::Int(3))
        );
        assert_eq!(context_property(&ContextValue::Int(5), "Collation"), None);
        assert_eq!(context_property(&ContextValue::Int(5), "Bogus"), None);
        assert_eq!(context_property(&ContextValue::Null, "BaseType"), None);
        let (max, total) = context_length_variables("foo");
        assert_eq!(max, "__msduck_session_context_666f6f006f_max");
        assert_eq!(total, "__msduck_session_context_666f6f006f_total");
    }

    #[test]
    fn client_names_lower_anywhere() {
        let mut statement = crate::batch::parse("SELECT HOST_NAME() AS h, APP_NAME()")
            .unwrap()
            .remove(0);
        let _ = visit_expressions_mut(&mut statement, |expr| {
            lower_client_name(expr);
            ControlFlow::<()>::Continue(())
        });
        assert_eq!(
            statement.to_string(),
            "SELECT CAST(getvariable('__msduck_session_host') AS NVARCHAR(128)) AS h, CAST(getvariable('__msduck_session_app') AS NVARCHAR(128))"
        );
    }
}
