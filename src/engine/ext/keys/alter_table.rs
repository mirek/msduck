//! ALTER TABLE on a table with key constraints or indexes.
//!
//! - DROP COLUMN, and ALTER COLUMN changing a column's type, fail with 5074
//!   and 4922 when a key constraint or index uses the column, as in SQL
//!   Server. Widening a variable-length character or binary key column is
//!   allowed.
//! - DuckDB refuses most ALTER TABLE forms other than ADD COLUMN while the
//!   table has an index, so the table's indexes are dropped before the
//!   change and recreated from their definitions after it. Outside a user
//!   transaction the drop commits first, and the indexes return even when
//!   the change fails. Inside one, everything shares the transaction (see
//!   [`within_transaction`]).
use super::{atomically, catalog, tables};
use crate::engine::{Execution, Parameter, Session, StatementErrors, ext};
use anyhow::Result;
use msduck_core::diagnostic::SqlError;
use sqlparser::ast::{
    AlterColumnOperation, AlterTableOperation, BinaryLength, CharacterLength, DataType, Statement,
};
use std::collections::HashMap;

/// An object using columns: a key constraint (`object`) or an index.
struct Dependent {
    name: String,
    index: bool,
    columns: Vec<String>,
}

fn dependents(db: &duckdb::Connection, object_id: i32) -> Result<Vec<Dependent>> {
    let mut found: Vec<Dependent> = catalog::table(db, object_id)?
        .into_iter()
        .map(|key| Dependent {
            index: !key.constraint(),
            columns: key.columns.iter().chain(&key.include).cloned().collect(),
            name: key.name,
        })
        .collect();
    // Ordinary indexes of the table-owned index catalog.
    let ordinary = db
        .prepare(
            "SELECT c.name,string_agg(k.column_name,chr(31) ORDER BY k.ordinal)
             FROM main.__msduck_index_catalog c JOIN main.__msduck_index_keys k USING(incarnation)
             WHERE c.object_id=? AND NOT EXISTS(
               SELECT 1 FROM main.__msduck_keys m WHERE m.incarnation=c.incarnation)
             GROUP BY c.name,c.incarnation",
        )?
        .query_map([object_id], |r| {
            Ok(Dependent {
                name: r.get(0)?,
                index: true,
                columns: r
                    .get::<_, String>(1)?
                    .split('\u{1f}')
                    .map(str::to_owned)
                    .collect(),
            })
        })?
        .collect::<duckdb::Result<Vec<_>>>()?;
    found.extend(ordinary);
    Ok(found)
}

/// A variable-length character or binary type, its system type and length.
fn varying(kind: &DataType) -> Option<(u8, Option<u64>)> {
    let character = |length: &Option<CharacterLength>| match length {
        Some(CharacterLength::IntegerLength { length, .. }) => Some(*length),
        None => Some(1),
        Some(CharacterLength::Max) => None,
    };
    Some(match kind {
        DataType::Varchar(n) | DataType::CharacterVarying(n) | DataType::CharVarying(n) => {
            (167, character(n))
        }
        DataType::Nvarchar(n) => (231, character(n).map(|n| n * 2)),
        DataType::Varbinary(n) => (
            165,
            match n {
                Some(BinaryLength::IntegerLength { length }) => Some(*length),
                None => Some(1),
                Some(BinaryLength::Max) => None,
            },
        ),
        _ => return None,
    })
}

/// Whether SQL Server lets ALTER COLUMN change a key column to `kind`.
fn compatible(column: &msduck_sql::dialect::ext::keys::value::Column, kind: &DataType) -> bool {
    match varying(kind) {
        Some((system, Some(length))) => {
            system == column.system_type_id
                && column.max_length >= 0
                && length >= column.max_length as u64
        }
        _ => false,
    }
}

pub(super) fn run(
    session: &mut Session,
    statement: &mut Statement,
    parameters: &mut HashMap<String, Parameter>,
) -> Result<Option<Execution>> {
    let Statement::AlterTable(alter) = statement else {
        return Ok(None);
    };
    let Some(table) = tables::resolve(&session.db, &alter.name)? else {
        return Ok(None);
    };
    let dependents = dependents(&session.db, table.object_id)?;
    for operation in &alter.operations {
        let (verb, columns, kind): (&str, Vec<&str>, Option<&DataType>) = match operation {
            AlterTableOperation::DropColumn { column_names, .. } => (
                "DROP",
                column_names.iter().map(|c| c.value.as_str()).collect(),
                None,
            ),
            AlterTableOperation::AlterColumn {
                column_name,
                op: AlterColumnOperation::SetDataType { data_type, .. },
            } => ("ALTER", vec![column_name.value.as_str()], Some(data_type)),
            _ => continue,
        };
        for column in columns {
            let Some(user) = dependents
                .iter()
                .find(|d| d.columns.iter().any(|c| c.eq_ignore_ascii_case(column)))
            else {
                continue;
            };
            if let (Some(kind), Some(declared)) = (kind, table.column(column))
                && compatible(declared, kind)
            {
                continue;
            }
            return Err(StatementErrors(vec![
                SqlError::new(
                    5074,
                    1,
                    format!(
                        "The {} '{}' is dependent on column '{column}'.",
                        if user.index { "index" } else { "object" },
                        user.name
                    ),
                ),
                SqlError::new(
                    4922,
                    9,
                    format!(
                        "ALTER TABLE {verb} COLUMN {column} failed because one or more objects access this column."
                    ),
                ),
            ])
            .into());
        }
    }
    // DuckDB allows ADD COLUMN while indexes exist.
    if alter
        .operations
        .iter()
        .all(|operation| matches!(operation, AlterTableOperation::AddColumn { .. }))
    {
        return Ok(None);
    }
    let indexes: Vec<(String, String)> = session
        .db
        .prepare(
            "SELECT index_name,sql FROM duckdb_indexes()
             WHERE database_name=current_database() AND schema_name=? AND table_name=?
             ORDER BY index_oid",
        )?
        .query_map([&table.schema, &table.name], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<duckdb::Result<Vec<_>>>()?;
    if indexes.is_empty() {
        return Ok(None);
    }
    // Only indexes this feature or the table-owned index catalog records
    // can be renamed; a legacy index keeps DuckDB's refusal.
    let owned: i64 = session.db.query_row(
        "SELECT count(*) FROM duckdb_indexes() i
         WHERE i.database_name=current_database() AND i.schema_name=? AND i.table_name=?
           AND (EXISTS(SELECT 1 FROM main.__msduck_keys k WHERE k.backend_name=i.index_name)
             OR EXISTS(SELECT 1 FROM main.__msduck_index_catalog c
               WHERE c.backend_schema=i.schema_name AND c.backend_name=i.index_name))",
        [&table.schema, &table.name],
        |r| r.get(0),
    )?;
    if owned != indexes.len() as i64 {
        return Ok(None);
    }
    let statement = statement.clone();
    if session.transactions > 0 {
        return within_transaction(session, &table, &indexes, statement, parameters).map(Some);
    }
    // Outside a user transaction the indexes are dropped and committed
    // first: DuckDB still sees an index dropped in the open transaction
    // when it checks DROP COLUMN and ALTER COLUMN.
    atomically(session, |session| {
        save(&session.db, &table)?;
        drop_indexes(&session.db, &table, &indexes)
    })?;
    let result = ext::reenter(session, "keys", |session| {
        session.execute(statement, parameters)
    });
    // Restore the indexes whether or not the change succeeded.
    let restored = atomically(session, |session| {
        for (_, sql) in &indexes {
            session.db.execute_batch(sql)?;
        }
        restore(&session.db, &table)
    });
    let execution = result?;
    restored?;
    Ok(Some(execution))
}

/// Keep the table's rows of the table-owned index catalog: the engine prunes
/// rows of missing indexes after the ALTER.
fn save(db: &duckdb::Connection, table: &tables::Table) -> Result<()> {
    db.execute(
        "CREATE OR REPLACE TEMP TABLE __msduck_keys_saved_indexes AS
         SELECT * FROM main.__msduck_index_catalog WHERE object_id=?",
        [table.object_id],
    )?;
    db.execute_batch(
        "CREATE OR REPLACE TEMP TABLE __msduck_keys_saved_columns AS
         SELECT * FROM main.__msduck_index_keys
         WHERE incarnation IN (SELECT incarnation FROM __msduck_keys_saved_indexes)",
    )?;
    Ok(())
}

fn drop_indexes(
    db: &duckdb::Connection,
    table: &tables::Table,
    indexes: &[(String, String)],
) -> Result<()> {
    for (name, _) in indexes {
        db.execute_batch(&format!(
            "DROP INDEX {}.{}",
            tables::quote(&table.schema),
            tables::quote(name)
        ))?;
    }
    Ok(())
}

fn restore(db: &duckdb::Connection, table: &tables::Table) -> Result<()> {
    db.execute(
        "UPDATE __msduck_keys_saved_indexes SET table_oid=(
           SELECT CAST(table_oid AS BIGINT) FROM duckdb_tables()
           WHERE database_name=current_database() AND schema_name=? AND table_name=?)",
        [&table.schema, &table.name],
    )?;
    db.execute_batch(
        "DELETE FROM main.__msduck_index_keys
           WHERE incarnation IN (SELECT incarnation FROM __msduck_keys_saved_indexes);
         DELETE FROM main.__msduck_index_catalog
           WHERE incarnation IN (SELECT incarnation FROM __msduck_keys_saved_indexes);
         INSERT INTO main.__msduck_index_catalog SELECT * FROM __msduck_keys_saved_indexes;
         INSERT INTO main.__msduck_index_keys SELECT * FROM __msduck_keys_saved_columns;
         DROP TABLE __msduck_keys_saved_indexes;
         DROP TABLE __msduck_keys_saved_columns",
    )?;
    Ok(())
}

/// Inside a user transaction everything shares it. DuckDB cannot create an
/// index under a name dropped in the same transaction, so each index
/// returns under a new backend name, and the catalogs follow. DuckDB can
/// still refuse a change that depends on a dropped index's columns.
fn within_transaction(
    session: &mut Session,
    table: &tables::Table,
    indexes: &[(String, String)],
    statement: Statement,
    parameters: &mut HashMap<String, Parameter>,
) -> Result<Execution> {
    save(&session.db, table)?;
    drop_indexes(&session.db, table, indexes)?;
    let execution = ext::reenter(session, "keys", |session| {
        session.execute(statement, parameters)
    })?;
    for (name, sql) in indexes {
        let base = match name.rsplit_once("_r") {
            Some((base, suffix)) if suffix.bytes().all(|b| b.is_ascii_digit()) => base,
            _ => name.as_str(),
        };
        let renamed = format!("{base}_r{}", catalog::next_tag(&session.db)?);
        let marker = format!("INDEX {name} ON ");
        anyhow::ensure!(sql.contains(&marker), "unexpected index definition {sql}");
        session
            .db
            .execute_batch(&sql.replacen(&marker, &format!("INDEX {renamed} ON "), 1))?;
        session.db.execute(
            "UPDATE main.__msduck_keys SET backend_name=? WHERE backend_name=?",
            [&renamed, name],
        )?;
        session.db.execute(
            "UPDATE __msduck_keys_saved_indexes SET backend_name=? WHERE backend_schema=? AND backend_name=?",
            [&renamed, &table.schema, name],
        )?;
    }
    restore(&session.db, table)?;
    Ok(execution)
}
