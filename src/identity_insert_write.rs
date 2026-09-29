//! Live catalog adapter for the deterministic IDENTITY_INSERT INSERT gate.
//!
//! This preflight performs bound catalog reads only. Source evaluation, row
//! writes, identity allocation and result emission belong to the engine.

#[path = "../crates/msduck-sql/src/identity_insert_gate.rs"]
mod gate;
#[path = "identity_insert_session.rs"]
pub mod session;

use duckdb::Connection;
use msduck_core::diagnostic::SqlError;
use sqlparser::ast::{ObjectName, Statement, TableObject};

pub use gate::Permit;

#[derive(Debug)]
pub enum PreflightError {
    Diagnostic { error: SqlError, done_command: u16 },
    Unsupported(&'static str),
    InconsistentCatalog(&'static str),
    Backend(duckdb::Error),
}

impl std::fmt::Display for PreflightError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Diagnostic { error, .. } => error.fmt(f),
            Self::Unsupported(message) | Self::InconsistentCatalog(message) => f.write_str(message),
            Self::Backend(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for PreflightError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Diagnostic { error, .. } => Some(error),
            Self::Backend(error) => Some(error),
            _ => None,
        }
    }
}

impl From<gate::GateError> for PreflightError {
    fn from(value: gate::GateError) -> Self {
        match value {
            gate::GateError::Diagnostic {
                error,
                done_command,
            } => Self::Diagnostic {
                error,
                done_command,
            },
            gate::GateError::Unsupported(message) => Self::Unsupported(message),
        }
    }
}

impl From<duckdb::Error> for PreflightError {
    fn from(value: duckdb::Error) -> Self {
        Self::Backend(value)
    }
}

/// Return None for non-INSERT statements. Missing or nonidentity tables return
/// NotApplicable so the ordinary INSERT binder reports its own error or writes
/// the nonidentity table. No SET-specific 1088/8106 error is invented here.
pub fn preflight(
    db: &Connection,
    database_id: i64,
    database_name: &str,
    state: &session::State,
    statement: &Statement,
) -> Result<Option<Permit>, PreflightError> {
    let Statement::Insert(insert) = statement else {
        return Ok(None);
    };
    let TableObject::TableName(name) = &insert.table else {
        return Err(PreflightError::Unsupported("unsupported INSERT target"));
    };
    let parts = name
        .0
        .iter()
        .map(|part| {
            part.as_ident().ok_or(PreflightError::Unsupported(
                "unsupported INSERT target part",
            ))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if database_name.is_empty() {
        return Err(PreflightError::Unsupported("empty database name"));
    }
    let (schema, table) = match parts.as_slice() {
        [table] => ("dbo", table.value.as_str()),
        [schema, table] => (schema.value.as_str(), table.value.as_str()),
        _ => {
            return Err(PreflightError::Unsupported(
                "unsupported INSERT target parts",
            ));
        }
    };
    if schema.is_empty() || table.is_empty() || table.starts_with('#') {
        return Err(PreflightError::Unsupported(
            "unsupported INSERT target name",
        ));
    };
    let mut table_query = db.prepare(
        "SELECT t.object_id,s.name,t.name,i.name \
         FROM sys.tables t JOIN sys.schemas s ON s.schema_id=t.schema_id \
         LEFT JOIN sys.identity_columns i ON i.object_id=t.object_id \
         WHERE lower(s.name)=lower(?) AND lower(t.name)=lower(?)",
    )?;
    let mut rows = table_query.query([schema, table])?;
    let Some(row) = rows.next()? else {
        return Ok(Some(Permit::NotApplicable));
    };
    let object_id: i32 = row.get(0)?;
    let resolved_schema: String = row.get(1)?;
    let resolved_table: String = row.get(2)?;
    let identity_column: Option<String> = row.get(3)?;
    if rows.next()?.is_some() {
        return Err(PreflightError::InconsistentCatalog(
            "multiple identity definitions for one table",
        ));
    }
    let Some(identity_column) = identity_column else {
        return Ok(Some(Permit::NotApplicable));
    };
    let mut metadata = db.prepare(
        "SELECT column_name FROM information_schema.columns \
         WHERE table_catalog=current_database() \
           AND lower(table_schema)=lower(?) AND lower(table_name)=lower(?) \
         ORDER BY ordinal_position",
    )?;
    let columns = metadata
        .query_map([&resolved_schema, &resolved_table], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<duckdb::Result<Vec<_>>>()?;
    let identity_position = columns
        .iter()
        .position(|name| name.eq_ignore_ascii_case(&identity_column))
        .ok_or(PreflightError::InconsistentCatalog(
            "identity table has no matching physical column",
        ))?;
    let key = session::TableKey {
        database_id,
        database_name: database_name.to_owned(),
        table: session::ResolvedTable {
            object_id,
            schema: resolved_schema.clone(),
            table: resolved_table.clone(),
            identity_position,
        },
    };
    let target = gate::ResolvedTarget {
        key: &key,
        schema: &resolved_schema,
        table: &resolved_table,
        column_count: columns.len(),
        identity_column: Some(identity_position),
    };
    let decision = gate::preflight(statement, &target, state.active(), |name: &ObjectName| {
        let column = name.0.last()?.as_ident()?;
        columns
            .iter()
            .position(|stored| stored.eq_ignore_ascii_case(&column.value))
    })?;
    Ok(Some(decision))
}
