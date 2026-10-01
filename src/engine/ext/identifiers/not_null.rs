//! SQL Server's NOT NULL violation (515), qualified with the database.
//!
//! DuckDB reports `Constraint Error: NOT NULL constraint failed: t.c`.
//! SQL Server reports, with state 2:
//! `Cannot insert the value NULL into column 'c', table 'db.dbo.t'; column
//! does not allow nulls. INSERT fails.` (`UPDATE fails.` for an UPDATE).
use super::super::{Execution, Parameter, Partial, Session, reenter};
use anyhow::Result;
use msduck_core::diagnostic::SqlError;
use sqlparser::ast::{Statement, TableFactor, TableObject};
use std::collections::HashMap;

const PREFIX: &str = "Constraint Error: NOT NULL constraint failed: ";

/// Run an INSERT or UPDATE through the ordinary path and translate its NOT
/// NULL violation. Other statements are declined.
pub(super) fn execute(
    session: &mut Session,
    statement: &Statement,
    parameters: &mut HashMap<String, Parameter>,
) -> Result<Option<Execution>> {
    let (verb, target) = match statement {
        Statement::Insert(insert) => match &insert.table {
            TableObject::TableName(name) => ("INSERT", Some(name)),
            _ => ("INSERT", None),
        },
        Statement::Update(update) => match &update.table.relation {
            TableFactor::Table { name, .. } => ("UPDATE", Some(name)),
            _ => ("UPDATE", None),
        },
        _ => return Ok(None),
    };
    let schema = target.and_then(|name| match name.0.as_slice() {
        [schema, _] => schema.as_ident().map(|ident| ident.value.clone()),
        _ => None,
    });
    let statement = statement.clone();
    match reenter(session, "identifiers", |session| {
        session.execute(statement, parameters)
    }) {
        Ok(execution) => Ok(Some(execution)),
        Err(error) => Err(translate(session, error, verb, schema.as_deref())),
    }
}

fn translate(
    session: &Session,
    error: anyhow::Error,
    verb: &str,
    schema: Option<&str>,
) -> anyhow::Error {
    // A feature's partial output keeps its tokens; only the error changes.
    let error = match error.downcast::<Partial>() {
        Ok(partial) => {
            return Partial {
                tokens: partial.tokens,
                error: translate(session, partial.error, verb, schema),
            }
            .into();
        }
        Err(error) => error,
    };
    if error.downcast_ref::<SqlError>().is_some() {
        return error;
    }
    let message = error.to_string();
    let Some(target) = message.strip_prefix(PREFIX) else {
        return error;
    };
    let Some((schema, table, column)) = resolve(session, target.trim_end(), schema) else {
        return error;
    };
    let diagnostic = SqlError::new(
        515,
        2,
        format!(
            "Cannot insert the value NULL into column '{column}', table '{}.{schema}.{table}'; column does not allow nulls. {verb} fails.",
            session.database.name
        ),
    );
    // Keep every context the engine attached (failed-query metadata,
    // OUTPUT sink state) and add the SqlError, which the client sees. The
    // backend text stays the error's display, which message-based
    // classification (515) reads.
    error.context(diagnostic).context(message)
}

/// Split DuckDB's `table.column` against the current database's catalog,
/// which also supplies the schema DuckDB leaves out. The statement's own
/// schema breaks ties between same-named tables. When the catalog cannot be
/// read (for example in a transaction the failure aborted), the first dot
/// separates the names and the statement's schema, or `dbo`, is used.
fn resolve(
    session: &Session,
    target: &str,
    preferred: Option<&str>,
) -> Option<(String, String, String)> {
    let schemas = |table: &str, column: &str| -> Option<Vec<String>> {
        let mut query = session
            .db
            .prepare(
                "SELECT table_schema FROM information_schema.columns \
                 WHERE table_catalog=current_database() AND table_name=? AND column_name=? \
                 ORDER BY table_schema",
            )
            .ok()?;
        query
            .query_map([table, column], |row| row.get::<_, String>(0))
            .ok()?
            .collect::<duckdb::Result<Vec<_>>>()
            .ok()
    };
    for (index, _) in target.match_indices('.') {
        let (table, column) = (&target[..index], &target[index + 1..]);
        let Some(schemas) = schemas(table, column) else {
            break;
        };
        let schema = preferred
            .and_then(|preferred| {
                schemas
                    .iter()
                    .find(|schema| schema.eq_ignore_ascii_case(preferred))
            })
            .or(schemas.iter().find(|schema| schema.as_str() == "dbo"))
            .or(schemas.first());
        if let Some(schema) = schema {
            return Some((schema.clone(), table.to_owned(), column.to_owned()));
        }
    }
    let (table, column) = target.split_once('.')?;
    Some((
        preferred.unwrap_or("dbo").to_owned(),
        table.to_owned(),
        column.to_owned(),
    ))
}
