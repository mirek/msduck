//! Isolation levels, SAVE TRANSACTION, named transactions, WAITFOR and
//! DBCC USEROPTIONS. Behavior, evidence and limits: docs/gaps-transactions.md.
//!
//! - [`options`]: the session isolation level (SET TRANSACTION ISOLATION
//!   LEVEL and transaction-manager begin requests), `sys.dm_exec_sessions`
//!   and DBCC USEROPTIONS.
//! - [`savepoints`]: SAVE TRANSACTION and ROLLBACK TRANSACTION to a
//!   savepoint, emulated with before-images because DuckDB has no
//!   savepoints.
//! - [`waitfor`]: WAITFOR DELAY and WAITFOR TIME.
//! - [`snapshot`]: ALTER DATABASE ... SET ALLOW_SNAPSHOT_ISOLATION, and the
//!   3952 and 3960 errors of SNAPSHOT transactions.
use super::{
    super::{Execution, Parameter, Session},
    Feature,
};
use anyhow::Result;
use msduck_core::diagnostic::SqlError;
use msduck_sql::dialect::ext::transactions::{self as syntax, Name, Request};
use sqlparser::ast::{Set, Statement};
use std::collections::HashMap;

mod options;
mod savepoints;
mod snapshot;
mod waitfor;

pub(crate) struct State {
    /// TDS numbering: 1 read uncommitted, 2 read committed, 3 repeatable
    /// read, 4 serializable, 5 snapshot.
    isolation: u8,
    /// One entry per running batch: the level to restore when it ends
    /// (for RPCs and procedure bodies), or `None`.
    batch_isolation: Vec<Option<u8>>,
    savepoints: savepoints::Stack,
    /// The SNAPSHOT write that `snapshot::write` is running through the
    /// ordinary path. Its own re-entry skips this feature once; statements
    /// nested in it (trigger bodies) are handled as usual.
    resumed: Option<Statement>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            isolation: options::READ_COMMITTED,
            batch_isolation: Vec::new(),
            savepoints: Default::default(),
            resumed: None,
        }
    }
}

/// The session's isolation level (TDS numbering), for frames that restore
/// it when they return, such as procedure bodies and dynamic SQL.
pub(super) fn isolation(session: &Session) -> u8 {
    session.ext.transactions.isolation
}

pub(super) fn restore_isolation(session: &mut Session, isolation: u8) {
    if session.ext.transactions.isolation != isolation {
        options::set(session, isolation);
    }
}

pub(super) struct Hooks;

impl Feature for Hooks {
    fn name(&self) -> &'static str {
        "transactions"
    }

    fn batch(
        &self,
        session: &mut Session,
        sql: &str,
        _parameters: &HashMap<String, Parameter>,
        rpc: bool,
    ) -> Option<(Vec<u8>, bool)> {
        snapshot::track_module_definition(session, sql);
        compile_error(session, sql, rpc)
    }

    fn statement(
        &self,
        session: &mut Session,
        statement: &mut Statement,
        parameters: &mut HashMap<String, Parameter>,
    ) -> Result<Option<Execution>> {
        if let Some(request) = syntax::request(statement) {
            return execute(session, request, parameters).map(Some);
        }
        if let Some(request) = msduck_sql::dialect::alter_database::request(statement)
            && !request.snapshot_isolation.is_empty()
        {
            return snapshot::alter(session, request).map(Some);
        }
        match statement {
            Statement::Set(Set::SetTransaction {
                modes,
                snapshot: None,
                session: false,
            }) => options::set_statement(session, modes).map(Some),
            Statement::Rollback {
                chain: false,
                savepoint: Some(name),
            } => savepoints::rollback_statement(session, &name.value),
            _ => {
                if let Some(resumed) = session.ext.transactions.resumed.take()
                    && resumed == *statement
                {
                    return Ok(None);
                }
                let snapshot = snapshot::active(session);
                if snapshot {
                    snapshot::check_access(session, statement)?;
                }
                snapshot::track_write(session, statement)?;
                savepoints::before_statement(session, statement)?;
                if snapshot {
                    return snapshot::write(session, statement, parameters);
                }
                Ok(None)
            }
        }
    }

    fn isolation(&self, isolation: u8) -> Option<Result<()>> {
        // 0 keeps the session's level; 1-5 are SQL Server's levels. DuckDB
        // runs every one of them as snapshot isolation (see options.rs).
        (isolation <= 5).then_some(Ok(()))
    }

    fn save_transaction(&self, session: &mut Session, name: &str) -> Option<Result<Vec<u8>>> {
        Some(savepoints::save_request(session, name))
    }

    fn rollback_to(&self, session: &mut Session, name: &str) -> Option<Result<Vec<u8>>> {
        savepoints::rollback_request(session, name)
    }

    fn batch_begin(&self, session: &mut Session, rpc: bool) {
        options::batch_begin(session, rpc);
    }

    fn batch_end(&self, session: &mut Session) {
        options::batch_end(session);
        if session.transactions == 0 {
            snapshot::end(session);
        }
    }

    fn transaction_begin(&self, session: &mut Session, isolation: u8) {
        options::begin_request(session, isolation);
        if session.transactions == 1 {
            snapshot::begin(session);
        }
    }

    fn transaction_end(&self, session: &mut Session, _committed: bool) {
        savepoints::release_all(session);
        snapshot::end(session);
    }

    fn session_end(&self, session: &mut Session) {
        snapshot::end(session);
    }

    fn session_start(&self, session: &mut Session) -> Result<()> {
        options::publish(session);
        Ok(())
    }
}

/// Report a compile-time diagnostic of this feature (148, 103, or 102 with
/// SQL Server's wording, or 5062 for conflicting ALLOW_SNAPSHOT_ISOLATION
/// values) before any statement of the batch runs, as SQL Server does.
/// Batches that cannot contain such a statement are not parsed twice.
fn compile_error(session: &mut Session, sql: &str, rpc: bool) -> Option<(Vec<u8>, bool)> {
    let upper = sql.to_ascii_uppercase();
    let snapshot = upper.contains("ALLOW_SNAPSHOT_ISOLATION");
    if !upper.contains("WAITFOR") && !upper.contains("TRAN") && !snapshot {
        return None;
    }
    // A batch that does not parse fails through the engine's own path.
    let statements = msduck_sql::batch::parse(sql).ok()?;
    let error = syntax::compile_error(&statements)
        .or_else(|| snapshot.then(|| snapshot::conflicting_values(&statements))?)?;
    let mut out = Vec::new();
    crate::tds::sql_error(&mut out, &error);
    crate::tds::done(&mut out, if rpc { 0xfe } else { 0xfd }, 2, 0, 0);
    session.last_error = error.number;
    Some((out, false))
}

fn execute(
    session: &mut Session,
    request: Request<'_>,
    parameters: &mut HashMap<String, Parameter>,
) -> Result<Execution> {
    match request {
        Request::Begin(name) => {
            let name = transaction_name(&name, parameters)?.unwrap_or_default();
            let tokens = session.begin_transaction(0, &name)?;
            Ok(Execution::statement(tokens, None, 0))
        }
        Request::Commit(name) => {
            // SQL Server ignores the name, but still checks its type.
            transaction_name(&name, parameters)?;
            let tokens = session.commit_transaction()?;
            Ok(Execution::statement(tokens, None, 0))
        }
        Request::Rollback(name) => match transaction_name(&name, parameters)? {
            // A NULL name rolls back the whole transaction.
            None => {
                let tokens = session.rollback_transaction("")?;
                Ok(Execution::statement(tokens, None, 0))
            }
            Some(name) if name.is_empty() && session.transactions > 0 => Err(SqlError::new(
                6401,
                2,
                "Cannot roll back . No transaction or savepoint of that name was found.",
            )
            .into()),
            Some(name) => match savepoints::rollback_statement(session, &name)? {
                Some(execution) => Ok(execution),
                None => {
                    let tokens = session.rollback_transaction(&name)?;
                    Ok(Execution::statement(tokens, None, 0))
                }
            },
        },
        Request::Save(name) => {
            let name = transaction_name(&name, parameters)?;
            savepoints::save_statement(session, name)?;
            Ok(Execution::statement(Vec::new(), None, 0))
        }
        Request::WaitFor(wait, value) => waitfor::run(session, wait, value, parameters),
        Request::UserOptions { no_infomsgs } => options::user_options(session, no_infomsgs),
        Request::Error(error) => Err(error.into()),
    }
}

/// The value of a transaction or savepoint name: a literal, or a character
/// variable truncated to 32 characters (`None` when it is NULL). Trailing
/// blanks never matter in comparisons, so they are kept as given.
fn transaction_name(
    name: &Name<'_>,
    parameters: &HashMap<String, Parameter>,
) -> Result<Option<String>> {
    use msduck_core::{types::Type, value::Value};
    let variable = match name {
        Name::Literal(name) => return Ok(Some((*name).to_owned())),
        Name::Variable(variable) => variable.to_string(),
    };
    let parameter = parameters.get(&variable.to_lowercase()).ok_or_else(|| {
        SqlError::syntax(
            137,
            2,
            format!("Must declare the scalar variable \"{variable}\"."),
        )
    })?;
    if !matches!(parameter.data_type, Type::Character(_)) {
        return Err(SqlError::new(
            3914,
            0,
            format!(
                "The data type \"{}\" is invalid for transaction names or savepoint names. Allowed data types are char, varchar, nchar, varchar(max), nvarchar, and nvarchar(max).",
                waitfor::type_name(parameter.data_type)
            ),
        )
        .into());
    }
    let text = match &parameter.value {
        Value::Null => return Ok(None),
        Value::Text(text) => text.clone(),
        Value::Unicode(units) => String::from_utf16_lossy(units),
        other => anyhow::bail!("unexpected character variable value {other:?}"),
    };
    Ok(Some(text.chars().take(syntax::NAME_LIMIT).collect()))
}
