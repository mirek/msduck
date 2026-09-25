//! Pinned SQL Server built-in column membership, separate from user DDL state.
use anyhow::{Context, Result, ensure};
use duckdb::{Connection, appender_params_from_iter, types::Value as DuckValue};
use serde_json::Value;

const SEED: &str = include_str!("system_columns.json");
const IMAGE: &str = "mcr.microsoft.com/mssql/server:2025-latest@sha256:86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a";
const FIELDS: [(&str, &str); 44] = [
    ("object_id", "INTEGER"),
    ("name", "VARCHAR"),
    ("column_id", "INTEGER"),
    ("system_type_id", "UTINYINT"),
    ("user_type_id", "INTEGER"),
    ("max_length", "SMALLINT"),
    ("precision", "UTINYINT"),
    ("scale", "UTINYINT"),
    ("collation_name", "VARCHAR"),
    ("is_nullable", "BOOLEAN"),
    ("is_ansi_padded", "BOOLEAN"),
    ("is_rowguidcol", "BOOLEAN"),
    ("is_identity", "BOOLEAN"),
    ("is_computed", "BOOLEAN"),
    ("is_filestream", "BOOLEAN"),
    ("is_replicated", "BOOLEAN"),
    ("is_non_sql_subscribed", "BOOLEAN"),
    ("is_merge_published", "BOOLEAN"),
    ("is_dts_replicated", "BOOLEAN"),
    ("is_xml_document", "BOOLEAN"),
    ("xml_collection_id", "INTEGER"),
    ("default_object_id", "INTEGER"),
    ("rule_object_id", "INTEGER"),
    ("is_sparse", "BOOLEAN"),
    ("is_column_set", "BOOLEAN"),
    ("generated_always_type", "UTINYINT"),
    ("generated_always_type_desc", "VARCHAR"),
    ("encryption_type", "INTEGER"),
    ("encryption_type_desc", "VARCHAR"),
    ("encryption_algorithm_name", "VARCHAR"),
    ("column_encryption_key_id", "INTEGER"),
    ("column_encryption_key_database_name", "VARCHAR"),
    ("is_hidden", "BOOLEAN"),
    ("is_masked", "BOOLEAN"),
    ("graph_type", "INTEGER"),
    ("graph_type_desc", "VARCHAR"),
    ("is_data_deletion_filter_column", "BOOLEAN"),
    ("ledger_view_column_type", "INTEGER"),
    ("ledger_view_column_type_desc", "VARCHAR"),
    ("is_dropped_ledger_column", "BOOLEAN"),
    ("vector_dimensions", "INTEGER"),
    ("vector_base_type", "UTINYINT"),
    ("vector_base_type_desc", "VARCHAR"),
    ("in_system_columns", "BOOLEAN"),
];

fn parse_value(value: &Value, kind: &str, name: &str) -> Result<DuckValue> {
    if value.is_null() {
        ensure!(
            !matches!(
                name,
                "object_id" | "name" | "column_id" | "in_system_columns"
            ),
            "built-in column {name} cannot be NULL"
        );
        return Ok(DuckValue::Null);
    }
    match kind {
        "INTEGER" => {
            Ok(DuckValue::Int(i32::try_from(value.as_i64().context(
                format!("built-in column {name} is not an integer"),
            )?)?))
        }
        "SMALLINT" => Ok(DuckValue::SmallInt(i16::try_from(
            value
                .as_i64()
                .context(format!("built-in column {name} is not an integer"))?,
        )?)),
        "UTINYINT" => {
            Ok(DuckValue::UTinyInt(u8::try_from(value.as_u64().context(
                format!("built-in column {name} is not an unsigned byte"),
            )?)?))
        }
        "BOOLEAN" => {
            Ok(DuckValue::Boolean(value.as_bool().context(format!(
                "built-in column {name} is not a Boolean"
            ))?))
        }
        "VARCHAR" => Ok(DuckValue::Text(
            value
                .as_str()
                .context(format!("built-in column {name} is not text"))?
                .to_owned(),
        )),
        _ => unreachable!("static field type"),
    }
}

fn seed_rows(seed: &Value) -> Result<(&Vec<Value>, &Vec<Value>)> {
    ensure!(
        seed["source_image"].as_str() == Some(IMAGE),
        "built-in column seed image changed"
    );
    let names = seed["columns"]
        .as_array()
        .context("built-in column seed names missing")?;
    ensure!(
        names.len() == FIELDS.len(),
        "built-in column seed width changed"
    );
    for (actual, (expected, _)) in names.iter().zip(FIELDS) {
        ensure!(
            actual.as_str() == Some(expected),
            "built-in column seed order changed"
        );
    }
    let rows = seed["rows"]
        .as_array()
        .context("built-in column seed rows missing")?;
    let hidden = seed["hidden_owners"]
        .as_array()
        .context("hidden owner inventory missing")?;
    ensure!(rows.len() == 12803, "built-in column seed count changed");
    ensure!(hidden.len() == 166, "hidden owner count changed");
    Ok((rows, hidden))
}

pub(super) fn register(db: &Connection) -> Result<()> {
    let definitions = FIELDS
        .iter()
        .map(|(name, kind)| {
            format!(
                "{name} {kind}{}",
                if matches!(
                    *name,
                    "object_id" | "name" | "column_id" | "in_system_columns"
                ) {
                    " NOT NULL"
                } else {
                    ""
                }
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    db.execute_batch(&format!("CREATE TABLE IF NOT EXISTS main.__msduck_builtin_columns({definitions},PRIMARY KEY(object_id,column_id));
        CREATE TABLE IF NOT EXISTS main.__msduck_hidden_column_owners(object_id INTEGER PRIMARY KEY,schema_name VARCHAR NOT NULL,name VARCHAR NOT NULL,UNIQUE(schema_name,name));
        CREATE TABLE IF NOT EXISTS main.__msduck_builtin_column_seed(source_image VARCHAR PRIMARY KEY);"))?;
    let count: i64 = db.query_row(
        "SELECT count(*) FROM main.__msduck_builtin_columns",
        [],
        |row| row.get(0),
    )?;
    let hidden_count: i64 = db.query_row(
        "SELECT count(*) FROM main.__msduck_hidden_column_owners",
        [],
        |row| row.get(0),
    )?;
    let version: Option<String> = db.query_row(
        "SELECT max(source_image) FROM main.__msduck_builtin_column_seed",
        [],
        |row| row.get(0),
    )?;
    if count != 0 || hidden_count != 0 || version.is_some() {
        ensure!(
            count == 12803 && hidden_count == 166 && version.as_deref() == Some(IMAGE),
            "persisted built-in column seed is incomplete or from another image"
        );
        return Ok(());
    }
    let seed: Value = serde_json::from_str(SEED).context("parse built-in column seed")?;
    let (rows, hidden) = seed_rows(&seed)?;
    db.execute_batch("BEGIN TRANSACTION")?;
    let result: Result<()> = (|| {
        let mut appender = db.appender("__msduck_builtin_columns")?;
        for (position, row) in rows.iter().enumerate() {
            let row = row
                .as_array()
                .context("built-in column row is not an array")?;
            ensure!(
                row.len() == FIELDS.len(),
                "built-in column row {position} width changed"
            );
            let values = row
                .iter()
                .zip(FIELDS)
                .map(|(value, (name, kind))| parse_value(value, kind, name))
                .collect::<Result<Vec<_>>>()?;
            appender.append_row(appender_params_from_iter(values))?;
        }
        appender.flush()?;
        drop(appender);
        let mut appender = db.appender("__msduck_hidden_column_owners")?;
        for row in hidden {
            let row = row.as_array().context("hidden owner row is not an array")?;
            ensure!(row.len() == 3, "hidden owner row width changed");
            let id = i32::try_from(row[0].as_i64().context("hidden owner ID missing")?)?;
            let schema = row[1].as_str().context("hidden owner schema missing")?;
            let name = row[2].as_str().context("hidden owner name missing")?;
            ensure!(schema == "sys", "hidden owner schema changed");
            appender.append_row(duckdb::params![id, schema, name])?;
        }
        appender.flush()?;
        drop(appender);
        db.execute(
            "INSERT INTO main.__msduck_builtin_column_seed VALUES(?)",
            [IMAGE],
        )?;
        let (actual, system): (i64, i64) = db.query_row("SELECT count(*),sum(CAST(in_system_columns AS BIGINT)) FROM main.__msduck_builtin_columns", [], |row| Ok((row.get(0)?, row.get(1)?)))?;
        ensure!(
            actual == 12803 && system == 11534,
            "built-in column membership changed during loading"
        );
        let mismatched_owners: i64 = db.query_row(
            "SELECT count(*) FROM main.__msduck_builtin_columns c LEFT JOIN sys.all_objects o ON o.object_id=c.object_id LEFT JOIN main.__msduck_hidden_column_owners h ON h.object_id=c.object_id WHERE (o.object_id IS NULL AND h.object_id IS NULL) OR (o.object_id IS NOT NULL AND h.object_id IS NOT NULL) OR (h.object_id IS NOT NULL AND NOT c.in_system_columns)",
            [],
            |row| row.get(0),
        )?;
        ensure!(
            mismatched_owners == 0,
            "built-in column owner membership changed"
        );
        Ok(())
    })();
    if result.is_err() {
        db.execute_batch("ROLLBACK")?;
        return result;
    }
    db.execute_batch("COMMIT")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_seed_rejected_before_database_writes() {
        let mut seed: Value = serde_json::from_str(SEED).unwrap();
        seed["columns"][0] = Value::String("wrong".into());
        assert!(seed_rows(&seed).is_err());
        let mut seed: Value = serde_json::from_str(SEED).unwrap();
        seed["rows"].as_array_mut().unwrap().pop();
        assert!(seed_rows(&seed).is_err());
        assert!(parse_value(&Value::String("x".into()), "INTEGER", "object_id").is_err());
        assert!(parse_value(&Value::Null, "VARCHAR", "name").is_err());
    }
}
