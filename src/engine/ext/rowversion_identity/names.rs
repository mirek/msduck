//! Target tables and their physical columns in the current database.
use crate::engine::Session;
use anyhow::Result;
use sqlparser::ast::ObjectName;

/// A table of the current database, by its spelled schema and name.
#[derive(Clone, Debug)]
pub(super) struct Table {
    pub schema: String,
    pub name: String,
}

/// A physical column, in ordinal order.
#[derive(Clone, Debug)]
pub(super) struct Column {
    pub name: String,
    pub default: Option<String>,
}

/// Resolve a one-, two- or three-part name in the current database. Temporary
/// tables and other databases are left to their own paths.
pub(super) fn table(session: &Session, name: &ObjectName) -> Option<Table> {
    let parts = name
        .0
        .iter()
        .map(|part| part.as_ident().map(|id| id.value.as_str()))
        .collect::<Option<Vec<_>>>()?;
    let (schema, table) = match parts.as_slice() {
        [table] => ("dbo", *table),
        [schema, table] => (*schema, *table),
        [database, schema, table] if database.eq_ignore_ascii_case(&session.database().name) => {
            (if schema.is_empty() { "dbo" } else { *schema }, *table)
        }
        _ => return None,
    };
    if table.is_empty() || table.starts_with('#') || schema.is_empty() {
        return None;
    }
    Some(Table {
        schema: schema.to_owned(),
        name: table.to_owned(),
    })
}

/// The table's physical columns in ordinal order; empty when it does not
/// exist. Every INSERT reads them, so this uses DuckDB's catalog function
/// rather than the information_schema view.
pub(super) fn columns(db: &duckdb::Connection, table: &Table) -> Result<Vec<Column>> {
    let mut query = db.prepare(
        "SELECT column_name, column_default FROM duckdb_columns() \
         WHERE database_name=current_database() AND schema_name=? COLLATE NOCASE \
           AND table_name=? COLLATE NOCASE ORDER BY column_index",
    )?;
    Ok(query
        .query_map([&table.schema, &table.name], |row| {
            Ok(Column {
                name: row.get(0)?,
                default: row.get(1)?,
            })
        })?
        .collect::<duckdb::Result<Vec<_>>>()?)
}

/// The lower-case names of the table's computed columns.
pub(super) fn computed(db: &duckdb::Connection, table: &Table) -> Result<Vec<String>> {
    crate::computed_columns::names(db, &table.schema, &table.name)
}
