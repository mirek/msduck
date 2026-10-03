//! Key and index columns of every type, UNIQUE NULL semantics and index options.
//!
//! - CREATE TABLE: PRIMARY KEY and UNIQUE constraints whose key columns
//!   DuckDB cannot index (Unicode carriers, DATETIME2, DATETIMEOFFSET) or
//!   that may hold NULL are enforced by keys-managed unique indexes over
//!   comparable key expressions, with SQL Server's single-NULL rule. Every
//!   constraint's SQL Server name is recorded for duplicate-key messages.
//! - CREATE INDEX: unique, clustered, INCLUDE, filtered and WITH-option
//!   indexes, and indexes over carrier columns; ordinary indexes stay with
//!   the table-owned index catalog.
//! - DROP INDEX: binds against registered indexes and key constraints, so
//!   constraint-backed indexes fail with 3723.
//! - ALTER TABLE: columns used by keys and indexes cannot be dropped or
//!   retyped (5074, 4922); the table's indexes are rebuilt around changes
//!   DuckDB refuses while they exist.
//! - INSERT, UPDATE and MERGE: DuckDB duplicate-key errors become 2627 or
//!   2601 with SQL Server's message.
//! - Comparisons, LIKE, ORDER BY, concatenation and character conversions
//!   over Unicode carrier values ([`predicates`]).
//! - The database's case-insensitive default collation: case-insensitive
//!   comparisons of literals, variables and carriers, LIKE, grouping and
//!   key indexes, and column collations (see docs/unicode-collation.md).
//!
//! The deterministic parts live in `msduck_sql::dialect::ext::keys`. See
//! docs/gaps-keys.md.
use super::Feature;
use crate::engine::{Execution, Parameter, Session};
use anyhow::Result;
use msduck_core::diagnostic::SqlError;
use sqlparser::ast::{Expr, Statement};
use std::collections::HashMap;

mod alter_table;
mod catalog;
mod create_index;
mod create_table;
mod drop_index;
mod duplicate;
mod predicates;
mod tables;

#[derive(Default)]
pub(crate) struct State {
    /// Whether the current batch is an RPC request (see [`duplicate`]).
    rpc: bool,
}

pub(super) struct Hooks;

impl Feature for Hooks {
    fn name(&self) -> &'static str {
        "keys"
    }

    fn bootstrap_database(&self, db: &duckdb::Connection) -> Result<()> {
        catalog::bootstrap(db)
    }

    fn register(&self, db: &duckdb::Connection) -> Result<()> {
        predicates::register(db)
    }

    fn rewrite_statement(
        &self,
        session: &Session,
        statement: &mut Statement,
        parameters: &HashMap<String, Parameter>,
    ) -> Result<()> {
        predicates::rewrite_statement(&session.db, statement, parameters)
    }

    fn rewrite_expr(
        &self,
        session: &Session,
        expr: &mut Expr,
        parameters: &HashMap<String, Parameter>,
    ) -> Result<()> {
        predicates::check(expr).map_err(error)?;
        predicates::rewrite_expr(&session.db, expr, parameters)
    }

    fn lower_expr(&self, expr: &mut Expr) -> Result<(), String> {
        predicates::lower_expr(expr);
        Ok(())
    }

    fn batch(
        &self,
        session: &mut Session,
        _sql: &str,
        _parameters: &HashMap<String, Parameter>,
        rpc: bool,
    ) -> Option<(Vec<u8>, bool)> {
        session.ext.keys.rpc = rpc;
        None
    }

    fn statement(
        &self,
        session: &mut Session,
        statement: &mut Statement,
        parameters: &mut HashMap<String, Parameter>,
    ) -> Result<Option<Execution>> {
        if let Some(request) = msduck_sql::drop_index_syntax::request(statement) {
            return drop_index::run(session, request).map(Some);
        }
        column_collations(statement)?;
        if let Some(execution) = alter_table::add_keys(session, statement, parameters)? {
            return Ok(Some(execution));
        }
        match statement {
            Statement::CreateTable(_) => create_table::run(session, statement, parameters),
            Statement::CreateIndex(_) => create_index::run(session, statement),
            Statement::AlterTable(_) => alter_table::run(session, statement, parameters),
            Statement::Insert(_) | Statement::Update(_) | Statement::Merge(_) => {
                duplicate::run(session, statement, parameters).map(Some)
            }
            _ => Ok(None),
        }
    }
}

/// SQL Server's errors for COLLATE clauses of new columns (447, 448).
fn column_collations(statement: &Statement) -> Result<()> {
    use msduck_sql::dialect::ext::keys::collation::column_error;
    use sqlparser::ast::AlterTableOperation;
    let columns: Vec<&sqlparser::ast::ColumnDef> = match statement {
        Statement::CreateTable(table) => table.columns.iter().collect(),
        Statement::AlterTable(alter) => alter
            .operations
            .iter()
            .filter_map(|operation| match operation {
                AlterTableOperation::AddColumn { column_def, .. } => Some(column_def),
                _ => None,
            })
            .collect(),
        _ => return Ok(()),
    };
    match columns.into_iter().find_map(column_error) {
        Some(diagnostic) => Err(error(diagnostic)),
        None => Ok(()),
    }
}

/// A SQL Server diagnostic as an error.
fn error((number, state, severity, message): (i32, u8, u8, String)) -> anyhow::Error {
    SqlError::from_utf16(number, state, severity, message.encode_utf16().collect()).into()
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

/// Create the DuckDB index enforcing (or serving) a keys-managed key.
fn build_index(
    db: &duckdb::Connection,
    table: &tables::Table,
    backend: &str,
    tag: i64,
    unique: bool,
    columns: &[msduck_sql::dialect::ext::keys::value::Column],
    filter: Option<&str>,
) -> Result<()> {
    use msduck_sql::dialect::ext::keys::value;
    let mut expressions = vec![];
    if unique {
        expressions.push(value::guard(tag, filter));
        for column in columns {
            expressions.extend(column.components().map_err(anyhow::Error::msg)?);
        }
    } else {
        for column in columns {
            let mut plain = column.clone();
            plain.nullable = false;
            expressions.extend(plain.components().map_err(anyhow::Error::msg)?);
        }
    }
    db.execute_batch(&format!(
        "CREATE {}INDEX {} ON {} ({})",
        if unique { "UNIQUE " } else { "" },
        tables::quote(backend),
        table.backend(),
        expressions.join(", ")
    ))?;
    Ok(())
}
