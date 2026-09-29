//! Root effect adapter for a parsed SET IDENTITY_INSERT operation.
//!
//! The SQL rule and catalog resolver are path-imported while their crate export
//! and the root engine entry point are reserved by other workers.

#[path = "../crates/msduck-sql/src/identity_insert.rs"]
mod binding;
#[path = "identity_insert_catalog.rs"]
mod catalog;

use duckdb::Connection;
use msduck_core::diagnostic::SqlError;
use sqlparser::ast::Statement;

pub use binding::{SessionState, Transition};
pub use catalog::ResolvedTable;

/// Database identity and the catalog object's persistent identity are the
/// session key. Spelling and column position are retained for diagnostics and
/// later INSERT binding, but aliases do not create different session keys.
#[derive(Clone, Debug)]
pub struct TableKey {
    pub database_id: i64,
    pub database_name: String,
    pub table: ResolvedTable,
}

impl PartialEq for TableKey {
    fn eq(&self, other: &Self) -> bool {
        self.database_id == other.database_id && self.table.object_id == other.table.object_id
    }
}

impl Eq for TableKey {}

pub type State = SessionState<TableKey>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Applied {
    pub transition: Transition,
    pub done_command: u16,
    pub target: TableKey,
}

#[derive(Debug)]
pub enum ApplyError {
    Diagnostic { error: SqlError, done_command: u16 },
    Unsupported(&'static str),
    InconsistentCatalog(&'static str),
    Backend(duckdb::Error),
}

impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Diagnostic { error, .. } => error.fmt(f),
            Self::Unsupported(message) | Self::InconsistentCatalog(message) => f.write_str(message),
            Self::Backend(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for ApplyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Diagnostic { error, .. } => Some(error),
            Self::Backend(error) => Some(error),
            _ => None,
        }
    }
}

impl From<catalog::ResolveError> for ApplyError {
    fn from(value: catalog::ResolveError) -> Self {
        match value {
            catalog::ResolveError::Diagnostic {
                error,
                done_command,
            } => Self::Diagnostic {
                error,
                done_command,
            },
            catalog::ResolveError::Unsupported(message) => Self::Unsupported(message),
            catalog::ResolveError::InconsistentCatalog(message) => {
                Self::InconsistentCatalog(message)
            }
            catalog::ResolveError::Backend(error) => Self::Backend(error),
        }
    }
}

/// Apply one typed SET operation after a live catalog lookup. The caller owns
/// both the state and the SQL-visible database identity/name; neither is read
/// from an ambient process setting. Other statements leave the state untouched.
pub fn apply(
    db: &Connection,
    database_id: i64,
    database_name: &str,
    state: &mut State,
    statement: &Statement,
) -> Result<Option<Applied>, ApplyError> {
    let operation = binding::operation(statement)
        .map_err(|_| ApplyError::Unsupported("unsupported IDENTITY_INSERT target"))?;
    let Some(operation) = operation else {
        return Ok(None);
    };
    if database_name.is_empty() {
        return Err(ApplyError::Unsupported("empty database name"));
    }
    let table = catalog::resolve(db, &operation.parts)?;
    let target = TableKey {
        database_id,
        database_name: database_name.to_owned(),
        table,
    };
    let transition = match state.apply(&operation, target.clone()) {
        Ok(transition) => transition,
        Err(conflict) => {
            let requested = operation
                .parts
                .iter()
                .map(|part| part.value.as_str())
                .collect::<Vec<_>>()
                .join(".");
            let active = conflict.active;
            return Err(ApplyError::Diagnostic {
                error: SqlError::new(
                    8107,
                    1,
                    format!(
                        "IDENTITY_INSERT is already ON for table '{}.{}.{}'. Cannot perform SET operation for table '{}'.",
                        active.database_name, active.table.schema, active.table.table, requested
                    ),
                ),
                done_command: 253,
            });
        }
    };
    Ok(Some(Applied {
        transition,
        done_command: if operation.enabled { 183 } else { 184 },
        target,
    }))
}
