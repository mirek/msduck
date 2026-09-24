//! Acquire stored target columns before building deterministic paired images.
use anyhow::{Result, ensure};
use duckdb::Connection;
use sqlparser::{ast::*, dialect::DuckDbDialect, parser::Parser};

pub fn columns(db: &Connection, update: &Update) -> Result<Vec<Ident>> {
    let TableFactor::Table { name, .. } = &update.table.relation else {
        anyhow::bail!("paired OUTPUT requires a base table")
    };
    let parts = name
        .0
        .iter()
        .map(|part| part.as_ident().map(|id| id.value.as_str()))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| anyhow::anyhow!("invalid paired OUTPUT target"))?;
    let (schema, table) = match parts.as_slice() {
        [table] => ("dbo", *table),
        [schema, table] => (*schema, *table),
        _ => anyhow::bail!("unsupported paired OUTPUT database target"),
    };
    let definition: String = db.query_row(
        "SELECT sql FROM duckdb_tables() WHERE database_name=current_database() AND lower(schema_name)=lower(?) AND lower(table_name)=lower(?)",
        [schema, table], |row| row.get(0),
    )?;
    let statements = Parser::parse_sql(&DuckDbDialect {}, &definition)?;
    let [Statement::CreateTable(table)] = statements.as_slice() else {
        anyhow::bail!("missing paired OUTPUT table definition")
    };
    ensure!(
        !table.columns.iter().any(|column| column
            .options
            .iter()
            .any(|option| matches!(option.option, ColumnOption::Generated { .. }))),
        "paired OUTPUT generated columns require generated-image acquisition"
    );
    Ok(table
        .columns
        .iter()
        .map(|column| column.name.clone())
        .collect())
}
