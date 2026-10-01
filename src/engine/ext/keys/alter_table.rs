//! ALTER TABLE on a table with key constraints or indexes.
//!
//! - DROP COLUMN, and ALTER COLUMN changing a column's type, fail with 5074
//!   and 4922 when a key constraint or index uses the column, as in SQL
//!   Server. Widening a variable-length character or binary key column is
//!   allowed.
//! - DuckDB refuses most ALTER TABLE forms while the table has an index
//!   (even ADD COLUMN, when the column gets a default or NOT NULL). For
//!   column changes the table's indexes are dropped before the change and
//!   recreated from their definitions after it, in one transaction
//!   ([`within_transaction`]). Outside a user transaction, when DuckDB still
//!   refuses because of the dropped indexes, the drop commits first
//!   ([`two_phase`]).
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
            columns: key
                .columns
                .iter()
                .chain(&key.include)
                .chain(&key.filter_columns)
                .cloned()
                .collect(),
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
    // Only column changes are rebuilt around; DuckDB keeps refusing other
    // forms (such as renames, after which the saved definitions would no
    // longer bind) while the table has indexes.
    if !alter.operations.iter().all(|operation| {
        matches!(
            operation,
            AlterTableOperation::AddColumn { .. }
                | AlterTableOperation::DropColumn { .. }
                | AlterTableOperation::AlterColumn { .. }
        )
    }) {
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
        return within_transaction(session, &table, &indexes, statement, parameters, true)
            .map(Some);
    }
    // Rebuild atomically in an owned transaction when DuckDB allows it.
    match atomically(session, |session| {
        within_transaction(
            session,
            &table,
            &indexes,
            statement.clone(),
            parameters,
            false,
        )
    }) {
        Ok(execution) => Ok(Some(execution)),
        Err(error) if depends_on_dropped_index(&error) => {
            two_phase(session, &table, &indexes, statement, parameters).map(Some)
        }
        Err(error) => Err(error),
    }
}

/// DuckDB still sees an index dropped in the open transaction when it
/// checks DROP COLUMN and ALTER COLUMN.
fn depends_on_dropped_index(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        let text = cause.to_string();
        text.contains("an index depends on") || text.contains("Dependency Error")
    })
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
         INSERT INTO main.__msduck_index_keys SELECT * FROM __msduck_keys_saved_columns",
    )?;
    forget_saved(db);
    Ok(())
}

fn forget_saved(db: &duckdb::Connection) {
    let _ = db.execute_batch(
        "DROP TABLE IF EXISTS __msduck_keys_saved_indexes;
         DROP TABLE IF EXISTS __msduck_keys_saved_columns",
    );
}

/// Recreate the dropped indexes under new backend names (DuckDB cannot
/// reuse a name dropped in the same transaction); the catalogs follow.
fn recreate_renamed(
    db: &duckdb::Connection,
    table: &tables::Table,
    indexes: &[(String, String)],
) -> Result<()> {
    for (name, sql) in indexes {
        let base = match name.rsplit_once("_r") {
            Some((base, suffix)) if suffix.bytes().all(|b| b.is_ascii_digit()) => base,
            _ => name.as_str(),
        };
        let renamed = format!("{base}_r{}", catalog::next_tag(db)?);
        let marker = format!("INDEX {name} ON ");
        anyhow::ensure!(sql.contains(&marker), "unexpected index definition {sql}");
        db.execute_batch(&sql.replacen(&marker, &format!("INDEX {renamed} ON "), 1))?;
        db.execute(
            "UPDATE main.__msduck_keys SET backend_name=? WHERE backend_name=?",
            [&renamed, name],
        )?;
        db.execute(
            "UPDATE __msduck_keys_saved_indexes SET backend_name=? WHERE backend_schema=? AND backend_name=?",
            [&renamed, &table.schema, name],
        )?;
    }
    restore(db, table)
}

/// Drop, change and recreate in the current transaction. Inside a user
/// transaction a failed change must not leave the indexes dropped: they
/// are recreated before the error is returned, and if even that fails the
/// transaction can only roll back.
fn within_transaction(
    session: &mut Session,
    table: &tables::Table,
    indexes: &[(String, String)],
    statement: Statement,
    parameters: &mut HashMap<String, Parameter>,
    user: bool,
) -> Result<Execution> {
    save(&session.db, table)?;
    drop_indexes(&session.db, table, indexes)?;
    let result = ext::reenter(session, "keys", |session| {
        session.execute(statement, parameters)
    });
    match result {
        Ok(execution) => {
            recreate_renamed(&session.db, table, indexes)?;
            Ok(execution)
        }
        Err(error) => {
            if user && recreate_renamed(&session.db, table, indexes).is_err() {
                session.transaction_doomed = true;
            }
            forget_saved(&session.db);
            Err(error)
        }
    }
}

/// Outside a user transaction, when DuckDB needs the drop committed first:
/// commit the drop, run the change, then recreate each index and restore
/// the catalog rows, whether or not the change succeeded. Writers in other
/// sessions can act between the commits.
fn two_phase(
    session: &mut Session,
    table: &tables::Table,
    indexes: &[(String, String)],
    statement: Statement,
    parameters: &mut HashMap<String, Parameter>,
) -> Result<Execution> {
    atomically(session, |session| {
        save(&session.db, table)?;
        drop_indexes(&session.db, table, indexes)
    })?;
    let result = ext::reenter(session, "keys", |session| {
        session.execute(statement, parameters)
    });
    // One index failing to return (another session's duplicate, say) does
    // not keep the others away.
    let mut failure = None;
    for (_, sql) in indexes {
        if let Err(error) = atomically(session, |session| Ok(session.db.execute_batch(sql)?)) {
            failure.get_or_insert(error);
        }
    }
    if let Err(error) = atomically(session, |session| restore(&session.db, table)) {
        failure.get_or_insert(error);
    }
    forget_saved(&session.db);
    let execution = result?;
    match failure {
        Some(error) => Err(error.context("an index could not be recreated after ALTER TABLE")),
        None => Ok(execution),
    }
}
