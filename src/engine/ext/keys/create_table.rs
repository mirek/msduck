//! PRIMARY KEY and UNIQUE constraints of CREATE TABLE.
use super::{atomically, build_index, catalog, tables};
use crate::engine::{Execution, Parameter, Session, StatementErrors, ext};
use anyhow::Result;
use msduck_core::diagnostic::SqlError;
use msduck_sql::dialect::ext::keys::table::{self as plan, Constraint};
use sqlparser::ast::{CreateTable, Statement};
use std::collections::{HashMap, HashSet};

/// DuckDB rejects STRUCT and other storage types as index keys.
const INVALID_KEY_TYPE: &str = "Invalid type for index key";

pub(super) fn run(
    session: &mut Session,
    statement: &mut Statement,
    parameters: &mut HashMap<String, Parameter>,
) -> Result<Option<Execution>> {
    let Statement::CreateTable(table) = statement else {
        return Ok(None);
    };
    if tables::parts(&table.name).is_none() || table.query.is_some() {
        return Ok(None);
    }
    let constraints = plan::constraints(table).map_err(|errors| {
        anyhow::Error::from(StatementErrors(
            errors
                .into_iter()
                .map(|(number, state, severity, message)| {
                    SqlError::from_utf16(number, state, severity, message.encode_utf16().collect())
                })
                .collect(),
        ))
    })?;
    if constraints.is_empty() {
        return Ok(None);
    }
    let table = table.clone();
    let owned = session.transactions == 0;
    match atomically(session, |session| {
        create(session, &table, constraints.clone(), parameters)
    }) {
        // A user-defined type can hide unindexable storage; enforce every key
        // through managed indexes then.
        Err(failure) if owned && failure.to_string().contains(INVALID_KEY_TYPE) => {
            let managed = constraints
                .into_iter()
                .map(|c| Constraint {
                    native: false,
                    managed: true,
                    ..c
                })
                .collect();
            atomically(session, |session| {
                create(session, &table, managed, parameters)
            })
            .map(Some)
        }
        result => result.map(Some),
    }
}

fn duplicate_name(name: &str) -> anyhow::Error {
    StatementErrors(vec![
        SqlError::new(
            2714,
            5,
            format!("There is already an object named '{name}' in the database."),
        ),
        SqlError::new(
            1750,
            1,
            "Could not create constraint or index. See previous errors.",
        ),
    ])
    .into()
}

fn create(
    session: &mut Session,
    table: &CreateTable,
    constraints: Vec<Constraint>,
    parameters: &mut HashMap<String, Parameter>,
) -> Result<Execution> {
    catalog::prune(&session.db)?;
    let (schema, object_name) = tables::parts(&table.name).expect("checked one- or two-part name");
    let schema = schema.unwrap_or_else(|| "dbo".into());
    // Constraint names are schema-scoped objects. An existing table is
    // reported first, by the CREATE TABLE itself.
    let exists = tables::resolve(&session.db, &table.name)?.is_some();
    let mut declared = HashSet::new();
    // Include the new table and non-key constraint names so a namespace
    // conflict cannot fail only after native DDL in a caller transaction.
    let other_names = std::iter::once(object_name.as_str())
        .chain(
            table
                .columns
                .iter()
                .flat_map(|column| column.options.iter())
                .filter(|option| {
                    !matches!(
                        option.option,
                        sqlparser::ast::ColumnOption::PrimaryKey(_)
                            | sqlparser::ast::ColumnOption::Unique(_)
                    )
                })
                .filter_map(|option| option.name.as_ref().map(|name| name.value.as_str())),
        )
        .chain(
            table
                .constraints
                .iter()
                .filter_map(|constraint| match constraint {
                    sqlparser::ast::TableConstraint::Check(check) => check.name.as_ref(),
                    sqlparser::ast::TableConstraint::ForeignKey(key) => key.name.as_ref(),
                    _ => None,
                })
                .map(|name| name.value.as_str()),
        );
    for name in other_names {
        let normalized: String = session
            .db
            .query_row("SELECT lower(?)", [name], |r| r.get(0))?;
        declared.insert(normalized);
    }
    for name in constraints
        .iter()
        .filter(|_| !exists)
        .filter_map(|c| c.name.as_deref())
    {
        let normalized: String = session
            .db
            .query_row("SELECT lower(?)", [name], |r| r.get(0))?;
        if !declared.insert(normalized)
            || crate::object_catalog::key_name_exists(&session.db, &schema, name)?
        {
            return Err(duplicate_name(name));
        }
    }
    let mut native = table.clone();
    plan::strip(&mut native, &constraints);
    let execution = ext::reenter(session, "keys", |session| {
        session.execute(Statement::CreateTable(native), parameters)
    })?;
    // A table this database cannot see by name (for example one renamed by
    // another feature) keeps only its native constraints.
    let Some(created) = tables::resolve(&session.db, &table.name)? else {
        anyhow::ensure!(
            constraints.iter().all(|c| !c.managed),
            "unsupported PRIMARY KEY or UNIQUE constraint on this table"
        );
        return Ok(execution);
    };
    for constraint in &constraints {
        let tag = catalog::next_tag(&session.db)?;
        let columns = constraint
            .columns
            .iter()
            .map(|name| {
                created
                    .column(name)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("key column {name} is missing"))
            })
            .collect::<Result<Vec<_>>>()?;
        let backend = if constraint.managed {
            let backend = format!("__msduck_key_{tag}");
            build_index(&session.db, &created, &backend, tag, true, &columns, None)?;
            Some(backend)
        } else {
            None
        };
        let name = if let Some(name) = &constraint.name {
            name.clone()
        } else {
            // Generated names share the schema namespace with user objects.
            // Resolve collisions instead of failing after native CREATE TABLE
            // has succeeded inside a caller-owned transaction.
            let mut seed = ((created.object_id as u64) << 20) ^ tag as u64;
            loop {
                let candidate = plan::generated_name(constraint.primary, &created.name, seed);
                let normalized: String =
                    session
                        .db
                        .query_row("SELECT lower(?)", [&candidate], |r| r.get(0))?;
                if !declared.contains(&normalized)
                    && !crate::object_catalog::key_name_exists(&session.db, &schema, &candidate)?
                {
                    declared.insert(normalized);
                    break candidate;
                }
                seed = seed.wrapping_add(1);
            }
        };
        catalog::insert(
            &session.db,
            &catalog::Key {
                tag,
                object_id: created.object_id,
                name,
                kind: if constraint.primary { "PK" } else { "UQ" }.into(),
                unique: true,
                clustered: false,
                native: constraint.native,
                backend_name: backend,
                incarnation: None,
                columns: columns.iter().map(|c| c.name.clone()).collect(),
                include: vec![],
                filter: None,
                filter_columns: vec![],
            },
        )?;
        // The original plan knows whether SQL Server supplied the name.
        // Legacy rows do not, so backfill deliberately retains NULL provenance.
        session.db.execute(
            "UPDATE main.__msduck_key_objects SET is_system_named=? WHERE key_tag=?",
            duckdb::params![constraint.name.is_none(), tag],
        )?;
    }
    Ok(execution)
}
