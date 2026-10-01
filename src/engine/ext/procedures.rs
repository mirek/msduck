//! Stored procedures, dynamic SQL (`EXEC (string)`) and sp_executesql.
//!
//! - `define`: CREATE/ALTER/CREATE OR ALTER PROCEDURE (a batch hook, since
//!   the definition must start its batch and keep its source text) and DROP
//!   PROCEDURE, over the module store.
//! - `call`: argument binding, OUTPUT round trips and return status for
//!   procedures, `EXEC (string)` and sp_executesql in SQL batches.
//! - `run`: the body interpreter. Each call runs in its own frame with its
//!   own variables; statements, control flow and errors follow the captured
//!   SQL Server token streams in docs/gaps-procedures.md.
use super::{Exec, Feature};
use crate::engine::{Execution, Parameter, Session};
use anyhow::Result;
use msduck_core::diagnostic::SqlError;
use sqlparser::ast::{Expr, Statement};
use std::collections::HashMap;

mod call;
mod define;
mod run;

/// Maximum procedure nesting (SQL Server error 217 beyond it).
const MAX_NESTING: usize = 32;

/// One active call.
struct Frame {
    /// The procedure name; `None` for dynamic SQL and sp_executesql.
    name: Option<String>,
    /// A caller's CATCH handler receives this frame's errors.
    in_try: bool,
}

#[derive(Default)]
pub(crate) struct State {
    /// Active calls, innermost last.
    frames: Vec<Frame>,
    /// The error most recently raised inside a procedure and that
    /// procedure's name, for ERROR_PROCEDURE().
    error_procedure: Option<(SqlError, String)>,
    /// Response bytes the enclosing frames already hold, so nested output
    /// stays within the TDS response bound.
    base: usize,
}

pub(super) struct Hooks;

impl Feature for Hooks {
    fn name(&self) -> &'static str {
        "procedures"
    }

    fn batch(
        &self,
        session: &mut Session,
        sql: &str,
        parameters: &HashMap<String, Parameter>,
        rpc: bool,
    ) -> Option<(Vec<u8>, bool)> {
        if let Some(definition) = msduck_sql::dialect::ext::procedures::definition(sql) {
            // sp_executesql prefixes its parameter declarations, so the
            // definition no longer starts the batch (captured: 156).
            let definition = if rpc && !parameters.is_empty() {
                Err(define::nested_create())
            } else {
                definition
            };
            return Some(define::create(session, sql, definition, rpc));
        }
        // A batch whose first statement is a module name executes it.
        if !rpc && define::bare_call(session, sql) {
            return Some(session.batch_response(&format!("EXEC {sql}"), parameters, false, None));
        }
        None
    }

    fn exec(
        &self,
        session: &mut Session,
        statement: &Statement,
        variables: &mut HashMap<String, Parameter>,
    ) -> Option<Result<Exec>> {
        call::exec(session, statement, variables)
    }

    fn statement(
        &self,
        session: &mut Session,
        statement: &mut Statement,
        _parameters: &mut HashMap<String, Parameter>,
    ) -> Result<Option<Execution>> {
        match statement {
            Statement::DropProcedure {
                if_exists,
                proc_desc,
                ..
            } => define::drop(session, *if_exists, proc_desc).map(Some),
            _ => Ok(None),
        }
    }

    fn rewrite_expr(
        &self,
        session: &Session,
        expr: &mut Expr,
        _parameters: &HashMap<String, Parameter>,
    ) -> Result<()> {
        use sqlparser::ast::*;
        match expr {
            Expr::Identifier(ident)
                if ident.quote_style.is_none()
                    && ident.value.eq_ignore_ascii_case("@@NESTLEVEL") =>
            {
                *expr = msduck_sql::expr::number(session.ext.procedures.frames.len());
            }
            Expr::Function(function)
                if function
                    .name
                    .to_string()
                    .eq_ignore_ascii_case("ERROR_PROCEDURE")
                    && matches!(&function.args, FunctionArguments::List(list) if list.args.is_empty()) =>
            {
                if let (Some(caught), Some((error, name))) = (
                    session.caught_error.as_ref(),
                    session.ext.procedures.error_procedure.as_ref(),
                ) && same(caught, error)
                {
                    *expr = Expr::Cast {
                        kind: CastKind::Cast,
                        expr: Box::new(Expr::Value(
                            Value::NationalStringLiteral(name.clone()).into(),
                        )),
                        data_type: DataType::Nvarchar(Some(CharacterLength::IntegerLength {
                            length: 128,
                            unit: None,
                        })),
                        format: None,
                    };
                }
            }
            _ => {}
        }
        Ok(())
    }
}

fn same(a: &SqlError, b: &SqlError) -> bool {
    a.number == b.number && a.state == b.state && a.severity == b.severity && a.message == b.message
}

/// The SQL Server diagnostic the engine would send for `error`: number,
/// state, severity and message, exactly as `emit_error` encodes them.
fn diagnostic(error: &anyhow::Error) -> SqlError {
    if let Some(error) = error.downcast_ref::<SqlError>() {
        return error.clone();
    }
    if let Some(crate::engine::StatementErrors(errors)) =
        error.downcast_ref::<crate::engine::StatementErrors>()
        && let Some(last) = errors.last()
    {
        return last.clone();
    }
    let mut tokens = Vec::new();
    crate::engine::emit_error(&mut tokens, error);
    decode_error(&tokens).unwrap_or_else(|| SqlError::new(50000, 1, error.to_string()))
}

/// Decode the first ERROR token of an encoded diagnostic.
fn decode_error(tokens: &[u8]) -> Option<SqlError> {
    if tokens.first() != Some(&0xaa) || tokens.len() < 11 {
        return None;
    }
    let number = i32::from_le_bytes(tokens[3..7].try_into().ok()?);
    let (state, severity) = (tokens[7], tokens[8]);
    let units = u16::from_le_bytes(tokens[9..11].try_into().ok()?) as usize;
    let text = tokens.get(11..11 + units * 2)?;
    let units: Vec<u16> = text
        .chunks_exact(2)
        .map(|unit| u16::from_le_bytes([unit[0], unit[1]]))
        .collect();
    Some(SqlError::from_utf16(number, state, severity, units))
}
