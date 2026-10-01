//! Application locks: sp_getapplock, sp_releaseapplock, APPLOCK_MODE and
//! APPLOCK_TEST. See docs/gaps-applock.md.
//!
//! Locks live in a process-wide table ([`table`]) keyed by database,
//! database principal and resource, and owned by a session token (never the
//! SPID) together with the session or transaction owner. Session locks end
//! with the session (disconnect or RESETCONNECTION), transaction locks at
//! the outermost COMMIT or ROLLBACK.
use super::{Exec, Feature};
use crate::engine::{Parameter, Session};
use anyhow::{Result, anyhow, bail};
use msduck_core::{diagnostic::SqlError, value::Value as ParameterValue};
use sqlparser::ast::{
    CastKind, DataType, Expr, Function, FunctionArg, FunctionArgExpr, FunctionArguments, Statement,
    Value as Literal,
};
use std::collections::HashMap;

mod call;
mod principal;
mod table;

#[derive(Default)]
pub(crate) struct State;

pub(super) struct Hooks;

impl Feature for Hooks {
    fn name(&self) -> &'static str {
        "applock"
    }

    fn exec(
        &self,
        session: &mut Session,
        statement: &Statement,
        variables: &mut HashMap<String, Parameter>,
    ) -> Option<Result<Exec>> {
        call::exec(session, statement, variables)
    }

    fn rewrite_expr(
        &self,
        session: &Session,
        expr: &mut Expr,
        parameters: &HashMap<String, Parameter>,
    ) -> Result<()> {
        function(session, expr, parameters)
    }

    fn transaction_end(&self, session: &mut Session, _committed: bool) {
        let token = session.ext.token;
        table::release_all(|owner| owner.session == token && owner.transaction);
    }

    fn session_end(&self, session: &mut Session) {
        let token = session.ext.token;
        table::release_all(|owner| owner.session == token);
    }
}

/// An argument value, after evaluation.
#[derive(Clone, Debug, PartialEq)]
enum Value {
    Null,
    Int(i64),
    /// Character data in UTF-16 units; `unicode` for nvarchar.
    Text {
        units: Vec<u16>,
        unicode: bool,
    },
}

impl Value {
    fn text(&self) -> String {
        match self {
            Self::Null => String::new(),
            Self::Int(value) => value.to_string(),
            Self::Text { units, .. } => String::from_utf16_lossy(units),
        }
    }
}

/// `nvarchar(255)` resource names: the UTF-16 units, truncated as the
/// parameter (or function argument) conversion truncates them.
fn resource_units(value: &Value) -> Vec<u16> {
    let units = match value {
        Value::Text { units, .. } => units.clone(),
        other => other.text().encode_utf16().collect(),
    };
    units.into_iter().take(255).collect()
}

fn from_parameter(value: &ParameterValue) -> Result<Value> {
    Ok(match value {
        ParameterValue::Null => Value::Null,
        ParameterValue::Boolean(v) => Value::Int((*v).into()),
        ParameterValue::TinyInt(v) => Value::Int((*v).into()),
        ParameterValue::UTinyInt(v) => Value::Int((*v).into()),
        ParameterValue::SmallInt(v) => Value::Int((*v).into()),
        ParameterValue::Int(v) => Value::Int((*v).into()),
        ParameterValue::BigInt(v) => Value::Int(*v),
        ParameterValue::Text(text) => Value::Text {
            units: text.encode_utf16().collect(),
            unicode: false,
        },
        ParameterValue::Unicode(units) => Value::Text {
            units: units.clone(),
            unicode: true,
        },
        other => bail!("unsupported application lock argument value {other:?}"),
    })
}

fn from_backend(value: duckdb::types::Value) -> Result<Value> {
    use duckdb::types::Value as Backend;
    Ok(match value {
        Backend::Null => Value::Null,
        Backend::Boolean(v) => Value::Int(v.into()),
        Backend::TinyInt(v) => Value::Int(v.into()),
        Backend::SmallInt(v) => Value::Int(v.into()),
        Backend::Int(v) => Value::Int(v.into()),
        Backend::BigInt(v) => Value::Int(v),
        Backend::UTinyInt(v) => Value::Int(v.into()),
        Backend::USmallInt(v) => Value::Int(v.into()),
        Backend::UInt(v) => Value::Int(v.into()),
        // int parameters truncate exact and approximate numbers.
        Backend::Float(v) => Value::Int(v.trunc() as i64),
        Backend::Double(v) => Value::Int(v.trunc() as i64),
        Backend::Decimal(v) => {
            let text = v.to_string();
            Value::Int(
                text.split('.')
                    .next()
                    .and_then(|whole| whole.parse().ok())
                    .ok_or_else(|| anyhow!("unsupported application lock argument value {text}"))?,
            )
        }
        Backend::Text(text) => Value::Text {
            units: text.encode_utf16().collect(),
            unicode: false,
        },
        other => from_parameter(&crate::backend_value::from_backend(other)?)?,
    })
}

/// Evaluate an argument: literals and variables directly, anything else
/// as a scalar expression without a FROM clause.
fn evaluate(
    session: &Session,
    expr: &Expr,
    variables: &HashMap<String, Parameter>,
) -> Result<Value> {
    match expr {
        Expr::Value(value) => match &value.value {
            Literal::Null => return Ok(Value::Null),
            Literal::SingleQuotedString(text) => {
                return Ok(Value::Text {
                    units: text.encode_utf16().collect(),
                    unicode: false,
                });
            }
            Literal::NationalStringLiteral(text) => {
                return Ok(Value::Text {
                    units: text.encode_utf16().collect(),
                    unicode: true,
                });
            }
            Literal::Number(text, _) => {
                if let Ok(number) = text.parse::<i64>() {
                    return Ok(Value::Int(number));
                }
            }
            _ => {}
        },
        Expr::Identifier(ident) if ident.value.starts_with('@') => {
            let name = ident.value.to_lowercase();
            let parameter = variables.get(&name).ok_or_else(|| {
                anyhow!(SqlError::new(
                    137,
                    2,
                    format!("Must declare the scalar variable \"{}\".", ident.value)
                ))
            })?;
            let mut value = from_parameter(&parameter.value)?;
            if let Value::Text { unicode, .. } = &mut value {
                *unicode |= crate::sql_type::ast(parameter.data_type)
                    .to_string()
                    .to_ascii_uppercase()
                    .starts_with('N');
            }
            return Ok(value);
        }
        _ => {}
    }
    from_backend(session.evaluate_expression(expr.clone(), variables, false)?)
}

/// Store a procedure's return status in the `EXEC @status =` variable,
/// converting it to the variable's declared type.
fn assign(
    session: &Session,
    variable: &str,
    status: i32,
    variables: &mut HashMap<String, Parameter>,
) -> Result<()> {
    let kind = variables[variable].data_type;
    let value = session.evaluate_scalar(
        Expr::value(Literal::Number(status.to_string(), false)),
        crate::sql_type::ast(kind),
        variables,
    )?;
    variables.insert(
        variable.into(),
        Parameter {
            value: crate::backend_value::from_backend(value)?,
            data_type: kind,
        },
    );
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Lookup {
    Mode,
    Test,
}

impl Lookup {
    fn of(function: &Function) -> Option<Self> {
        let [part] = function.name.0.as_slice() else {
            return None;
        };
        let name = part.as_ident()?;
        if name.quote_style.is_some() {
            return None;
        }
        if name.value.eq_ignore_ascii_case("APPLOCK_MODE") {
            Some(Self::Mode)
        } else if name.value.eq_ignore_ascii_case("APPLOCK_TEST") {
            Some(Self::Test)
        } else {
            None
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::Mode => "applock_mode",
            Self::Test => "applock_test",
        }
    }
}

fn function_error(number: i32, state: u8, message: impl Into<String>) -> anyhow::Error {
    anyhow!(SqlError::new(number, state, message.into()))
}

/// Replace APPLOCK_MODE and APPLOCK_TEST with their current values. The
/// arguments must not depend on rows (constants, variables and expressions
/// over them); the lock table cannot change during one statement.
fn function(
    session: &Session,
    expr: &mut Expr,
    variables: &HashMap<String, Parameter>,
) -> Result<()> {
    let Expr::Function(function) = expr else {
        return Ok(());
    };
    let Some(lookup) = Lookup::of(function) else {
        return Ok(());
    };
    let arguments: Vec<&Expr> = match &function.args {
        FunctionArguments::List(list) if list.clauses.is_empty() => list
            .args
            .iter()
            .map(|arg| match arg {
                FunctionArg::Unnamed(FunctionArgExpr::Expr(expr)) => Some(expr),
                _ => None,
            })
            .collect::<Option<_>>()
            .ok_or_else(|| anyhow!("unsupported arguments in {function}"))?,
        FunctionArguments::None | FunctionArguments::List(_) => Vec::new(),
        FunctionArguments::Subquery(_) => bail!("unsupported arguments in {function}"),
    };
    let expected = match lookup {
        Lookup::Mode => 3,
        Lookup::Test => 4,
    };
    if arguments.len() != expected {
        return Err(function_error(
            174,
            1,
            format!(
                "The {} function requires {expected} argument(s).",
                lookup.name()
            ),
        ));
    }
    // Literal NULL and numeric arguments fail when the statement compiles.
    for (position, argument) in arguments.iter().enumerate() {
        let declared = match argument {
            Expr::Value(value) => match &value.value {
                Literal::Null => Some("NULL"),
                Literal::Number(text, _) if text.parse::<i32>().is_ok() => Some("int"),
                Literal::Number(..) => Some("numeric"),
                _ => None,
            },
            _ => None,
        };
        // A NULL owner means the default (Transaction) owner.
        let owner = position + 1 == expected;
        if let Some(declared) = declared
            && !(owner && declared == "NULL")
        {
            return Err(function_error(
                8116,
                1,
                format!(
                    "Argument data type {declared} is invalid for argument {} of {} function.",
                    position + 1,
                    lookup.name()
                ),
            ));
        }
    }
    let values = arguments
        .iter()
        .map(|argument| evaluate(session, argument, variables))
        .collect::<Result<Vec<_>>>()
        .map_err(|error| {
            if error.downcast_ref::<SqlError>().is_some() {
                error
            } else {
                anyhow!(
                    "unsupported {} argument: only constants and variables are supported ({error})",
                    lookup.name().to_uppercase()
                )
            }
        })?;
    let principal = match &values[0] {
        Value::Null => {
            return Err(function_error(
                1230,
                3,
                format!(
                    "An invalid database principal was passed to {}.",
                    lookup.name()
                ),
            ));
        }
        value => value.text(),
    };
    let resource = match &values[1] {
        Value::Null => {
            return Err(function_error(
                1225,
                1,
                format!(
                    "An invalid application lock mode was passed to {}.",
                    lookup.name()
                ),
            ));
        }
        value => resource_units(value),
    };
    let requested = match lookup {
        Lookup::Mode => None,
        Lookup::Test => Some(match &values[2] {
            Value::Null => {
                return Err(function_error(
                    1225,
                    2,
                    "An invalid application lock mode was passed to applock_test.",
                ));
            }
            value => table::Mode::requested(&value.text()).ok_or_else(|| {
                function_error(
                    1225,
                    3,
                    "An invalid application lock mode was passed to applock_test.",
                )
            })?,
        }),
    };
    let transaction = match &values[expected - 1] {
        Value::Null => true,
        value => {
            let text = value.text();
            let text = text.trim_end_matches(' ');
            if text.eq_ignore_ascii_case("Transaction") {
                true
            } else if text.eq_ignore_ascii_case("Session") {
                false
            } else {
                return Err(function_error(
                    1226,
                    1,
                    format!(
                        "An invalid application lock owner was passed to {}.",
                        lookup.name()
                    ),
                ));
            }
        }
    };
    if transaction && session.transactions == 0 {
        return Err(function_error(
            3918,
            2,
            "The statement or function must be executed in the context of a user transaction.",
        ));
    }
    let canonical = principal::canonical(&principal).ok_or_else(|| {
        function_error(
            1202,
            1,
            format!("The database-principal '{principal}' does not exist or user is not a member."),
        )
    })?;
    let key = table::Key::new(&session.database().name, canonical, &resource);
    let owner = table::Owner {
        session: session.ext.token,
        transaction,
    };
    *expr = match requested {
        None => {
            let mode = table::mode(&key, owner).map_or("NoLock", table::Mode::name);
            Expr::Cast {
                kind: CastKind::Cast,
                expr: Box::new(Expr::value(Literal::NationalStringLiteral(mode.into()))),
                data_type: DataType::Nvarchar(Some(
                    sqlparser::ast::CharacterLength::IntegerLength {
                        length: 32,
                        unit: None,
                    },
                )),
                format: None,
            }
        }
        Some(mode) => Expr::value(Literal::Number(
            i32::from(table::test(&key, owner, mode)).to_string(),
            false,
        )),
    };
    Ok(())
}
