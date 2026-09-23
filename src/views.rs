//! Validation for persisted view definitions before SQL translation.
use anyhow::{Result, bail, ensure};
pub use msduck_sql::view_definition::alter_definition;
use sqlparser::ast::*;

pub fn alter_existing(
    db: &duckdb::Connection,
    name: &ObjectName,
    sql: &str,
    autocommit: bool,
) -> Result<()> {
    let parts = name
        .0
        .iter()
        .map(|part| {
            part.as_ident()
                .map(|id| id.value.as_str())
                .ok_or_else(|| anyhow::anyhow!("unsupported view identifier"))
        })
        .collect::<Result<Vec<_>>>()?;
    let (schema, view) = match parts.as_slice() {
        [view] => ("dbo", *view),
        [schema, view] => (*schema, *view),
        _ => bail!("unsupported cross-database view name"),
    };
    // Keep lookup and replacement in the same transaction snapshot. In an
    // explicit user transaction, leave commit/rollback to the caller.
    if autocommit {
        db.execute_batch("BEGIN TRANSACTION")?;
    }
    let result = (|| -> Result<()> {
        let exists: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM duckdb_views() WHERE database_name=current_database() AND schema_name=? COLLATE NOCASE AND view_name=? COLLATE NOCASE AND NOT internal)",
            [schema, view], |row| row.get(0))?;
        ensure!(exists, "Cannot alter view {name}: view does not exist");
        db.execute(sql, [])?;
        if autocommit {
            db.execute_batch("COMMIT")?;
        }
        Ok(())
    })();
    if result.is_err() && autocommit {
        let _ = db.execute_batch("ROLLBACK");
    }
    result
}
