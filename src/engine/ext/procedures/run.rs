//! The interpreter for procedure bodies and dynamic SQL.
//!
//! It mirrors the batch loop in `Session::batch_response_inner` with the
//! differences SQL Server shows inside a module (see docs/gaps-procedures.md):
//!
//! - completions are DONEINPROC tokens and always carry DONE_MORE;
//! - a statement-terminating error continues with the next statement, a
//!   compilation error ends only this frame, and other errors end the batch;
//! - when a caller's CATCH handler will receive the error, the frame ends at
//!   the first error instead;
//! - nested calls end with DONEINPROC (224) instead of RETURNSTATUS and
//!   DONEPROC.
use super::super::{Exec, take_partial};
use super::Frame;
use crate::engine::{
    Parameter, Session, Work, binding_failure, control_done, ddl_completion_command, emit_error,
};
use crate::tds;
use anyhow::Result;
use duckdb::types::Value;
use msduck_core::diagnostic::SqlError;
use sqlparser::ast::{DataType, ReturnStatementValue, Statement};
use std::collections::HashMap;

/// How a frame ended without completing.
pub(super) enum Failure {
    /// An error the caller handles: its CATCH handler receives it, or it
    /// ends the call (compilation errors end only the frame). Not yet sent.
    Error(anyhow::Error),
    /// The batch is aborted; the error has been sent.
    Abort,
}

/// How a frame completed.
#[derive(Default)]
pub(super) struct Finished {
    /// The value of `RETURN expression`.
    pub explicit: Option<i32>,
    /// Highest severity of the errors this frame's statements raised.
    pub severity: u8,
    /// The engine's batch status rule (used by sp_executesql): the last
    /// error number, reset by a later successful statement.
    pub error_status: i32,
}

impl Finished {
    /// A procedure's status without an explicit RETURN value: 0, or
    /// 10 - severity after an error (captured -6 for 16 and -4 for 14).
    pub fn procedure_status(&self) -> i32 {
        self.explicit.unwrap_or(if self.severity > 10 {
            10 - i32::from(self.severity)
        } else {
            0
        })
    }
}

/// Settings SQL Server restores when a module returns.
struct Saved {
    nocount: bool,
    xact_abort: bool,
    ansi_warnings: bool,
    datefirst: i32,
    caught_error: Option<SqlError>,
    base: usize,
}

/// Run `body` in a new frame and restore the caller's settings afterwards.
pub(super) fn frame(
    session: &mut Session,
    name: Option<String>,
    in_try: bool,
    levels: usize,
    body: impl FnOnce(&mut Session, &mut Vec<u8>) -> Result<Finished, Failure>,
) -> (Vec<u8>, Result<Finished, Failure>) {
    let saved = Saved {
        nocount: session.nocount,
        xact_abort: session.xact_abort,
        ansi_warnings: session.ansi_warnings,
        datefirst: session.datefirst,
        caught_error: session.caught_error.clone(),
        base: session.ext.procedures.base,
    };
    session.ext.procedures.frames.push(Frame {
        name,
        in_try,
        levels,
    });
    let mut out = Vec::new();
    let result = body(session, &mut out);
    session.ext.procedures.frames.pop();
    session.nocount = saved.nocount;
    session.xact_abort = saved.xact_abort;
    session.ansi_warnings = saved.ansi_warnings;
    session.datefirst = saved.datefirst;
    session.caught_error = saved.caught_error;
    session.ext.procedures.base = saved.base;
    (out, result)
}

/// Whether the innermost frame's errors go to a caller's CATCH handler.
fn in_try(session: &Session) -> bool {
    session
        .ext
        .procedures
        .frames
        .last()
        .is_some_and(|frame| frame.in_try)
}

/// Remember the procedure an error arose in, for ERROR_PROCEDURE().
pub(super) fn note(session: &mut Session, error: &anyhow::Error) {
    let name = session
        .ext
        .procedures
        .frames
        .last()
        .and_then(|frame| frame.name.clone());
    session.ext.procedures.error_procedure = name.map(|name| (super::diagnostic(error), name));
}

fn more(out: &mut Vec<u8>, status: u16, command: u16, count: u64) {
    tds::done(out, 0xff, 1 | status, command, count);
}

/// Execute `statements` with `variables` as this frame's scope.
pub(super) fn run(
    session: &mut Session,
    statements: &[Statement],
    variables: &mut HashMap<String, Parameter>,
    out: &mut Vec<u8>,
) -> Result<Finished, Failure> {
    let mut finished = Finished::default();
    let mut pending: Vec<Work<'_>> = statements.iter().rev().map(Work::leaf).collect();
    let mut steps = 0usize;
    while let Some(work) = pending.pop() {
        steps += 1;
        if steps > 10_000 {
            session.error(
                out,
                50000,
                "batch execution exceeds current 10000-step limit",
            );
            return Err(Failure::Abort);
        }
        if out.len() + session.ext.procedures.base > tds::MAX_MESSAGE - 1024 {
            session.error(
                out,
                50000,
                "batch result exceeds current 16 MiB response limit",
            );
            return Err(Failure::Abort);
        }
        if let Some(command) = work.completion {
            control_done(out, true, session.nocount, command);
            continue;
        }
        if let Some(previous) = work.restore_error {
            session.caught_error = previous;
            session.rowcount = 0;
            control_done(out, true, session.nocount, 351);
            continue;
        }
        let statement = work.statement;
        if let Some((body, handler)) = msduck_sql::preflight::try_catch_parts(statement) {
            control_done(out, true, session.nocount, 349);
            pending.push(Work {
                statement,
                loop_boundary: false,
                catch_handler: Some(handler),
                restore_error: Some(session.caught_error.clone()),
                completion: None,
            });
            pending.extend(body.iter().rev().map(Work::leaf));
            continue;
        }
        if let Some(is_continue) = crate::dialect::loop_control(statement) {
            if let Some(index) = pending.iter().rposition(|work| work.loop_boundary) {
                session.unwind_work(&mut pending, index + usize::from(is_continue));
                control_done(out, true, session.nocount, 202);
                continue;
            }
            session.error(out, 50000, "loop control outside WHILE");
            return Err(Failure::Abort);
        }
        // An error raised by this frame's own statement.
        macro_rules! fail {
            ($error:expr, $caught_command:expr) => {{
                let error: anyhow::Error = $error;
                note(session, &error);
                if !binding_failure(&error) {
                    let severity = super::diagnostic(&error).severity;
                    finished.severity = finished.severity.max(severity);
                }
                if session.catch_error(&mut pending, &error) {
                    if let Some(command) = $caught_command {
                        control_done(out, true, session.nocount, command);
                    }
                    continue;
                }
                if in_try(session) {
                    if let Some(command) = $caught_command {
                        control_done(out, true, session.nocount, command);
                    }
                    return Err(Failure::Error(error));
                }
                if binding_failure(&error) {
                    return Err(Failure::Error(error));
                }
                session.last_error = emit_error(out, &error);
                session.rollback_doomed(out);
                return Err(Failure::Abort);
            }};
        }
        if let Statement::Return(return_statement) = statement {
            match &return_statement.value {
                None => {
                    session.rowcount = 0;
                    session.last_error = 0;
                    control_done(out, true, session.nocount, 219);
                }
                Some(ReturnStatementValue::Expr(expression)) => {
                    let value =
                        session.evaluate_scalar(expression.clone(), DataType::Int(None), variables);
                    let status = match value {
                        Ok(Value::Int(value)) => value,
                        Ok(Value::Null) => {
                            let name = session
                                .ext
                                .procedures
                                .frames
                                .last()
                                .and_then(|frame| frame.name.clone())
                                .unwrap_or_default();
                            tds::diagnostic_utf16(
                                out,
                                tds::DiagnosticKind::Information,
                                0,
                                1,
                                282,
                                &format!("The '{name}' procedure attempted to return a status of NULL, which is not allowed. A status of 0 will be returned instead.")
                                    .encode_utf16()
                                    .collect::<Vec<_>>(),
                            );
                            0
                        }
                        Ok(_) => unreachable!("RETURN expression is cast to INT"),
                        Err(error) => fail!(error, None::<u16>),
                    };
                    finished.explicit = Some(status);
                    session.rowcount = 1;
                    session.last_error = 0;
                    if !session.nocount {
                        more(out, 0x10, 193, 1);
                    }
                }
            }
            // Leave every TRY and CATCH this frame entered.
            session.unwind_work(&mut pending, 0);
            return Ok(finished);
        }
        let branch: Result<Option<&[Statement]>> = match statement {
            Statement::While(while_statement) => while_statement
                .while_block
                .condition
                .clone()
                .ok_or_else(|| anyhow::anyhow!("missing WHILE condition"))
                .and_then(|expression| session.evaluate_expression(expression, variables, true))
                .map(|value| {
                    if matches!(value, Value::Boolean(true)) {
                        pending.push(Work {
                            statement,
                            loop_boundary: true,
                            catch_handler: None,
                            restore_error: None,
                            completion: None,
                        });
                        Some(while_statement.while_block.statements().as_slice())
                    } else {
                        Some(&[][..])
                    }
                }),
            Statement::If(condition) => condition
                .if_block
                .condition
                .clone()
                .ok_or_else(|| anyhow::anyhow!("missing IF condition"))
                .and_then(|expression| session.evaluate_expression(expression, variables, true))
                .map(|value| {
                    if matches!(value, Value::Boolean(true)) {
                        Some(condition.if_block.statements().as_slice())
                    } else {
                        Some(
                            condition
                                .else_block
                                .as_ref()
                                .map(|block| block.statements().as_slice())
                                .unwrap_or(&[]),
                        )
                    }
                }),
            Statement::StartTransaction {
                has_end_keyword: true,
                statements,
                exception,
                modifier,
                ..
            } => {
                if exception.is_some() || modifier.is_some() {
                    Err(anyhow::anyhow!("unsupported exception block"))
                } else {
                    Ok(Some(statements.as_slice()))
                }
            }
            _ => Ok(None),
        };
        match branch {
            Ok(Some(statements)) => {
                if matches!(statement, Statement::If(_) | Statement::While(_)) {
                    control_done(out, true, session.nocount, 0xc0);
                }
                pending.extend(statements.iter().rev().map(Work::leaf));
                continue;
            }
            Ok(None) => {}
            Err(error) => {
                let command =
                    matches!(statement, Statement::If(_) | Statement::While(_)).then_some(0xc0u16);
                fail!(error, command)
            }
        }
        if let Some(call) = msduck_sql::raiserror::call(statement) {
            let raised = match crate::raiserror::bind(&call, variables) {
                Ok(raised) => raised,
                Err(error) => fail!(error, None::<u16>),
            };
            session.rowcount = 0;
            let is_error = raised.delivery == msduck_core::raiserror::Delivery::Error;
            if is_error {
                let error: anyhow::Error = raised.diagnostic.clone().into();
                note(session, &error);
                finished.severity = finished.severity.max(raised.diagnostic.severity);
                // Like the batch loop, only a TRY in this frame may doom the
                // transaction: XACT_ABORT does not apply to RAISERROR.
                if pending.iter().any(|work| work.catch_handler.is_some())
                    && session.catch_error(&mut pending, &error)
                {
                    control_done(out, true, session.nocount, 246);
                    continue;
                }
                if in_try(session) {
                    control_done(out, true, session.nocount, 246);
                    return Err(Failure::Error(error));
                }
                finished.error_status = raised.error_number;
                session.last_error = raised.error_number;
                tds::sql_error(out, &raised.diagnostic);
                more(out, 2, 246, 0);
                continue;
            }
            session.last_error = raised.error_number;
            let units = raised
                .diagnostic
                .message_utf16
                .clone()
                .unwrap_or_else(|| raised.diagnostic.message.encode_utf16().collect());
            tds::diagnostic_utf16(
                out,
                tds::DiagnosticKind::Information,
                raised.diagnostic.severity,
                raised.diagnostic.state,
                raised.diagnostic.number,
                &units,
            );
            finished.error_status = 0;
            control_done(out, true, session.nocount, 246);
            continue;
        }
        let procedure_call = match session.set_session_context(statement, variables) {
            Some(result) => Some(result.map(|()| Exec::status(0))),
            None => {
                session.ext.procedures.base += out.len();
                let result = super::super::exec(session, statement, variables);
                session.ext.procedures.base -= out.len();
                result
            }
        };
        if let Some(result) = procedure_call {
            match result {
                Ok(Exec { tokens, .. }) => {
                    out.extend(tokens);
                    session.last_error = 0;
                    finished.error_status = 0;
                    control_done(out, true, session.nocount, 224);
                }
                Err(error) => {
                    let error = take_partial(error, out);
                    if error.downcast_ref::<super::call::Aborted>().is_some() {
                        return Err(Failure::Abort);
                    }
                    if session.catch_error(&mut pending, &error) {
                        control_done(out, true, session.nocount, 224);
                        continue;
                    }
                    if in_try(session) {
                        control_done(out, true, session.nocount, 224);
                        return Err(Failure::Error(error));
                    }
                    if error.downcast_ref::<SqlError>().is_none() {
                        // Unsupported forms stop the batch explicitly.
                        session.last_error = emit_error(out, &error);
                        return Err(Failure::Abort);
                    }
                    finished.error_status = super::diagnostic(&error).number;
                    session.last_error = emit_error(out, &error);
                    if !session.nocount {
                        more(out, 2, 224, 0);
                    }
                }
            }
            continue;
        }
        match session.execute(statement.clone(), variables) {
            Ok(crate::engine::Execution {
                tokens,
                count,
                command,
                kind,
            }) => {
                out.extend(tokens);
                session.last_error = 0;
                finished.error_status = 0;
                if matches!(statement, Statement::Declare { stmts }
                    if stmts.iter().all(|declaration| declaration.assignment.is_none()))
                {
                    continue;
                }
                session.rowcount = count.unwrap_or(0);
                if ddl_completion_command(statement) == Some(253)
                    || !msduck_core::completion::visible(true, session.nocount, kind)
                {
                    continue;
                }
                more(
                    out,
                    if count.is_some() && !session.nocount {
                        0x10
                    } else {
                        0
                    },
                    command,
                    count.unwrap_or(0),
                );
            }
            Err(error) => {
                let error = take_partial(error, out);
                if let Some(failed) = error.downcast_ref::<crate::query_error::FailedQuery>() {
                    out.extend_from_slice(&failed.metadata);
                    if matches!(failed.command, 0xc1 | 0xc3..=0xc5) {
                        session.rowcount = 0;
                    }
                }
                note(session, &error);
                let failed_binding = binding_failure(&error);
                if !failed_binding {
                    let severity = super::diagnostic(&error).severity;
                    finished.severity = finished.severity.max(severity);
                }
                // Tokens SQL Server sends for a failure a CATCH handler takes.
                let caught_tokens = |session: &mut Session, out: &mut Vec<u8>| {
                    if let Some(failed) = error.downcast_ref::<crate::output_sink::Failed>() {
                        session.rowcount = 0;
                        if !session.nocount {
                            let command = match failed.operation {
                                msduck_sql::output::Operation::Insert => 0xc3,
                                msduck_sql::output::Operation::Update => 0xc5,
                                msduck_sql::output::Operation::Delete => 0xc4,
                            };
                            more(out, 0x10, command, 0);
                        }
                    }
                    if matches!(statement, Statement::Commit { .. }) {
                        control_done(out, true, session.nocount, 0);
                    }
                    if matches!(statement, Statement::Throw(_)) {
                        control_done(out, true, session.nocount, 246);
                    }
                    if let Some(failed) = error.downcast_ref::<crate::query_error::FailedQuery>() {
                        more(
                            out,
                            if session.nocount { 0 } else { 0x10 },
                            failed.command,
                            0,
                        );
                    }
                };
                if session.catch_error(&mut pending, &error) {
                    caught_tokens(session, out);
                    continue;
                }
                if in_try(session) {
                    caught_tokens(session, out);
                    return Err(Failure::Error(error));
                }
                if failed_binding {
                    return Err(Failure::Error(error));
                }
                let diagnostic = super::diagnostic(&error);
                session.last_error = emit_error(out, &error);
                if session.transaction_doomed {
                    session.rollback_doomed(out);
                    return Err(Failure::Abort);
                }
                if session.last_error == 2742 && matches!(statement, Statement::Set(_)) {
                    finished.error_status = diagnostic.number;
                    session.rowcount = 0;
                    more(out, 2, 0, 0);
                    continue;
                }
                let failed_dml = error
                    .downcast_ref::<crate::query_error::FailedQuery>()
                    .map(|failed| failed.command)
                    .filter(|command| matches!(command, 0xc3..=0xc5))
                    .or_else(|| {
                        error.downcast_ref::<crate::output_sink::Failed>().map(
                            |failed| match failed.operation {
                                msduck_sql::output::Operation::Insert => 0xc3,
                                msduck_sql::output::Operation::Update => 0xc5,
                                msduck_sql::output::Operation::Delete => 0xc4,
                            },
                        )
                    });
                if let Some(command) = failed_dml {
                    tds::diagnostic_utf16(
                        out,
                        tds::DiagnosticKind::Information,
                        0,
                        0,
                        3621,
                        &"The statement has been terminated."
                            .encode_utf16()
                            .collect::<Vec<_>>(),
                    );
                    finished.error_status = diagnostic.number;
                    session.rowcount = 0;
                    more(out, 2, command, 0);
                    continue;
                }
                if let Some(failed) = error.downcast_ref::<crate::query_error::FailedQuery>()
                    && matches!(session.last_error, 8115 | 8134)
                {
                    finished.error_status = diagnostic.number;
                    session.rowcount = 0;
                    more(out, 2, failed.command, 0);
                    continue;
                }
                if msduck_sql::drop_index_syntax::request(statement).is_some()
                    && session.last_error == 3701
                {
                    finished.error_status = diagnostic.number;
                    session.rowcount = 0;
                    more(out, 2, 201, 0);
                    continue;
                }
                return Err(Failure::Abort);
            }
        }
    }
    Ok(finished)
}

/// A CREATE/ALTER PROCEDURE that begins dynamic SQL: a statement whose
/// failure (2714, for example) does not end the frame.
pub(super) fn define(
    session: &mut Session,
    sql: &str,
    definition: Result<msduck_sql::dialect::ext::procedures::Definition, SqlError>,
    out: &mut Vec<u8>,
) -> Result<Finished, Failure> {
    let mut finished = Finished::default();
    match super::define::define(session, sql, definition) {
        Ok(()) => {
            session.last_error = 0;
            control_done(out, true, session.nocount, 222);
        }
        Err(error) => {
            let diagnostic = super::diagnostic(&error);
            if in_try(session) {
                return Err(Failure::Error(error));
            }
            finished.severity = diagnostic.severity;
            finished.error_status = diagnostic.number;
            session.last_error = emit_error(out, &error);
            more(out, 2, 222, 0);
        }
    }
    Ok(finished)
}

/// SQL Server rejects `RETURN value` in dynamic SQL and sp_executesql
/// (178) while compiling it.
pub(super) fn returns_value(statements: &[Statement]) -> bool {
    use sqlparser::ast::{Visit, Visitor};
    struct Find;
    impl Visitor for Find {
        type Break = ();
        fn pre_visit_statement(&mut self, statement: &Statement) -> std::ops::ControlFlow<()> {
            if let Statement::Return(sqlparser::ast::ReturnStatement { value: Some(_) }) = statement
                && crate::dialect::loop_control(statement).is_none()
            {
                return std::ops::ControlFlow::Break(());
            }
            std::ops::ControlFlow::Continue(())
        }
    }
    statements
        .iter()
        .any(|statement| statement.visit(&mut Find).is_break())
}
