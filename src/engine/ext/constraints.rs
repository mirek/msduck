//! ALTER TABLE constraint lifecycle and foreign-key referential actions.
//!
//! - CHECK and FOREIGN KEY constraints are stored in `main.__msduck_constraints`
//!   and enforced around DML (`enforce`), so they can be added and dropped,
//!   disabled with NOCHECK, trusted or not, and so foreign keys can CASCADE,
//!   SET NULL and SET DEFAULT, which DuckDB does not support.
//! - PRIMARY KEY and UNIQUE stay native DuckDB constraints; their names are
//!   recorded so they can be dropped (`rebuild`).
//! - DEFAULT constraints use the built-in named-default store.
//!
//! See docs/gaps-constraints.md for behavior and limits.
use super::Feature;
use crate::engine::{Execution, Parameter, Session};
use anyhow::Result;
use sqlparser::ast::{ObjectType, Statement};
use std::collections::HashMap;

mod catalog;
mod define;
mod enforce;
mod errors;
mod guard;
mod rebuild;
mod translate;

#[derive(Default)]
pub(crate) struct State;

pub(super) struct Hooks;

/// Whether the native connection is inside an explicit transaction that
/// msduck did not count (two statements see the same transaction id).
fn native_transaction(db: &duckdb::Connection) -> Result<bool> {
    let first: u64 = db.query_row("SELECT txid_current()", [], |row| row.get(0))?;
    let second: u64 = db.query_row("SELECT txid_current()", [], |row| row.get(0))?;
    Ok(first == second)
}

/// A statement-level transaction: owned when the session is in autocommit
/// mode, otherwise the caller's transaction.
pub(crate) struct Transaction {
    pub owned: bool,
}

impl Transaction {
    pub fn begin(session: &mut Session) -> Result<Self> {
        if session.transactions == 0 && !native_transaction(&session.db)? {
            session.db.execute_batch("BEGIN TRANSACTION")?;
            // Engine paths reached from here must not open their own.
            session.transactions += 1;
            Ok(Self { owned: true })
        } else {
            Ok(Self { owned: false })
        }
    }

    /// Commit or roll back an owned transaction.
    pub fn finish<T>(self, session: &mut Session, result: Result<T>) -> Result<T> {
        if !self.owned {
            return result;
        }
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

    /// As [`Transaction::finish`]; in the caller's transaction, a failure
    /// after this statement wrote invalidates the native transaction, so its
    /// partial effects can never commit.
    pub fn finish_or_abort<T>(
        self,
        session: &mut Session,
        result: Result<T>,
        wrote: bool,
    ) -> Result<T> {
        if !self.owned && result.is_err() && wrote {
            invalidate(session);
        }
        self.finish(session, result)
    }
}

/// Make the native transaction fail, like a failed native statement does.
pub(crate) fn invalidate(session: &Session) {
    let _ = session.db.execute_batch(
        "SELECT error('msduck: the statement was rolled back after a constraint failure')",
    );
}

impl Feature for Hooks {
    fn name(&self) -> &'static str {
        "constraints"
    }

    fn bootstrap_database(&self, db: &duckdb::Connection) -> Result<()> {
        catalog::bootstrap(db)
    }

    fn statement(
        &self,
        session: &mut Session,
        statement: &mut Statement,
        parameters: &mut HashMap<String, Parameter>,
    ) -> Result<Option<Execution>> {
        use msduck_sql::dialect::ext::constraints::decode;
        if let Some(alter) = decode(statement) {
            return define::alter(session, alter?, parameters).map(Some);
        }
        match statement {
            Statement::CreateTable(_) => define::create_table(session, statement, parameters),
            Statement::Drop {
                object_type: ObjectType::Table,
                ..
            } => guard::drop_tables(session, statement, parameters),
            Statement::Truncate(_) => {
                guard::truncate(session, statement)?;
                Ok(None)
            }
            Statement::AlterTable(_) => {
                guard::alter_table(session, statement)?;
                Ok(None)
            }
            _ => enforce::dml(session, statement, parameters),
        }
    }
}
