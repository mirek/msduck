//! Catalog effect for resolving a parsed SET IDENTITY_INSERT target.
//!
//! The parser and session transition live in msduck-sql. This adapter reads the
//! current database's live catalog and returns a stable object id before any
//! session setting may be changed.

use duckdb::Connection;
use msduck_core::diagnostic::SqlError;
use sqlparser::ast::Ident;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedTable {
    pub object_id: i32,
    pub schema: String,
    pub table: String,
    /// Zero-based physical position used by INSERT source binding.
    pub identity_position: usize,
}

#[derive(Debug)]
pub enum ResolveError {
    Diagnostic { error: SqlError, done_command: u16 },
    Unsupported(&'static str),
    InconsistentCatalog(&'static str),
    Backend(duckdb::Error),
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Diagnostic { error, .. } => error.fmt(f),
            Self::Unsupported(message) | Self::InconsistentCatalog(message) => f.write_str(message),
            Self::Backend(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for ResolveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Diagnostic { error, .. } => Some(error),
            Self::Backend(error) => Some(error),
            _ => None,
        }
    }
}

impl From<duckdb::Error> for ResolveError {
    fn from(error: duckdb::Error) -> Self {
        Self::Backend(error)
    }
}

fn diagnostic(number: i32, state: u8, message: String) -> ResolveError {
    ResolveError::Diagnostic {
        error: SqlError::new(number, state, message),
        done_command: 253,
    }
}

/// Resolve only an ordinary one- or two-part table name in the current
/// database. All requested names are bound parameters; no part becomes SQL.
pub fn resolve(db: &Connection, parts: &[Ident]) -> Result<ResolvedTable, ResolveError> {
    let (schema, table) = match parts {
        [table] => ("dbo", table.value.as_str()),
        [schema, table] => (schema.value.as_str(), table.value.as_str()),
        _ => {
            return Err(ResolveError::Unsupported(
                "unsupported IDENTITY_INSERT target parts",
            ));
        }
    };
    if schema.is_empty() || table.is_empty() {
        return Err(ResolveError::Unsupported(
            "empty IDENTITY_INSERT target part",
        ));
    }
    let display = format!("{schema}.{table}");
    let mut statement = db.prepare(
        "SELECT t.object_id,s.name,t.name,i.column_id,c.ordinal_position \
         FROM sys.tables t JOIN sys.schemas s ON s.schema_id=t.schema_id \
         LEFT JOIN sys.identity_columns i ON i.object_id=t.object_id \
         LEFT JOIN information_schema.columns c ON c.table_catalog=current_database() \
           AND lower(c.table_schema)=lower(s.name) AND lower(c.table_name)=lower(t.name) \
           AND lower(c.column_name)=lower(i.name) \
         WHERE lower(s.name)=lower(?) AND lower(t.name)=lower(?)",
    )?;
    let mut rows = statement.query([schema, table])?;
    let Some(row) = rows.next()? else {
        return Err(diagnostic(
            1088,
            11,
            format!(
                "Cannot find the object \"{display}\" because it does not exist or you do not have permissions."
            ),
        ));
    };
    let object_id: i32 = row.get(0)?;
    let resolved_schema: String = row.get(1)?;
    let resolved_table: String = row.get(2)?;
    let identity_column: Option<i32> = row.get(3)?;
    let ordinal: Option<i32> = row.get(4)?;
    if rows.next()?.is_some() {
        return Err(ResolveError::InconsistentCatalog(
            "multiple identity columns for one table",
        ));
    }
    let Some(_identity_column) = identity_column else {
        return Err(diagnostic(
            8106,
            1,
            format!(
                "Table '{display}' does not have the identity property. Cannot perform SET operation."
            ),
        ));
    };
    let identity_position = ordinal
        .and_then(|value| value.checked_sub(1))
        .and_then(|value| usize::try_from(value).ok())
        .ok_or(ResolveError::InconsistentCatalog(
            "identity column has no physical ordinal",
        ))?;
    Ok(ResolvedTable {
        object_id,
        schema: resolved_schema,
        table: resolved_table,
        identity_position,
    })
}
