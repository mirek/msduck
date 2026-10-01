//! Bulk load: INSERT BULK and the BulkLoadBCP message (packet type 0x07)
//! that follows it, as SqlBulkCopy, tedious `newBulkLoad`, mssql
//! `request.bulk` and bcp send them.
//!
//! - The `batch` hook binds an INSERT BULK statement (which must be alone in
//!   its batch, 428) to its target table ([`plan`]) and keeps the plan until
//!   the next message.
//! - The server hands the next BulkLoadBCP message to the session packet by
//!   packet ([`Session::bulk_load_packet`]); [`load`] decodes the rows with
//!   the shared codec ([`wire`]), checks the column metadata like SQL Server
//!   (4816, 4804) and inserts the rows through the engine, so keys, NOT NULL,
//!   identity, conversions, triggers and transactions behave as for INSERT.
//! - Any other request while a load is expected fails with 4022, as in SQL
//!   Server.
//!
//! See docs/gaps-bulk.md for the captured behavior and the remaining limits.
use super::Feature;
use crate::{
    engine::{Parameter, Session},
    tds,
};
use msduck_core::diagnostic::SqlError;
use msduck_sql::dialect::ext::bulk as syntax;
use sqlparser::ast::Statement;
use std::collections::HashMap;

mod load;
mod plan;
mod wire;

const NAME: &str = "bulk";

#[derive(Default)]
pub(crate) struct State {
    /// The bound INSERT BULK statement, waiting for its BulkLoadBCP message.
    pending: Option<plan::Plan>,
    /// The BulkLoadBCP message being received.
    load: Option<load::Load>,
}

pub(super) struct Hooks;

/// The diagnostics and failed DONE of a refused INSERT BULK statement.
fn refuse(session: &mut Session, errors: &[SqlError]) -> (Vec<u8>, bool) {
    let mut out = Vec::new();
    for error in errors {
        tds::sql_error(&mut out, error);
    }
    if let Some(error) = errors.last() {
        session.last_error = error.number;
    }
    session.rowcount = 0;
    tds::done(&mut out, 0xfd, 2, 253, 0);
    (out, false)
}

/// The message of one of the INSERT BULK parser's 102 errors, as the batch
/// parser returns it: the raw parser error, or the 102 diagnostic the
/// procedures syntax derives from any "Incorrect syntax near" parser error
/// (`msduck_sql::dialect::ext::procedures::diagnostic`).
fn syntax_message(error: &anyhow::Error) -> Option<String> {
    use sqlparser::parser::ParserError;
    if let Some(error) = error.downcast_ref::<SqlError>() {
        let parsed = ParserError::ParserError(error.message.clone());
        return (error.number == 102 && syntax::is_syntax_error(&parsed))
            .then(|| error.message.clone());
    }
    error
        .downcast_ref::<ParserError>()
        .filter(|error| syntax::is_syntax_error(error))
        .map(|error| match error {
            ParserError::ParserError(message) => message.clone(),
            other => other.to_string(),
        })
}

fn multi_statement() -> SqlError {
    SqlError::new(
        428,
        1,
        "Insert bulk cannot be used in a multi-statement batch.",
    )
}

impl Feature for Hooks {
    fn name(&self) -> &'static str {
        NAME
    }

    fn batch(
        &self,
        session: &mut Session,
        sql: &str,
        _parameters: &HashMap<String, Parameter>,
        rpc: bool,
    ) -> Option<(Vec<u8>, bool)> {
        if !sql
            .as_bytes()
            .windows(4)
            .any(|window| window.eq_ignore_ascii_case(b"BULK"))
        {
            return None;
        }
        let statements = match msduck_sql::batch::parse(sql) {
            Ok(statements) => statements,
            Err(error) => {
                // A syntax error inside INSERT BULK keeps SQL Server's 102.
                let leading = msduck_sql::dialect::ext::leading_words(sql, 2);
                let message = syntax_message(&error);
                return match message {
                    Some(message) if !rpc && leading == ["INSERT", "BULK"] => {
                        Some(refuse(session, &[SqlError::syntax(102, 1, message)]))
                    }
                    _ => None,
                };
            }
        };
        let carriers = statements
            .iter()
            .filter(|statement| syntax::decode(statement).is_some())
            .count();
        if carriers == 0 {
            return None;
        }
        if rpc {
            // Not probed against SQL Server; refuse explicitly.
            let mut out = Vec::new();
            tds::sql_error(
                &mut out,
                &SqlError::new(
                    40515,
                    1,
                    "INSERT BULK is supported only in a SQL batch request",
                ),
            );
            session.last_error = 40515;
            out.push(0x79);
            out.extend(1i32.to_le_bytes());
            tds::done(&mut out, 0xfe, 2, 224, 0);
            return Some((out, false));
        }
        if statements.len() > 1 {
            return Some(refuse(session, &[multi_statement()]));
        }
        let statement = match syntax::decode(&statements[0]).expect("an INSERT BULK carrier") {
            Ok(statement) => statement,
            Err(error) => {
                let message = match error {
                    sqlparser::parser::ParserError::ParserError(message) => message,
                    other => other.to_string(),
                };
                return Some(refuse(session, &[SqlError::syntax(102, 1, message)]));
            }
        };
        session.ext.bulk.pending = None;
        session.ext.bulk.load = None;
        Some(match plan::prepare(session, statement) {
            Ok(plan) => {
                session.ext.bulk.pending = Some(plan);
                session.last_error = 0;
                let mut out = Vec::new();
                tds::done(&mut out, 0xfd, 0, 253, 0);
                (out, true)
            }
            Err(plan::Refusal::Errors(errors)) => refuse(session, &errors),
            Err(plan::Refusal::Silent) => refuse(session, &[]),
            Err(plan::Refusal::Other(error)) => {
                let mut out = Vec::new();
                session.last_error = crate::engine::emit_error(&mut out, &error);
                tds::done(&mut out, 0xfd, 2, 253, 0);
                (out, false)
            }
        })
    }

    fn statement(
        &self,
        _session: &mut Session,
        statement: &mut Statement,
        _parameters: &mut HashMap<String, Parameter>,
    ) -> anyhow::Result<Option<crate::engine::Execution>> {
        // INSERT BULK nested in a block or a module body.
        if syntax::decode(statement).is_some() {
            return Err(multi_statement().into());
        }
        Ok(None)
    }

    fn session_end(&self, session: &mut Session) {
        session.ext.bulk.pending = None;
        session.ext.bulk.load = None;
    }
}

/// The SQL Server text of 4022.
const NOT_SENT: &str = "Bulk load data was expected but not sent. The batch will be terminated.";

impl Session {
    /// Whether an INSERT BULK statement is waiting for its BulkLoadBCP
    /// message, or that message is being received.
    pub fn bulk_load_expected(&self) -> bool {
        self.ext.bulk.pending.is_some() || self.ext.bulk.load.is_some()
    }

    /// One packet of the BulkLoadBCP message (packet type 0x07); `last` is
    /// its end-of-message packet.
    pub fn bulk_load_packet(&mut self, data: &[u8], last: bool) {
        if self.ext.bulk.load.is_none() {
            let Some(plan) = self.ext.bulk.pending.take() else {
                return;
            };
            self.ext.bulk.load = Some(load::Load::new(plan));
        }
        let Some(mut load) = self.ext.bulk.load.take() else {
            return;
        };
        load.push(self, data, last);
        self.ext.bulk.load = Some(load);
    }

    /// The BulkLoadBCP message ended: load its rows and return the response.
    /// `ignored` is the packet IGNORE bit (the client abandoned the message).
    /// A message without a preceding INSERT BULK fails without a diagnostic,
    /// as in SQL Server.
    pub fn bulk_load_finish(&mut self, ignored: bool) -> Vec<u8> {
        self.ext.bulk.pending = None;
        match self.ext.bulk.load.take() {
            Some(load) => load.finish(self, ignored),
            None => {
                let mut out = Vec::new();
                tds::done(&mut out, 0xfd, 2, 0, 0);
                out
            }
        }
    }

    /// Another request arrived instead of the expected BulkLoadBCP message:
    /// SQL Server terminates that request with 4022 and forgets the INSERT
    /// BULK statement.
    pub fn bulk_load_missing(&mut self) -> Vec<u8> {
        self.ext.bulk.pending = None;
        self.ext.bulk.load = None;
        let mut out = Vec::new();
        tds::sql_error(&mut out, &SqlError::new(4022, 1, NOT_SENT));
        self.last_error = 4022;
        tds::done(&mut out, 0xfd, 2, 253, 0);
        out
    }

    /// An attention signal cancels an expected load.
    pub fn bulk_load_cancel(&mut self) {
        self.ext.bulk.pending = None;
        self.ext.bulk.load = None;
    }
}
