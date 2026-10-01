//! SESSIONPROPERTY options and the per-session store behind
//! `sp_set_session_context` and `SESSION_CONTEXT`. Rules come from
//! reference/session-property-context.json; every input is explicit and the
//! store is an ordinary value owned by the caller's session.
use crate::parameter::Parameter;
use anyhow::{Result, bail};
use msduck_core::{
    character::{Family, Length},
    diagnostic::SqlError,
    types::Type,
    value::Value,
};
use sqlparser::ast::{
    BinaryOperator, Expr, ObjectNamePart, Statement, UnaryOperator, Value as AstValue,
};
use std::collections::HashMap;

/// The SET options SESSIONPROPERTY reports. `LOGIN` is the state tedious
/// establishes at login (captured `@@OPTIONS` 5496).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionOptions {
    pub ansi_nulls: bool,
    pub ansi_padding: bool,
    pub ansi_warnings: bool,
    pub arithabort: bool,
    pub concat_null_yields_null: bool,
    pub quoted_identifier: bool,
    pub numeric_roundabort: bool,
}
impl SessionOptions {
    pub const LOGIN: Self = Self {
        ansi_nulls: true,
        ansi_padding: true,
        ansi_warnings: true,
        arithabort: true,
        concat_null_yields_null: true,
        quoted_identifier: true,
        numeric_roundabort: false,
    };
    /// Option names compare case-insensitively and ignore trailing spaces,
    /// not leading ones. Any other name, including other SET options such as
    /// ANSI_NULL_DFLT_ON, has no value (NULL).
    pub fn property(&self, name: &str) -> Option<bool> {
        let name = name.trim_end_matches(' ');
        let options = [
            ("ANSI_NULLS", self.ansi_nulls),
            ("ANSI_PADDING", self.ansi_padding),
            ("ANSI_WARNINGS", self.ansi_warnings),
            ("ARITHABORT", self.arithabort),
            ("CONCAT_NULL_YIELDS_NULL", self.concat_null_yields_null),
            ("QUOTED_IDENTIFIER", self.quoted_identifier),
            ("NUMERIC_ROUNDABORT", self.numeric_roundabort),
        ];
        options
            .iter()
            .find(|(option, _)| option.eq_ignore_ascii_case(name))
            .map(|(_, value)| *value)
    }
}

/// A stored value with its `sql_variant` base type. Only the families below
/// are supported; others are refused before the store changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContextValue {
    Null,
    Bit(bool),
    TinyInt(u8),
    SmallInt(i16),
    Int(i32),
    BigInt(i64),
    /// `nvarchar` with the declared byte length of its source.
    NVarChar {
        text: String,
        max_bytes: u16,
    },
}
impl ContextValue {
    fn bytes(&self) -> usize {
        match self {
            Self::Null => 0,
            Self::Bit(_) | Self::TinyInt(_) => 1,
            Self::SmallInt(_) => 2,
            Self::Int(_) => 4,
            Self::BigInt(_) => 8,
            Self::NVarChar { text, .. } => text.encode_utf16().count() * 2,
        }
    }
}

#[derive(Clone, Debug)]
struct Entry {
    key: String,
    value: ContextValue,
    read_only: bool,
}

/// Explicit bound on stored keys and values. SQL Server accounts internal
/// allocations against 1 MB, which was not derived; this simple byte sum is
/// a guard and is documented as approximate.
pub const SIZE_LIMIT: usize = 1 << 20;

/// Per-session key/value store. RESETCONNECTION replaces it with an empty one.
#[derive(Clone, Debug, Default)]
pub struct SessionContext {
    entries: Vec<Entry>,
}
impl SessionContext {
    /// Stored keys (as first set) and values, in the order keys were added.
    pub fn entries(&self) -> impl Iterator<Item = (&str, &ContextValue)> {
        self.entries
            .iter()
            .map(|entry| (entry.key.as_str(), &entry.value))
    }
    pub fn get(&self, key: &str) -> Option<&ContextValue> {
        self.entries
            .iter()
            .find(|entry| keys_match(&entry.key, key))
            .map(|entry| &entry.value)
    }
    pub fn set(&mut self, key: &str, value: ContextValue, read_only: bool) -> Result<(), SqlError> {
        let units = key.encode_utf16().count();
        if !(1..=128).contains(&units) {
            return Err(SqlError::new(
                15666,
                1,
                format!(
                    "Cannot set key '{key}' in the session context. The size of the key cannot exceed 256 bytes."
                ),
            ));
        }
        let existing = self
            .entries
            .iter()
            .position(|entry| keys_match(&entry.key, key));
        if let Some(index) = existing
            && self.entries[index].read_only
        {
            return Err(SqlError::new(
                15664,
                1,
                format!(
                    "Cannot set key '{}' in the session context. The key has been set as read_only for this session.",
                    self.entries[index].key
                ),
            ));
        }
        let others: usize = self
            .entries
            .iter()
            .enumerate()
            .filter(|(index, _)| Some(*index) != existing)
            .map(|(_, entry)| entry.key.encode_utf16().count() * 2 + entry.value.bytes())
            .sum();
        if others + units * 2 + value.bytes() > SIZE_LIMIT {
            return Err(SqlError::new(
                15665,
                1,
                format!(
                    "The value was not set for key '{key}' because the total size of keys and values in the session context would exceed the 1 MB limit."
                ),
            ));
        }
        match existing {
            Some(index) => {
                let entry = &mut self.entries[index];
                entry.value = value;
                entry.read_only = read_only;
            }
            None => self.entries.push(Entry {
                key: key.to_owned(),
                value,
                read_only,
            }),
        }
        Ok(())
    }
}

/// Captured key identity under SQL_Latin1_General_CP1_CI_AS: equal ignoring
/// case and trailing spaces, and the final non-space character must match
/// exactly (`email` finds `Email` and `email `, but not `EMAIL`).
pub fn keys_match(stored: &str, probe: &str) -> bool {
    key_identity(stored) == key_identity(probe)
}

/// The identity [`keys_match`] compares: the lower-case key without trailing
/// spaces, and its exact final non-space character.
pub fn key_identity(key: &str) -> (String, Option<char>) {
    let key = key.trim_end_matches(' ');
    (key.to_lowercase(), key.chars().last())
}

/// One `EXEC sp_set_session_context` argument, bound by position. Parameter
/// names are ignored, as SQL Server does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Argument {
    Null,
    Integer(String),
    NationalString(String),
    String(String),
    Variable(String),
}

/// Recognize `sp_set_session_context`, `sys.sp_set_session_context` and
/// `<database>.sys.sp_set_session_context` and bind its arguments.
pub fn set_call(statement: &Statement) -> Option<Result<Vec<Argument>>> {
    let Statement::Execute {
        name: Some(name),
        parameters,
        has_parentheses,
        immediate,
        into,
        using,
        output,
        default,
    } = statement
    else {
        return None;
    };
    let parts: Vec<_> = name
        .0
        .iter()
        .map(|part| match part {
            ObjectNamePart::Identifier(ident) => Some(ident.value.as_str()),
            _ => None,
        })
        .collect::<Option<_>>()?;
    let procedure = match parts.as_slice() {
        [procedure] | [_, procedure] | [_, _, procedure] => procedure,
        _ => return None,
    };
    if !procedure.eq_ignore_ascii_case("sp_set_session_context")
        || (parts.len() > 1 && !parts[parts.len() - 2].eq_ignore_ascii_case("sys"))
    {
        return None;
    }
    if *has_parentheses
        || *immediate
        || !into.is_empty()
        || !using.is_empty()
        || *output
        || *default
    {
        return Some(Err(anyhow::anyhow!(
            "unsupported sp_set_session_context call form: {statement}"
        )));
    }
    Some(parameters.iter().map(argument).collect())
}

/// SQL Server binds `sp_set_session_context` arguments by position and
/// ignores their names, so `@key = N'k'` is `N'k'`. Dropping the names keeps
/// preflight from reading them as undeclared variables. Nested blocks are
/// normalized too; other procedure calls are unchanged.
pub fn positional_arguments<T: sqlparser::ast::VisitMut>(node: &mut T) {
    struct Strip;
    impl sqlparser::ast::VisitorMut for Strip {
        type Break = ();
        fn pre_visit_statement(&mut self, statement: &mut Statement) -> std::ops::ControlFlow<()> {
            if set_call(statement).is_some()
                && let Statement::Execute { parameters, .. } = statement
            {
                for parameter in parameters {
                    if let Expr::BinaryOp {
                        left,
                        op: BinaryOperator::Eq,
                        right,
                    } = parameter
                        && matches!(left.as_ref(), Expr::Identifier(id) if id.value.starts_with('@'))
                    {
                        *parameter = right.as_ref().clone();
                    }
                }
            }
            std::ops::ControlFlow::Continue(())
        }
    }
    let _ = node.visit(&mut Strip);
}

/// Argument syntax SQL Server rejects while compiling the batch (102 for an
/// expression argument), before any statement runs.
pub fn validate_set_calls<T: sqlparser::ast::Visit>(node: &T) -> Result<()> {
    struct Check;
    impl sqlparser::ast::Visitor for Check {
        type Break = anyhow::Error;
        fn pre_visit_statement(
            &mut self,
            statement: &Statement,
        ) -> std::ops::ControlFlow<anyhow::Error> {
            match set_call(statement) {
                Some(Err(error)) if error.downcast_ref::<SqlError>().is_some() => {
                    std::ops::ControlFlow::Break(error)
                }
                _ => std::ops::ControlFlow::Continue(()),
            }
        }
    }
    match node.visit(&mut Check) {
        std::ops::ControlFlow::Continue(()) => Ok(()),
        std::ops::ControlFlow::Break(error) => Err(error),
    }
}

fn argument(expr: &Expr) -> Result<Argument> {
    let expr = match expr {
        Expr::BinaryOp {
            left,
            op: BinaryOperator::Eq,
            right,
        } if matches!(left.as_ref(), Expr::Identifier(id) if id.value.starts_with('@')) => right,
        other => other,
    };
    Ok(match expr {
        Expr::Value(value) => match &value.value {
            AstValue::Null => Argument::Null,
            AstValue::Number(text, false) => Argument::Integer(text.clone()),
            AstValue::NationalStringLiteral(text) => Argument::NationalString(text.clone()),
            AstValue::SingleQuotedString(text) => Argument::String(text.clone()),
            _ => bail!("unsupported sp_set_session_context argument {expr}"),
        },
        Expr::UnaryOp {
            op: UnaryOperator::Minus,
            expr: inner,
        } if matches!(inner.as_ref(), Expr::Value(v) if matches!(v.value, AstValue::Number(_, false))) =>
        {
            let Expr::Value(v) = inner.as_ref() else {
                unreachable!()
            };
            let AstValue::Number(text, _) = &v.value else {
                unreachable!()
            };
            Argument::Integer(format!("-{text}"))
        }
        Expr::Identifier(id) if id.value.starts_with('@') && !id.value.starts_with("@@") => {
            Argument::Variable(id.value.to_lowercase())
        }
        // SQL Server accepts only constants and variables here.
        Expr::BinaryOp { op, .. } => {
            return Err(SqlError::syntax(102, 1, format!("Incorrect syntax near '{op}'.")).into());
        }
        _ => bail!("unsupported sp_set_session_context argument {expr}"),
    })
}

/// A call ready for the store, after arity, key and value validation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetContext {
    pub key: String,
    pub value: ContextValue,
    pub read_only: bool,
}

fn invalid_parameters() -> SqlError {
    SqlError::new(
        225,
        1,
        "The parameters supplied for the procedure \"sp_set_session_context\" are not valid.",
    )
}
fn invalid_option() -> SqlError {
    SqlError::new(
        15600,
        1,
        "An invalid parameter or option was specified for procedure 'sp_set_connection_context'.",
    )
}

/// Resolve bound arguments against the batch's variables. Execution errors
/// are `SqlError`s; unsupported value families are explicit plain errors.
pub fn bind(arguments: &[Argument], variables: &HashMap<String, Parameter>) -> Result<SetContext> {
    if arguments.len() < 2 {
        return Err(SqlError::new(
            16903,
            1,
            "The \"sp_set_connection_context\" procedure was called with an incorrect number of parameters.",
        )
        .into());
    }
    if arguments.len() > 3 {
        return Err(SqlError::new(
            16914,
            1,
            "The \"sp_set_connection_context\" procedure was called with too many parameters.",
        )
        .into());
    }
    let variable = |name: &str| {
        variables
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("Must declare the scalar variable {name}"))
    };
    let key = match &arguments[0] {
        Argument::NationalString(text) | Argument::String(text) => text.clone(),
        Argument::Variable(name) => {
            let parameter = variable(name)?;
            match (&parameter.data_type, text(&parameter.value)) {
                (Type::Character(_), Some(text)) => text,
                _ => return Err(invalid_parameters().into()),
            }
        }
        Argument::Null | Argument::Integer(_) => return Err(invalid_parameters().into()),
    };
    let value = match &arguments[1] {
        Argument::Null => ContextValue::Null,
        Argument::Integer(text) => match text.parse::<i32>() {
            Ok(value) => ContextValue::Int(value),
            Err(_) => bail!("unsupported sp_set_session_context value: numeric constant {text}"),
        },
        Argument::NationalString(text) => ContextValue::NVarChar {
            max_bytes: nvarchar_bytes(text)?,
            text: text.clone(),
        },
        Argument::String(_) => {
            bail!("unsupported sp_set_session_context value type varchar")
        }
        Argument::Variable(name) => variable_value(variable(name)?)?,
    };
    let read_only = match arguments.get(2) {
        None => false,
        Some(Argument::Integer(text)) => text.trim_start_matches('-').bytes().any(|b| b != b'0'),
        Some(Argument::Variable(name)) => {
            let parameter = variable(name)?;
            match (&parameter.data_type, &parameter.value) {
                (_, Value::Null) => return Err(invalid_option().into()),
                (Type::Bit, Value::Boolean(value)) => *value,
                (Type::TinyInt | Type::SmallInt | Type::Int | Type::BigInt, value) => {
                    integer(value).is_some_and(|value| value != 0)
                }
                _ => bail!("unsupported sp_set_session_context read_only argument type"),
            }
        }
        Some(Argument::Null | Argument::NationalString(_) | Argument::String(_)) => {
            return Err(invalid_option().into());
        }
    };
    Ok(SetContext {
        key,
        value,
        read_only,
    })
}

fn nvarchar_bytes(text: &str) -> Result<u16> {
    let units = text.encode_utf16().count().max(1);
    if units > 4000 {
        bail!(
            "unsupported sp_set_session_context value: nvarchar constant longer than 4000 characters"
        );
    }
    Ok(units as u16 * 2)
}
fn text(value: &Value) -> Option<String> {
    match value {
        Value::Text(text) => Some(text.clone()),
        Value::Unicode(units) => Some(String::from_utf16_lossy(units)),
        _ => None,
    }
}
fn integer(value: &Value) -> Option<i64> {
    Some(match value {
        Value::Boolean(value) => i64::from(*value),
        Value::TinyInt(value) => i64::from(*value),
        Value::UTinyInt(value) => i64::from(*value),
        Value::SmallInt(value) => i64::from(*value),
        Value::Int(value) => i64::from(*value),
        Value::BigInt(value) => *value,
        _ => return None,
    })
}
fn variable_value(parameter: &Parameter) -> Result<ContextValue> {
    if let Type::Character(character) = parameter.data_type
        && character.length() == Length::Max
    {
        return Err(invalid_option().into());
    }
    if matches!(parameter.value, Value::Null) {
        return Ok(ContextValue::Null);
    }
    let value = &parameter.value;
    let unsupported = || {
        anyhow::anyhow!(
            "unsupported sp_set_session_context value type {:?}",
            parameter.data_type
        )
    };
    Ok(match parameter.data_type {
        Type::Bit => ContextValue::Bit(integer(value).ok_or_else(unsupported)? != 0),
        Type::TinyInt => {
            ContextValue::TinyInt(u8::try_from(integer(value).ok_or_else(unsupported)?)?)
        }
        Type::SmallInt => {
            ContextValue::SmallInt(i16::try_from(integer(value).ok_or_else(unsupported)?)?)
        }
        Type::Int => ContextValue::Int(i32::try_from(integer(value).ok_or_else(unsupported)?)?),
        Type::BigInt => ContextValue::BigInt(integer(value).ok_or_else(unsupported)?),
        Type::Character(character) if character.family() == Family::Nvarchar => {
            let Length::Bounded(length) = character.length() else {
                unreachable!("max length refused above")
            };
            ContextValue::NVarChar {
                text: text(value).ok_or_else(unsupported)?,
                max_bytes: length * 2,
            }
        }
        _ => return Err(unsupported()),
    })
}

/// The constant text of a SESSIONPROPERTY or SESSION_CONTEXT argument.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NameArgument {
    /// An untyped NULL constant.
    Null,
    /// A character constant or variable, Unicode (`N'...'`, nchar/nvarchar)
    /// or not. A NULL character variable keeps its type.
    Text { text: Option<String>, unicode: bool },
    /// A non-character constant or variable, named by its SQL type.
    Other(&'static str),
}

/// Resolve a function argument that must be a constant or a variable.
pub fn name_argument(expr: &Expr, variables: &HashMap<String, Parameter>) -> Result<NameArgument> {
    Ok(match expr {
        Expr::Nested(inner) => return name_argument(inner, variables),
        Expr::Value(value) => match &value.value {
            AstValue::Null => NameArgument::Null,
            AstValue::NationalStringLiteral(text) => NameArgument::Text {
                text: Some(text.clone()),
                unicode: true,
            },
            AstValue::SingleQuotedString(text) => NameArgument::Text {
                text: Some(text.clone()),
                unicode: false,
            },
            AstValue::Number(_, _) => NameArgument::Other("int"),
            _ => bail!("unsupported session function argument {expr}"),
        },
        Expr::Identifier(id) if id.value.starts_with('@') && !id.value.starts_with("@@") => {
            let name = id.value.to_lowercase();
            let parameter = variables
                .get(&name)
                .ok_or_else(|| anyhow::anyhow!("Must declare the scalar variable {name}"))?;
            match parameter.data_type {
                Type::Character(character) => {
                    let unicode = matches!(character.family(), Family::Nvarchar | Family::Nchar);
                    let text =
                        match &parameter.value {
                            Value::Null => None,
                            value => Some(text(value).ok_or_else(|| {
                                anyhow::anyhow!("invalid character variable value")
                            })?),
                        };
                    NameArgument::Text { text, unicode }
                }
                Type::Int => NameArgument::Other("int"),
                _ => bail!(
                    "unsupported session function argument type {:?}",
                    parameter.data_type
                ),
            }
        }
        _ => bail!(
            "unsupported session function argument {expr}; only constants and variables are supported"
        ),
    })
}

/// SESSION_CONTEXT requires a Unicode key; an untyped NULL or a non-Unicode
/// argument raises 8116 before execution. A NULL Unicode variable reads NULL.
pub fn context_key(argument: &NameArgument) -> Result<Option<&str>, SqlError> {
    let kind = match argument {
        NameArgument::Text {
            text,
            unicode: true,
        } => return Ok(text.as_deref()),
        NameArgument::Text { unicode: false, .. } => "varchar",
        NameArgument::Null => "NULL",
        NameArgument::Other(kind) => kind,
    };
    Err(SqlError::new(
        8116,
        1,
        format!("Argument data type {kind} is invalid for argument 1 of session_context function."),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_keep_first_spelling_and_identity_matches_keys_match() {
        let mut context = SessionContext::default();
        context.set("Email", ContextValue::Int(1), false).unwrap();
        context.set("email ", ContextValue::Int(2), false).unwrap();
        context
            .set(
                "name",
                ContextValue::NVarChar {
                    text: "x".into(),
                    max_bytes: 2,
                },
                false,
            )
            .unwrap();
        let entries: Vec<_> = context.entries().collect();
        assert_eq!(
            entries,
            vec![
                ("Email", &ContextValue::Int(2)),
                (
                    "name",
                    &ContextValue::NVarChar {
                        text: "x".into(),
                        max_bytes: 2
                    }
                )
            ]
        );
        for (stored, probe, same) in [
            ("email", "Email", true),
            ("Email", "email", true),
            ("Email", "EMAIL ", false),
            ("Email", "eMail  ", true),
            ("email", "email", true),
            ("ab", "aB", false),
            ("aB", "AB", true),
        ] {
            assert_eq!(keys_match(stored, probe), same, "{stored} {probe}");
            assert_eq!(key_identity(stored) == key_identity(probe), same);
        }
    }
}
