//! SQL Server's duplicate-key errors for INSERT, UPDATE and MERGE.
//!
//! DuckDB reports which key values collided but not which index or
//! constraint. Keys-managed indexes carry a tag that names them; native
//! DuckDB constraints are matched on the target table by kind and columns.
use super::{catalog, tables};
use crate::engine::{Execution, Parameter, Session, ext};
use anyhow::Result;
use duckdb::Connection;
use msduck_core::diagnostic::SqlError;
use msduck_sql::dialect::ext::keys::{
    message::{self, Duplicate, Kind},
    table::generated_name,
    value,
};
use sqlparser::ast::{FromTable, ObjectName, Statement, TableFactor, TableObject};
use std::collections::HashMap;

fn target(statement: &Statement) -> Option<ObjectName> {
    let relation = |factor: &TableFactor| match factor {
        TableFactor::Table { name, .. } => Some(name.clone()),
        _ => None,
    };
    match statement {
        Statement::Insert(insert) => match &insert.table {
            TableObject::TableName(name) => Some(name.clone()),
            _ => None,
        },
        Statement::Update(update) => relation(&update.table.relation),
        Statement::Merge(merge) => relation(&merge.table),
        Statement::Delete(delete) => match &delete.from {
            FromTable::WithFromKeyword(tables) | FromTable::WithoutKeyword(tables) => {
                relation(&tables.first()?.relation)
            }
        },
        _ => None,
    }
}

pub(super) fn run(
    session: &mut Session,
    statement: &mut Statement,
    parameters: &mut HashMap<String, Parameter>,
) -> Result<Execution> {
    let target = target(statement);
    // SQL Server ends only the statement on a duplicate key, in SQL batches
    // and RPC requests alike (reference/gaps-rpc-procedures.json); the
    // engine continues after a failed INSERT or UPDATE it can identify.
    // `rpc` (set by the keys batch hook) no longer changes this.
    let _batch_is_rpc = session.ext.keys.rpc;
    let command = match statement {
        Statement::Insert(_) => Some(0xc3),
        Statement::Update(_) => Some(0xc5),
        _ => None,
    };
    let result = ext::reenter(session, "keys", |session| {
        session.execute(statement.clone(), parameters)
    });
    result.map_err(|error| translate(session, target.as_ref(), command, error))
}

/// Replace a DuckDB duplicate-key error with SQL Server's, keeping the
/// context the engine attached to the failed statement.
fn translate(
    session: &Session,
    target: Option<&ObjectName>,
    command: Option<u16>,
    error: anyhow::Error,
) -> anyhow::Error {
    let error = match error.downcast::<ext::Partial>() {
        Ok(partial) => {
            return ext::Partial {
                tokens: partial.tokens,
                error: translate(session, target, command, partial.error),
            }
            .into();
        }
        Err(error) => error,
    };
    if error.chain().any(|cause| cause.is::<SqlError>()) {
        return error;
    }
    let Some((text, duplicate)) = error.chain().find_map(|cause| {
        let text = cause.to_string();
        message::duplicate(&text).map(|duplicate| (text, duplicate))
    }) else {
        return error;
    };
    let failed_context = error
        .downcast_ref::<crate::query_error::FailedQuery>()
        .map(|failed| (failed.metadata.clone(), failed.command));
    let describe = |db: &Connection| describe(db, target, &text, &duplicate);
    // A failed statement can abort the DuckDB transaction; read the
    // committed catalog through a separate connection then.
    let described = describe(&session.db).or_else(|_| {
        let other = session.db.try_clone()?;
        other.execute_batch(&format!("USE {}", tables::quote(session.database.alias())))?;
        describe(&other)
    });
    // An unidentified duplicate keeps DuckDB's message (and number 2627),
    // but still ends only its statement.
    let replacement: anyhow::Error = match described {
        Ok(Some((number, state, severity, message))) => {
            SqlError::from_utf16(number, state, severity, message.encode_utf16().collect()).into()
        }
        _ => error,
    };
    if replacement
        .downcast_ref::<crate::query_error::FailedQuery>()
        .is_some()
    {
        return replacement;
    }
    match (failed_context, command) {
        (Some((metadata, failed)), _) => {
            crate::query_error::attach_context(replacement, metadata, failed)
        }
        (None, Some(command)) => crate::query_error::attach_context(replacement, vec![], command),
        (None, None) => replacement,
    }
}

type Diagnostic = (i32, u8, u8, String);

fn describe(
    db: &Connection,
    target: Option<&ObjectName>,
    text: &str,
    duplicate: &Duplicate,
) -> Result<Option<Diagnostic>> {
    let table = match target {
        Some(name) => tables::resolve(db, name)?,
        None => None,
    };
    if let Some(found) = managed(db, table.as_ref(), duplicate)? {
        return Ok(Some(found));
    }
    let Some(table) = table else {
        return Ok(None);
    };
    native(db, &table, text, duplicate)
}

/// A violation of a keys-managed index, named by its tag.
fn managed(
    db: &Connection,
    table: Option<&tables::Table>,
    duplicate: &Duplicate,
) -> Result<Option<Diagnostic>> {
    let Some(tag) = duplicate.tag() else {
        return Ok(None);
    };
    let Some(key) = catalog::by_tag(db, tag)? else {
        return Ok(None);
    };
    if key.backend_name.is_none() || table.is_some_and(|t| t.object_id != key.object_id) {
        return Ok(None);
    }
    let Some(owner) = tables::by_id(db, key.object_id)? else {
        return Ok(None);
    };
    let Some(columns) = key
        .columns
        .iter()
        .map(|name| owner.column(name).cloned())
        .collect::<Option<Vec<_>>>()
    else {
        return Ok(None);
    };
    let Some(values) = duplicate
        .managed_values(value::component_count(&columns))
        .and_then(|values| value::managed_values(&columns, &values))
    else {
        return Ok(None);
    };
    Ok(Some(message::violation(
        key.message_kind(),
        &key.name,
        &owner.qualified(),
        &values,
    )))
}

/// A violation of a native DuckDB PRIMARY KEY or UNIQUE constraint.
fn native(
    db: &Connection,
    table: &tables::Table,
    text: &str,
    duplicate: &Duplicate,
) -> Result<Option<Diagnostic>> {
    let keys: Vec<catalog::Key> = catalog::table(db, table.object_id)?
        .into_iter()
        .filter(|key| key.constraint() && key.native)
        .collect();
    let same = |key: &catalog::Key, names: &[String]| {
        key.columns.len() == names.len()
            && key
                .columns
                .iter()
                .zip(names)
                .all(|(a, b)| a.eq_ignore_ascii_case(b.trim_matches('"')))
    };
    let candidates: Vec<&catalog::Key> = match (duplicate.primary, &duplicate.expressions) {
        (Some(true), _) => keys.iter().filter(|k| k.kind == "PK").collect(),
        (Some(false), Some(names)) => {
            let exact: Vec<_> = keys
                .iter()
                .filter(|k| k.kind == "UQ" && same(k, names))
                .collect();
            if exact.is_empty() {
                // One column whose value itself contains ", ".
                keys.iter()
                    .filter(|k| {
                        k.kind == "UQ"
                            && k.columns.len() == 1
                            && same(k, &names[..1.min(names.len())])
                    })
                    .collect()
            } else {
                exact
            }
        }
        // A duplicate within one statement names neither the kind nor the
        // columns; only an unambiguous width identifies the constraint.
        _ => keys
            .iter()
            .filter(|k| k.columns.len() == duplicate.values.len())
            .collect(),
    };
    let (kind, name, columns) = match candidates.as_slice() {
        [key] => (key.message_kind(), key.name.clone(), key.columns.clone()),
        [_, _, ..] => return Ok(None),
        // A table created before keys were recorded: SQL Server would have
        // generated the name.
        [] => {
            let (Some(primary), Some(names)) = (duplicate.primary, &duplicate.expressions) else {
                return Ok(None);
            };
            if !keys.is_empty() {
                return Ok(None);
            }
            (
                if primary {
                    Kind::PrimaryKey
                } else {
                    Kind::UniqueConstraint
                },
                generated_name(primary, &table.name, table.object_id as u64),
                names
                    .iter()
                    .map(|n| n.trim_matches('"').to_owned())
                    .collect(),
            )
        }
    };
    let Some(columns) = columns
        .iter()
        .map(|name| table.column(name).cloned())
        .collect::<Option<Vec<_>>>()
    else {
        return Ok(None);
    };
    let values: Vec<String> = if columns.len() == 1 {
        let Some(raw) = message::single_value(text) else {
            return Ok(None);
        };
        vec![columns[0].display(&raw, false)]
    } else if columns.len() == duplicate.values.len() {
        columns
            .iter()
            .zip(&duplicate.values)
            .map(|(column, raw)| column.display(raw, false))
            .collect()
    } else {
        return Ok(None);
    };
    Ok(Some(message::violation(
        kind,
        &name,
        &table.qualified(),
        &values,
    )))
}
