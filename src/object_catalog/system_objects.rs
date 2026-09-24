//! Built-in object membership captured from the pinned SQL Server image.
use anyhow::{Context, Result, ensure};
use duckdb::Connection;
use serde_json::Value;

const SEED: &str = include_str!("system_objects.json");
const COLUMNS: [&str; 15] = [
    "name",
    "object_id",
    "principal_id",
    "schema_id",
    "parent_object_id",
    "type",
    "type_desc",
    "create_date",
    "modify_date",
    "is_ms_shipped",
    "is_published",
    "is_schema_published",
    "in_objects",
    "in_system_objects",
    "dynamic_clock",
];

pub(super) fn register(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS main.__msduck_builtin_objects(
            name VARCHAR NOT NULL, object_id INTEGER PRIMARY KEY, principal_id INTEGER,
            schema_id INTEGER NOT NULL, parent_object_id INTEGER NOT NULL,
            type VARCHAR NOT NULL, type_desc VARCHAR NOT NULL,
            create_date TIMESTAMP NOT NULL, modify_date TIMESTAMP NOT NULL,
            is_ms_shipped BOOLEAN NOT NULL, is_published BOOLEAN NOT NULL,
            is_schema_published BOOLEAN NOT NULL, in_objects BOOLEAN NOT NULL,
            in_system_objects BOOLEAN NOT NULL,
            CHECK (in_objects != in_system_objects));",
    )?;
    let count: i64 = db.query_row(
        "SELECT count(*) FROM main.__msduck_builtin_objects",
        [],
        |row| row.get(0),
    )?;
    if count != 0 {
        return Ok(());
    }

    let seed: Value = serde_json::from_str(SEED).context("parse built-in object seed")?;
    let columns = seed["columns"].as_array().context("seed columns missing")?;
    ensure!(columns.len() == COLUMNS.len());
    for (actual, expected) in columns.iter().zip(COLUMNS) {
        ensure!(
            actual.as_str() == Some(expected),
            "built-in object seed columns changed"
        );
    }
    let rows = seed["rows"].as_array().context("seed rows missing")?;
    let current_clock: String =
        db.query_row("SELECT CAST(current_timestamp AS VARCHAR)", [], |row| {
            row.get(0)
        })?;
    db.execute_batch("BEGIN TRANSACTION")?;
    let result: Result<()> = (|| {
        let mut insert = db.prepare(
            "INSERT INTO main.__msduck_builtin_objects VALUES (
                ?,?,?,?,?,?,?,CAST(? AS TIMESTAMP),CAST(? AS TIMESTAMP),?,?,?,?,?)",
        )?;
        for values in rows {
            let row = values.as_array().context("seed row is not an array")?;
            ensure!(
                row.len() == COLUMNS.len(),
                "built-in object seed row width changed"
            );
            let string = |index: usize| -> Result<&str> {
                row[index]
                    .as_str()
                    .context("built-in object seed string missing")
            };
            let integer = |index: usize| -> Result<i32> {
                i32::try_from(
                    row[index]
                        .as_i64()
                        .context("built-in object seed integer missing")?,
                )
                .context("built-in object seed integer out of range")
            };
            let boolean = |index: usize| -> Result<bool> {
                row[index]
                    .as_bool()
                    .context("built-in object seed Boolean missing")
            };
            let date = |index: usize| -> Result<String> {
                if boolean(14)? {
                    return Ok(current_clock.clone());
                }
                Ok(string(index)?
                    .replace('T', " ")
                    .trim_end_matches('Z')
                    .to_owned())
            };
            let principal_id = row[2].as_i64().map(i32::try_from).transpose()?;
            insert.execute(duckdb::params![
                string(0)?,
                integer(1)?,
                principal_id,
                integer(3)?,
                integer(4)?,
                string(5)?,
                string(6)?,
                date(7)?,
                date(8)?,
                boolean(9)?,
                boolean(10)?,
                boolean(11)?,
                boolean(12)?,
                boolean(13)?,
            ])?;
        }
        Ok(())
    })();
    if result.is_err() {
        db.execute_batch("ROLLBACK")?;
        return result;
    }
    db.execute_batch("COMMIT")?;
    Ok(())
}
