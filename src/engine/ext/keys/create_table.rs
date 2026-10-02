//! PRIMARY KEY and UNIQUE constraints of CREATE TABLE.
use super::{atomically, build_index, catalog, tables};
use crate::engine::{Execution, Parameter, Session, StatementErrors, ext};
use anyhow::Result;
use msduck_core::diagnostic::SqlError;
use msduck_sql::dialect::ext::keys::table::{self as plan, Constraint};
use sqlparser::ast::{CreateTable, Statement};
use std::collections::HashMap;

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
    // The CLUSTERED or NONCLUSTERED keyword each key was declared with,
    // which tokenizing drops (see the catalog feature).
    let declared = session.ext.catalog.take_keys(&table.name);
    let owned = session.transactions == 0;
    match atomically(session, |session| {
        create(
            session,
            &table,
            constraints.clone(),
            declared.clone(),
            parameters,
        )
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
                create(session, &table, managed, declared, parameters)
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
    declared: Vec<msduck_sql::dialect::ext::catalog::declarations::Key>,
    parameters: &mut HashMap<String, Parameter>,
) -> Result<Execution> {
    catalog::prune(&session.db)?;
    let (schema, _) = tables::parts(&table.name).expect("checked one- or two-part name");
    let schema = schema.unwrap_or_else(|| "dbo".into());
    // Constraint names are schema-scoped objects. An existing table is
    // reported first, by the CREATE TABLE itself.
    let exists = tables::resolve(&session.db, &table.name)?.is_some();
    for name in constraints
        .iter()
        .filter(|_| !exists)
        .filter_map(|c| c.name.as_deref())
    {
        let taken: i64 = session.db.query_row(
            "SELECT count(*) FROM main.__msduck_keys k JOIN sys.objects o USING(object_id)
             JOIN sys.schemas s USING(schema_id)
             WHERE k.kind IN ('PK','UQ') AND lower(k.name)=lower(?) AND lower(s.name)=lower(?)",
            [name, &schema],
            |r| r.get(0),
        )?;
        if taken > 0 {
            return Err(duplicate_name(name));
        }
    }
    let mut declared = declared;
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
        let clustered = declared_clustering(&mut declared, constraint);
        catalog::insert(
            &session.db,
            &catalog::Key {
                tag,
                object_id: created.object_id,
                name: constraint.name.clone().unwrap_or_else(|| {
                    plan::generated_name(
                        constraint.primary,
                        &created.name,
                        ((created.object_id as u64) << 20) ^ tag as u64,
                    )
                }),
                kind: if constraint.primary { "PK" } else { "UQ" }.into(),
                unique: true,
                // Only an explicit CLUSTERED: a PRIMARY KEY that is clustered
                // by default gives way to a later clustered index.
                clustered: clustered == Some(true),
                native: constraint.native,
                backend_name: backend,
                incarnation: None,
                columns: columns.iter().map(|c| c.name.clone()).collect(),
                include: vec![],
                filter: None,
                filter_columns: vec![],
            },
        )?;
        catalog::record_layout(&session.db, tag, clustered, &descending(table, constraint))?;
    }
    Ok(execution)
}

/// The keyword a key was declared with: the first declared key of the same
/// kind over the same columns.
fn declared_clustering(
    declared: &mut Vec<msduck_sql::dialect::ext::catalog::declarations::Key>,
    constraint: &Constraint,
) -> Option<bool> {
    use msduck_sql::dialect::ext::catalog::declarations::KeyKind;
    let kind = if constraint.primary {
        KeyKind::Primary
    } else {
        KeyKind::Unique
    };
    let position = declared.iter().position(|key| {
        key.kind == kind
            && key.columns.len() == constraint.columns.len()
            && key
                .columns
                .iter()
                .zip(&constraint.columns)
                .all(|(a, b)| a.eq_ignore_ascii_case(b))
    })?;
    declared.remove(position).clustered
}

/// The 1-based ordinals of a table constraint's DESC key columns.
fn descending(table: &CreateTable, constraint: &Constraint) -> Vec<i32> {
    let plan::Origin::Table(index) = constraint.origin else {
        return Vec::new();
    };
    let columns = match table.constraints.get(index) {
        Some(sqlparser::ast::TableConstraint::PrimaryKey(key)) => &key.columns,
        Some(sqlparser::ast::TableConstraint::Unique(key)) => &key.columns,
        _ => return Vec::new(),
    };
    columns
        .iter()
        .enumerate()
        .filter(|(_, column)| {
            matches!(
                column.column.options.sort,
                Some(sqlparser::ast::OrderBySort::Desc)
            )
        })
        .map(|(ordinal, _)| ordinal as i32 + 1)
        .collect()
}
