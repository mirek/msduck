//! RPC requests that name a procedure (tedious `callProcedure`, mssql
//! `request.execute`), and sp_executesql requests with OUTPUT parameters.
//!
//! The request runs as the call `EXEC name arguments` would in a SQL batch:
//! `sp_set_session_context`, then the features' exec hooks (user procedures
//! and sp_executesql here, sp_getapplock in `applock`, ...). Each argument
//! is bound to a synthetic caller variable typed as the RPC parameter, so
//! values never become SQL text and OUTPUT values convert back to the
//! parameter's declared type. `src/rpc.rs` completes the response with
//! RETURNSTATUS, RETURNVALUE and DONEPROC.
use super::super::{Exec, batch_begin, batch_end, exec, take_partial};
use crate::engine::{Parameter, Session, emit_error};
use crate::rpc::procedures::{RpcArgument, RpcOutcome, RpcResult};
use anyhow::Result;
use msduck_core::diagnostic::SqlError;
use sqlparser::ast::Statement;
use std::collections::HashMap;

const VARIABLE: &str = "@__msduck_rpc_";

/// Whether `name` is a parameter name an EXEC argument can carry.
fn parameter_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next() == Some('@')
        && name.len() > 1
        && chars.all(|c| c.is_alphanumeric() || matches!(c, '_' | '@' | '#' | '$'))
}

/// `EXEC name arguments`, each argument a synthetic variable. Arguments are
/// parsed one at a time and kept in request order: an RPC may pass a
/// positional argument after a named one (it binds by its ordinal), which
/// the T-SQL grammar rejects (119).
fn statement(name: &str, arguments: &[RpcArgument<'_>]) -> Result<Statement, SqlError> {
    let not_found = || {
        SqlError::new(
            2812,
            62,
            format!("Could not find stored procedure '{name}'."),
        )
    };
    let dialect = msduck_sql::dialect::ServerDialect;
    let object = sqlparser::parser::Parser::new(&dialect)
        .try_with_sql(name)
        .and_then(|mut parser| {
            let object = parser.parse_object_name(false)?;
            parser.expect_token(&sqlparser::tokenizer::Token::EOF)?;
            Ok(object)
        })
        .map_err(|_| not_found())?;
    let parse = |sql: &str| -> Result<Statement, SqlError> {
        let mut statements = crate::engine::parse_batch(sql).map_err(sql_error)?;
        match (statements.pop(), statements.is_empty()) {
            (Some(statement), true) => Ok(statement),
            _ => Err(not_found()),
        }
    };
    let mut call = parse(&format!("EXEC {object}"))?;
    let mut parameters = Vec::new();
    for (index, argument) in arguments.iter().enumerate() {
        let mut text = String::new();
        if let Some(parameter) = argument.name {
            if !parameter_name(parameter) {
                return Err(SqlError::new(
                    8145,
                    1,
                    format!("{parameter} is not a parameter for procedure {name}."),
                ));
            }
            text.push_str(parameter);
            text.push_str(" = ");
        }
        if argument.default {
            text.push_str("DEFAULT");
        } else {
            text.push_str(&format!("{VARIABLE}{index}"));
            if argument.output {
                text.push_str(" OUTPUT");
            }
        }
        let Statement::Execute {
            parameters: mut parsed,
            ..
        } = parse(&format!("EXEC {object} {text}"))?
        else {
            return Err(not_found());
        };
        if parsed.len() != 1 {
            return Err(not_found());
        }
        parameters.push(parsed.remove(0));
    }
    if let Statement::Execute {
        parameters: target, ..
    } = &mut call
    {
        *target = parameters;
    }
    // As the batch parser does: sp_set_session_context binds by position.
    let mut statements = vec![call];
    msduck_sql::session_function::positional_arguments(&mut statements);
    msduck_sql::session_function::validate_set_calls(&statements).map_err(sql_error)?;
    Ok(statements.remove(0))
}

fn sql_error(error: anyhow::Error) -> SqlError {
    error
        .downcast_ref::<SqlError>()
        .cloned()
        .unwrap_or_else(|| SqlError::new(50000, 1, error.to_string()))
}

impl Session {
    /// Run an RPC request naming procedure `name` with `arguments`. The call
    /// is its own batch scope, like any RPC request.
    pub(crate) fn rpc_call(&mut self, name: &str, arguments: &[RpcArgument<'_>]) -> RpcResult {
        let mut tokens = Vec::new();
        let mut variables: HashMap<String, Parameter> = arguments
            .iter()
            .enumerate()
            .filter(|(_, argument)| !argument.default)
            .map(|(index, argument)| (format!("{VARIABLE}{index}"), argument.value.clone()))
            .collect();
        let failed = |session: &mut Session, error: &anyhow::Error| {
            let mut tokens = Vec::new();
            let number = emit_error(&mut tokens, error);
            session.last_error = number;
            RpcOutcome::Failed {
                number,
                error: tokens,
            }
        };
        let statement = match statement(name, arguments) {
            Ok(statement) => statement,
            Err(error) => {
                let outcome = failed(self, &error.into());
                return RpcResult {
                    tokens,
                    outcome,
                    outputs: arguments.iter().map(|_| None).collect(),
                };
            }
        };
        batch_begin(self, true);
        self.caught_error = None;
        if self.ext.procedures.frames.is_empty() {
            self.ext.procedures.error_procedure = None;
        }
        let result = match self.set_session_context(&statement, &variables) {
            Some(result) => result.map(|()| Exec::status(0)),
            None => match exec(self, &statement, &mut variables) {
                Some(result) => result,
                // No feature runs it: what `EXEC` does in a batch.
                None => self
                    .execute(statement.clone(), &mut variables)
                    .map(|execution| Exec {
                        tokens: execution.tokens,
                        status: 0,
                    }),
            },
        };
        let mut outcome = match result {
            Ok(Exec {
                tokens: produced,
                status,
            }) => {
                tokens.extend(produced);
                self.last_error = 0;
                RpcOutcome::Completed(status)
            }
            Err(error) => {
                let error = take_partial(error, &mut tokens);
                if error.downcast_ref::<SqlError>().is_some() {
                    failed(self, &error)
                } else {
                    self.last_error = emit_error(&mut tokens, &error);
                    RpcOutcome::Aborted
                }
            }
        };
        batch_end(self);
        // As at the end of any batch: a doomed transaction rolls back.
        if self.transaction_doomed {
            if let RpcOutcome::Failed { error, .. } = &outcome {
                tokens.extend(error);
            }
            self.rollback_doomed(&mut tokens);
            self.error(
                &mut tokens,
                3998,
                "Uncommittable transaction is detected at the end of the batch. The transaction is rolled back.",
            );
            outcome = RpcOutcome::Aborted;
        }
        self.caught_error = None;
        let outputs = arguments
            .iter()
            .enumerate()
            .map(|(index, argument)| {
                argument
                    .output
                    .then(|| variables.get(&format!("{VARIABLE}{index}")).cloned())
                    .flatten()
            })
            .collect();
        RpcResult {
            tokens,
            outcome,
            outputs,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use msduck_core::{types::Type, value::Value};

    fn int(value: i32) -> Parameter {
        Parameter {
            value: Value::Int(value),
            data_type: Type::Int,
        }
    }

    #[test]
    fn arguments_keep_request_order_and_markers() {
        let one = int(1);
        let arguments = [
            RpcArgument {
                name: Some("@a"),
                value: &one,
                output: false,
                default: false,
            },
            RpcArgument {
                name: None,
                value: &one,
                output: true,
                default: false,
            },
            RpcArgument {
                name: Some("@c"),
                value: &one,
                output: false,
                default: true,
            },
        ];
        let statement = statement("[dbo].[p]", &arguments).unwrap();
        let call = msduck_sql::dialect::ext::procedures::call(&statement).unwrap();
        assert_eq!(call.arguments.len(), 3);
        assert_eq!(call.arguments[0].name, Some("@a"));
        assert!(!call.arguments[0].output);
        assert_eq!(call.arguments[1].name, None);
        assert!(call.arguments[1].output);
        assert_eq!(call.arguments[2].name, Some("@c"));
        assert!(call.arguments[2].value.is_none());
    }

    #[test]
    fn names_never_become_sql() {
        let one = int(1);
        for name in ["p; DROP TABLE t", "p q", "", "p(1)"] {
            let error = statement(name, &[]).unwrap_err();
            assert_eq!(error.number, 2812, "{name}");
        }
        let argument = RpcArgument {
            name: Some("@a = 1; DROP TABLE t; --"),
            value: &one,
            output: false,
            default: false,
        };
        assert_eq!(statement("p", &[argument]).unwrap_err().number, 8145);
    }
}
