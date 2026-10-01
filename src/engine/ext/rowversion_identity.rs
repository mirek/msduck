//! rowversion/timestamp columns, decimal identity, SCOPE_IDENTITY and IDENTITY_INSERT.
//!
//! - [`rowversion`]: `rowversion` and `timestamp` columns are binary(8)
//!   columns filled from a database-wide counter (`main.__msduck_rowversion`)
//!   on INSERT and on every UPDATE of the row. Explicit values fail with 273
//!   and 272, a second such column with 2738; `@@DBTS` and
//!   `MIN_ACTIVE_ROWVERSION()` read the counter.
//! - [`decimal`]: `decimal(p,0)`/`numeric(p,0)` IDENTITY columns use the same
//!   private sequences as the integer identity columns (src/identity.rs), and
//!   other identity types fail with 2749.
//! - [`scope`]: `SCOPE_IDENTITY()` and `@@IDENTITY` record the last identity
//!   value an INSERT of this session produced, as numeric(38,0).
//! - [`insert`]: `SET IDENTITY_INSERT` with SQL Server's rules, explicit
//!   identity values and the allocator advance past them.
//!
//! RPC requests (sp_executesql, prepared execution) and module bodies such
//! as triggers start with the caller's IDENTITY_INSERT table and an empty
//! identity scope, and leave both of the caller's unchanged, as SQL Server
//! does; `@@IDENTITY` is session-wide. See
//! docs/gaps-rowversion_identity.md.
use super::Feature;
use crate::engine::{Execution, Parameter, Session};
use anyhow::Result;
use sqlparser::ast::{Expr, Statement};
use std::collections::HashMap;

mod decimal;
mod insert;
mod names;
mod rowversion;
mod scope;
mod table;

// The deterministic IDENTITY_INSERT rules (msduck-sql) and their catalog
// adapters are path-imported, as their own tests do.
#[allow(dead_code)]
#[path = "../../identity_insert_write.rs"]
mod write;

const NAME: &str = "rowversion_identity";

#[derive(Default)]
pub(crate) struct State {
    /// Open batches, innermost last (see [`Feature::batch_begin`]).
    frames: Vec<Frame>,
    /// The current scope's SET IDENTITY_INSERT table.
    insert: write::session::State,
    /// The current scope's SCOPE_IDENTITY().
    scope: Option<i128>,
    /// @@IDENTITY.
    last: Option<i128>,
    /// Counts recorded INSERTs, so an INSERT can tell whether a nested one
    /// (in a trigger) set @@IDENTITY while it ran.
    recorded: u64,
}

/// An open batch. An RPC request, a trigger body or another module body is
/// a scope of its own: it starts with the caller's IDENTITY_INSERT table and
/// no SCOPE_IDENTITY(), and the caller's are restored when it ends.
struct Frame {
    saved: Option<(write::session::State, Option<i128>)>,
}

impl State {
    fn insert(&self) -> &write::session::State {
        &self.insert
    }

    fn insert_mut(&mut self) -> &mut write::session::State {
        &mut self.insert
    }

    fn scope(&self) -> Option<i128> {
        self.scope
    }

    /// An INSERT completed: `value` is its last identity value, or None for
    /// a table without an identity column. `before` is [`State::recorded`]
    /// when the INSERT started: an INSERT nested in it (in a trigger) that
    /// recorded since then keeps its @@IDENTITY, as in SQL Server.
    fn record(&mut self, value: Option<i128>, before: u64) {
        self.scope = value;
        if self.recorded == before {
            self.last = value;
        }
        self.recorded += 1;
    }
}

pub(super) struct Hooks;

impl Feature for Hooks {
    fn name(&self) -> &'static str {
        NAME
    }

    fn register(&self, db: &duckdb::Connection) -> Result<()> {
        rowversion::register(db)?;
        insert::register(db)
    }

    fn bootstrap_database(&self, db: &duckdb::Connection) -> Result<()> {
        rowversion::bootstrap(db)
    }

    fn batch_begin(&self, session: &mut Session, rpc: bool) {
        let state = &mut session.ext.rowversion_identity;
        let saved = rpc.then(|| (state.insert.fork_rpc(), state.scope.take()));
        state.frames.push(Frame { saved });
    }

    fn batch_end(&self, session: &mut Session) {
        let state = &mut session.ext.rowversion_identity;
        if let Some(Frame {
            saved: Some((insert, scope)),
        }) = state.frames.pop()
        {
            state.insert = insert;
            state.scope = scope;
        }
    }

    fn statement(
        &self,
        session: &mut Session,
        statement: &mut Statement,
        parameters: &mut HashMap<String, Parameter>,
    ) -> Result<Option<Execution>> {
        match statement {
            Statement::Set(_) => insert::set(session, statement),
            Statement::CreateTable(_) => table::create(session, statement, parameters),
            Statement::AlterTable(_) => rowversion::alter_table(session, statement, parameters),
            Statement::Insert(_) => insert::run(session, statement, parameters),
            Statement::Update(_) => {
                rowversion::update(session, statement)?;
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    fn rewrite_expr(
        &self,
        session: &Session,
        expr: &mut Expr,
        _parameters: &HashMap<String, Parameter>,
    ) -> Result<()> {
        scope::rewrite(session, expr)?;
        rowversion::rewrite(session, expr)
    }

    fn session_end(&self, session: &mut Session) {
        insert::forget(session.ext.token);
    }
}

/// Run `work` in a transaction: the caller's, or one owned here that commits
/// on success and rolls back on failure. The engine sees the owned
/// transaction as the caller's, so nested statements do not commit it.
fn atomically<T>(session: &mut Session, work: impl FnOnce(&mut Session) -> Result<T>) -> Result<T> {
    if session.transactions > 0 {
        return work(session);
    }
    session.db.execute_batch("BEGIN TRANSACTION")?;
    session.transactions += 1;
    let result = work(session);
    session.transactions -= 1;
    match result {
        Ok(value) => match session.db.execute_batch("COMMIT") {
            Ok(()) => Ok(value),
            Err(error) => {
                let _ = session.db.execute_batch("ROLLBACK");
                Err(error.into())
            }
        },
        Err(error) => {
            let _ = session.db.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

/// Execute `statement` through the ordinary engine path, with this feature's
/// statement hook suspended.
fn execute(
    session: &mut Session,
    statement: Statement,
    parameters: &mut HashMap<String, Parameter>,
) -> Result<Execution> {
    super::reenter(session, NAME, |session| {
        session.execute(statement, parameters)
    })
}

/// A SQL Server diagnostic (severity 16) as an error.
fn error(number: i32, state: u8, message: String) -> anyhow::Error {
    msduck_core::diagnostic::SqlError::new(number, state, message).into()
}

/// A DEFAULT definition error, followed by SQL Server's 1750.
fn constraint_error(number: i32, message: String) -> anyhow::Error {
    use msduck_core::diagnostic::SqlError;
    crate::engine::StatementErrors(vec![
        SqlError::new(number, 0, message),
        SqlError::new(
            1750,
            0,
            "Could not create constraint or index. See previous errors.",
        ),
    ])
    .into()
}
