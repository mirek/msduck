//! Built-in object membership captured from the pinned SQL Server image.
use anyhow::{Context, Result, ensure};
use duckdb::{
    Connection,
    arrow::{
        array::{ArrayRef, BooleanBuilder, Int32Builder, StringBuilder},
        datatypes::{DataType, Field, Schema},
        record_batch::RecordBatch,
    },
};
use serde_json::Value;
use std::sync::Arc;

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
    ensure!(rows.len() == 2742, "built-in object seed count changed");
    let current_clock: String =
        db.query_row("SELECT CAST(current_timestamp AS VARCHAR)", [], |row| {
            row.get(0)
        })?;
    db.execute_batch(
        "BEGIN TRANSACTION;
        CREATE TABLE main.__msduck_builtin_seed(
            name VARCHAR, object_id INTEGER, principal_id INTEGER,
            schema_id INTEGER, parent_object_id INTEGER, type VARCHAR,
            type_desc VARCHAR, create_date VARCHAR, modify_date VARCHAR,
            is_ms_shipped BOOLEAN, is_published BOOLEAN, is_schema_published BOOLEAN,
            in_objects BOOLEAN, in_system_objects BOOLEAN);",
    )?;
    let result: Result<()> = (|| {
        let mut appender = db.appender("__msduck_builtin_seed")?;
        appender.append_record_batch(seed_batch(rows, &current_clock)?)?;
        appender.flush()?;
        drop(appender);
        db.execute_batch(
            "INSERT INTO main.__msduck_builtin_objects
             SELECT name,object_id,principal_id,schema_id,parent_object_id,type,type_desc,
                    CAST(create_date AS TIMESTAMP),CAST(modify_date AS TIMESTAMP),
                    is_ms_shipped,is_published,is_schema_published,in_objects,in_system_objects
             FROM main.__msduck_builtin_seed;
             DROP TABLE main.__msduck_builtin_seed;",
        )?;
        Ok(())
    })();
    if result.is_err() {
        db.execute_batch("ROLLBACK")?;
        return result;
    }
    db.execute_batch("COMMIT")?;
    Ok(())
}

fn seed_batch(rows: &[Value], current_clock: &str) -> Result<RecordBatch> {
    let mut names = StringBuilder::new();
    let mut ids = Int32Builder::new();
    let mut principal_ids = Int32Builder::new();
    let mut schema_ids = Int32Builder::new();
    let mut parent_ids = Int32Builder::new();
    let mut types = StringBuilder::new();
    let mut descriptions = StringBuilder::new();
    let mut created = StringBuilder::new();
    let mut modified = StringBuilder::new();
    let mut shipped = BooleanBuilder::new();
    let mut published = BooleanBuilder::new();
    let mut schema_published = BooleanBuilder::new();
    let mut in_objects = BooleanBuilder::new();
    let mut in_system_objects = BooleanBuilder::new();
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
                return Ok(current_clock.to_owned());
            }
            Ok(string(index)?
                .replace('T', " ")
                .trim_end_matches('Z')
                .to_owned())
        };
        names.append_value(string(0)?);
        ids.append_value(integer(1)?);
        if row[2].is_null() {
            principal_ids.append_null();
        } else {
            principal_ids.append_value(integer(2)?);
        }
        schema_ids.append_value(integer(3)?);
        parent_ids.append_value(integer(4)?);
        types.append_value(string(5)?);
        descriptions.append_value(string(6)?);
        created.append_value(date(7)?);
        modified.append_value(date(8)?);
        shipped.append_value(boolean(9)?);
        published.append_value(boolean(10)?);
        schema_published.append_value(boolean(11)?);
        in_objects.append_value(boolean(12)?);
        in_system_objects.append_value(boolean(13)?);
    }
    let schema = Schema::new(vec![
        Field::new("name", DataType::Utf8, false),
        Field::new("object_id", DataType::Int32, false),
        Field::new("principal_id", DataType::Int32, true),
        Field::new("schema_id", DataType::Int32, false),
        Field::new("parent_object_id", DataType::Int32, false),
        Field::new("type", DataType::Utf8, false),
        Field::new("type_desc", DataType::Utf8, false),
        Field::new("create_date", DataType::Utf8, false),
        Field::new("modify_date", DataType::Utf8, false),
        Field::new("is_ms_shipped", DataType::Boolean, false),
        Field::new("is_published", DataType::Boolean, false),
        Field::new("is_schema_published", DataType::Boolean, false),
        Field::new("in_objects", DataType::Boolean, false),
        Field::new("in_system_objects", DataType::Boolean, false),
    ]);
    let columns: Vec<ArrayRef> = vec![
        Arc::new(names.finish()),
        Arc::new(ids.finish()),
        Arc::new(principal_ids.finish()),
        Arc::new(schema_ids.finish()),
        Arc::new(parent_ids.finish()),
        Arc::new(types.finish()),
        Arc::new(descriptions.finish()),
        Arc::new(created.finish()),
        Arc::new(modified.finish()),
        Arc::new(shipped.finish()),
        Arc::new(published.finish()),
        Arc::new(schema_published.finish()),
        Arc::new(in_objects.finish()),
        Arc::new(in_system_objects.finish()),
    ];
    Ok(RecordBatch::try_new(Arc::new(schema), columns)?)
}
