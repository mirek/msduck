//! CREATE TABLE names share the schema's object namespace with constraints
//! (reference/gaps-catalog.json, `namespaceProfile`).
//!
//! - An existing table or view takes precedence: 2714, state 6.
//! - A named DEFAULT whose name an object of the schema, or an earlier
//!   DEFAULT of the statement, already has fails with 2714 (state 5) and
//!   1750.
//!
//! These checks run before the backend creates the table, so no partial
//! table exists after the error, inside a caller's transaction too.
use crate::engine::{Session, StatementErrors};
use anyhow::Result;
use msduck_core::diagnostic::SqlError;
use sqlparser::ast::{ColumnOption, CreateTable};
use std::collections::HashSet;

fn exists(session: &Session, schema: &str, name: &str, types: &str) -> Result<bool> {
    Ok(session.db.query_row(
        &format!(
            "SELECT count(*)>0 FROM sys.objects o JOIN main.__msduck_schemas s USING(schema_id)
             WHERE lower(s.name)=lower(?) AND lower(o.name)=lower(?) {types}"
        ),
        [schema, name],
        |row| row.get(0),
    )?)
}

fn taken(name: &str, state: u8) -> SqlError {
    SqlError::new(
        2714,
        state,
        format!("There is already an object named '{name}' in the database."),
    )
}

pub(super) fn create_table(session: &Session, table: &CreateTable) -> Result<()> {
    let parts: Option<Vec<&str>> = table
        .name
        .0
        .iter()
        .map(|part| part.as_ident().map(|ident| ident.value.as_str()))
        .collect();
    let (schema, name) = match parts.as_deref() {
        Some([name]) => ("dbo", *name),
        Some([schema, name]) => (*schema, *name),
        _ => return Ok(()),
    };
    if name.starts_with('#') || name.starts_with("__msduck_") {
        return Ok(());
    }
    if exists(session, schema, name, "AND rtrim(o.type) IN ('U','V')")? {
        anyhow::bail!(taken(name, 6));
    }
    let mut declared = HashSet::from([name.to_lowercase()]);
    for option in table.columns.iter().flat_map(|column| &column.options) {
        let (ColumnOption::Default(_), Some(constraint)) = (&option.option, &option.name) else {
            continue;
        };
        if !declared.insert(constraint.value.to_lowercase())
            || exists(session, schema, &constraint.value, "")?
        {
            return Err(StatementErrors(vec![
                taken(&constraint.value, 5),
                SqlError::new(
                    1750,
                    1,
                    "Could not create constraint or index. See previous errors.",
                ),
            ])
            .into());
        }
    }
    Ok(())
}
