//! Scoped effect adapter for owned TVP cells. This does not dispatch procedures.
use super::{BoundTvp, ColumnKind, OwnedCell, Supply};
use duckdb::{Connection, types::Value};
use msduck_tds::tvp::Limits;
use std::collections::HashSet;

/// The callback must not change the connection's transaction boundaries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transaction {
    /// Begin, clean up, then commit; roll back on any error.
    Owned,
    /// Preserve the caller's transaction, including on error. If DuckDB aborts
    /// it, the caller must roll back; cleanup errors are returned explicitly.
    Caller,
}

#[derive(Debug)]
pub enum Error {
    Invalid(&'static str),
    Limit(&'static str),
    Backend(duckdb::Error),
    Operation(anyhow::Error),
    Cleanup {
        primary: Box<Error>,
        cleanup: duckdb::Error,
    },
}
impl From<duckdb::Error> for Error {
    fn from(error: duckdb::Error) -> Self {
        Self::Backend(error)
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) | Self::Limit(message) => f.write_str(message),
            Self::Backend(error) => error.fmt(f),
            Self::Operation(error) => error.fmt(f),
            Self::Cleanup { primary, cleanup } => write!(f, "{primary}; cleanup failed: {cleanup}"),
        }
    }
}
impl std::error::Error for Error {}

/// Exists only during the callback, on its original connection.
pub struct Relation<'a> {
    sql_name: String,
    bound: &'a BoundTvp,
}
impl Relation<'_> {
    /// Fully qualified, quoted temporary table reference for root AST lowering.
    pub fn sql_name(&self) -> &str {
        &self.sql_name
    }
    /// Logical declarations remain separate from DuckDB storage types.
    pub fn binding(&self) -> &BoundTvp {
        self.bound
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && !name.contains('\0') && name.encode_utf16().count() <= 128
}
fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn validate(bound: &BoundTvp, name: &str, limits: Limits) -> Result<(), Error> {
    if !valid_name(name) || !name.starts_with("__msduck_tvp_") {
        return Err(Error::Invalid(
            "TVP relation requires a bounded private identifier",
        ));
    }
    let table = &bound.table_type;
    if !valid_name(&table.schema)
        || !valid_name(&table.name)
        || table.user_type_id <= 0
        || table.object_id <= 0
    {
        return Err(Error::Invalid("invalid TVP declaration identity"));
    }
    if table.columns.is_empty()
        || table.columns.len() > limits.max_columns.min(1024)
        || bound.rows.len() > limits.max_rows
        || bound
            .rows
            .len()
            .checked_mul(table.columns.len())
            .is_none_or(|n| n > limits.max_cells)
    {
        return Err(Error::Limit("TVP materialization shape limit"));
    }
    if bound.supply != Supply::Explicit && !bound.rows.is_empty() {
        return Err(Error::Invalid("omitted/default TVP must have no rows"));
    }
    let mut names = HashSet::with_capacity(table.columns.len());
    for column in &table.columns {
        if !valid_name(&column.name) || !names.insert(column.name.to_lowercase()) {
            return Err(Error::Invalid("invalid or duplicate TVP column name"));
        }
        match &column.kind {
            ColumnKind::Int => {}
            ColumnKind::Nvarchar {
                max_units,
                collation,
            } if (1..=4000).contains(max_units) && valid_name(collation) => {}
            ColumnKind::Varbinary { max_bytes } if (1..=8000).contains(max_bytes) => {}
            _ => return Err(Error::Invalid("invalid TVP column declaration")),
        }
    }
    let mut bytes = 0usize;
    for row in &bound.rows {
        if row.len() != table.columns.len() {
            return Err(Error::Invalid("TVP row width differs from declaration"));
        }
        for (cell, column) in row.iter().zip(&table.columns) {
            let size = match (cell, &column.kind) {
                (OwnedCell::Null, _) if column.nullable => 0,
                (OwnedCell::Int(_), ColumnKind::Int) => 4,
                (OwnedCell::Nvarchar(units), ColumnKind::Nvarchar { max_units, .. })
                    if units.len() <= usize::from(*max_units) =>
                {
                    units
                        .len()
                        .checked_mul(2)
                        .ok_or(Error::Limit("TVP byte overflow"))?
                }
                (OwnedCell::Varbinary(value), ColumnKind::Varbinary { max_bytes })
                    if value.len() <= usize::from(*max_bytes) =>
                {
                    value.len()
                }
                _ => {
                    return Err(Error::Invalid(
                        "TVP cell violates declared type, width or nullability",
                    ));
                }
            };
            if size > limits.max_cell_bytes {
                return Err(Error::Limit("TVP materialization cell limit"));
            }
            bytes = bytes
                .checked_add(size)
                .ok_or(Error::Limit("TVP byte overflow"))?;
            if bytes > limits.max_input_bytes {
                return Err(Error::Limit("TVP materialization owned-byte limit"));
            }
        }
    }
    Ok(())
}

fn table_oid(db: &Connection, name: &str) -> Result<u64, Error> {
    Ok(db.query_row(
        "SELECT table_oid FROM duckdb_tables() WHERE database_name='temp' AND schema_name='main' AND table_name=?",
        [name], |row| row.get(0),
    )?)
}

/// Materialize a new connection-local relation for one trusted root operation.
/// Values are bound; names are quoted. Validation happens before any writes.
/// The operation may read the relation but must not rename, drop, replace or
/// modify it, or change transaction boundaries. READONLY SQL binding is a
/// separate executor responsibility, not enforced by this backend API.
pub fn with_relation<T>(
    db: &Connection,
    bound: &BoundTvp,
    name: &str,
    limits: Limits,
    transaction: Transaction,
    operation: impl FnOnce(&Relation<'_>) -> anyhow::Result<T>,
) -> Result<T, Error> {
    validate(bound, name, limits)?;
    if transaction == Transaction::Owned {
        db.execute_batch("BEGIN TRANSACTION")?;
    }
    let sql_name = format!("temp.main.{}", quote(name));
    let mut created = None;
    let result = (|| {
        let columns: Vec<_> = bound
            .table_type
            .columns
            .iter()
            .map(|column| {
                let kind = match column.kind {
                    ColumnKind::Int => "INTEGER",
                    ColumnKind::Nvarchar { .. } => "STRUCT(__msduck_utf16le BLOB)",
                    ColumnKind::Varbinary { .. } => "BLOB",
                };
                format!(
                    "{} {kind}{}",
                    quote(&column.name),
                    if column.nullable { "" } else { " NOT NULL" }
                )
            })
            .collect();
        // No IF NOT EXISTS / OR REPLACE: collision cannot adopt another table.
        db.execute_batch(&format!(
            "CREATE TEMP TABLE {} ({})",
            quote(name),
            columns.join(",")
        ))?;
        created = Some(table_oid(db, name)?);
        if !bound.rows.is_empty() {
            let fields: Vec<_> = bound.table_type.columns.iter().enumerate().map(|(index,column)| {
                let parameter = format!("\u{24}{}", index + 1);
                match column.kind {
                    ColumnKind::Nvarchar { .. } => format!(
                        "CASE WHEN {parameter} IS NULL THEN NULL::STRUCT(__msduck_utf16le BLOB) ELSE struct_pack(__msduck_utf16le := {parameter}::BLOB) END"),
                    _ => parameter,
                }
            }).collect();
            let mut statement = db.prepare(&format!(
                "INSERT INTO {sql_name} VALUES ({})",
                fields.join(",")
            ))?;
            for row in &bound.rows {
                let values: Vec<_> = row
                    .iter()
                    .map(|cell| match cell {
                        OwnedCell::Null => Value::Null,
                        OwnedCell::Int(value) => Value::Int(*value),
                        OwnedCell::Nvarchar(units) => {
                            Value::Blob(units.iter().flat_map(|unit| unit.to_le_bytes()).collect())
                        }
                        OwnedCell::Varbinary(bytes) => Value::Blob(bytes.clone()),
                    })
                    .collect();
                statement.execute(duckdb::params_from_iter(values.iter()))?;
            }
        }
        operation(&Relation {
            sql_name: sql_name.clone(),
            bound,
        })
        .map_err(Error::Operation)
    })();
    let cleanup = if let Some(oid) = created {
        match table_oid(db, name) {
            Ok(current) if current == oid => db
                .execute_batch(&format!("DROP TABLE {sql_name}"))
                .map(|_| ()),
            Ok(_) => Err(duckdb::Error::InvalidParameterName(
                "TVP relation identity changed during operation".into(),
            )),
            Err(Error::Backend(error)) => Err(error),
            Err(_) => unreachable!(),
        }
    } else {
        Ok(())
    };
    let result = match (result, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(Error::Backend(error)),
        (Err(primary), Err(cleanup)) => Err(Error::Cleanup {
            primary: Box::new(primary),
            cleanup,
        }),
    };
    if transaction == Transaction::Owned {
        match result {
            Ok(value) => match db.execute_batch("COMMIT") {
                Ok(_) => Ok(value),
                Err(error) => {
                    let primary = Error::Backend(error);
                    match db.execute_batch("ROLLBACK") {
                        Ok(_) => Err(primary),
                        Err(cleanup) => Err(Error::Cleanup {
                            primary: Box::new(primary),
                            cleanup,
                        }),
                    }
                }
            },
            Err(primary) => match db.execute_batch("ROLLBACK") {
                Ok(_) => Err(primary),
                Err(cleanup) => Err(Error::Cleanup {
                    primary: Box::new(primary),
                    cleanup,
                }),
            },
        }
    } else {
        result
    }
}
