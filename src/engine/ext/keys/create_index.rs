//! CREATE INDEX forms beyond the table-owned index catalog's ordinary
//! indexes: unique, CLUSTERED, INCLUDE, filtered, WITH options, descending
//! keys and keys over carrier columns.
//!
//! Each index is registered in the table-owned catalog (`index_catalog`),
//! which assigns its index ID and lets DROP INDEX and `sys.indexes` see it,
//! and in `__msduck_keys`, which keeps what that catalog does not model.
use super::{atomically, build_index, catalog, error, tables};
use crate::engine::{Execution, Session};
use anyhow::Result;
use duckdb::params;
use msduck_sql::dialect::ext::keys::{
    filter, index,
    message::creation,
    value::{self, Column, Storage},
};
use sqlparser::ast::{CreateIndex, Expr, OrderBySort, Statement};

const OPTIONS: &[&str] = &[
    "FILLFACTOR",
    "PAD_INDEX",
    "SORT_IN_TEMPDB",
    "STATISTICS_NORECOMPUTE",
    "STATISTICS_INCREMENTAL",
    "DROP_EXISTING",
    "ONLINE",
    "RESUMABLE",
    "MAX_DURATION",
    "MAXDOP",
    "ALLOW_ROW_LOCKS",
    "ALLOW_PAGE_LOCKS",
    "OPTIMIZE_FOR_SEQUENTIAL_KEY",
    "DATA_COMPRESSION",
    "XML_COMPRESSION",
    "IGNORE_DUP_KEY",
];

fn written(index: &CreateIndex) -> String {
    tables::parts(&index.table_name)
        .map(|(schema, table)| match schema {
            Some(schema) => format!("{schema}.{table}"),
            None => table,
        })
        .unwrap_or_else(|| index.table_name.to_string())
}

fn key_names(index: &CreateIndex) -> Option<Vec<(String, bool)>> {
    index
        .columns
        .iter()
        .map(|column| match &column.column.expr {
            Expr::Identifier(ident) => Some((
                ident.value.clone(),
                column.column.options.sort == Some(OrderBySort::Desc),
            )),
            _ => None,
        })
        .collect()
}

/// Whether the table-owned index catalog implements this index as is.
fn ordinary(index: &CreateIndex, table: &tables::Table) -> bool {
    let distinct = key_names(index).is_some_and(|keys| {
        keys.iter().enumerate().all(|(i, (name, _))| {
            !keys[..i]
                .iter()
                .any(|(other, _)| other.eq_ignore_ascii_case(name))
        })
    });
    distinct
        && !index.unique
        && index.using.is_none()
        && index.include.is_empty()
        && index.predicate.is_none()
        && index.with.is_empty()
        && key_names(index).is_some_and(|keys| {
            keys.iter().all(|(name, descending)| {
                !descending
                    && table.column(name).is_some_and(|c| {
                        !matches!(
                            c.kind(),
                            Ok(Storage::Unicode | Storage::Ticks { .. }) | Err(_)
                        )
                    })
            })
        })
}

pub(super) fn run(session: &mut Session, statement: &Statement) -> Result<Option<Execution>> {
    let Statement::CreateIndex(index) = statement else {
        return Ok(None);
    };
    let Some(table) = tables::resolve(&session.db, &index.table_name)? else {
        // The built-in path reports the missing table.
        return Ok(None);
    };
    // An index named like one of the table's constraints fails with 1913.
    let named_like_constraint = match index.name.as_ref().map(|n| n.0.as_slice()) {
        Some([sqlparser::ast::ObjectNamePart::Identifier(name)]) => {
            catalog::table(&session.db, table.object_id)?
                .iter()
                .any(|k| k.constraint() && k.name.eq_ignore_ascii_case(&name.value))
        }
        _ => false,
    };
    if !named_like_constraint && ordinary(index, &table) {
        return Ok(None);
    }
    session.require_committable()?;
    let name = match index.name.as_ref().map(|n| n.0.as_slice()) {
        Some([sqlparser::ast::ObjectNamePart::Identifier(ident)]) => ident.value.clone(),
        _ => anyhow::bail!("index name must be one identifier"),
    };
    let options = index::options(index);
    let mut drop_existing = false;
    for (option, value) in &options {
        if !OPTIONS.contains(&option.as_str()) {
            return Err(error((
                155,
                1,
                15,
                format!("'{option}' is not a recognized CREATE INDEX option."),
            )));
        }
        match (option.as_str(), value.as_str()) {
            ("IGNORE_DUP_KEY", "ON") if !index.unique => {
                return Err(error((
                    1916,
                    4,
                    16,
                    "CREATE INDEX options nonunique and ignore_dup_key are mutually exclusive."
                        .into(),
                )));
            }
            ("IGNORE_DUP_KEY", "ON") => {
                anyhow::bail!("unsupported index option IGNORE_DUP_KEY = ON")
            }
            ("DROP_EXISTING", "ON") => drop_existing = true,
            ("FILLFACTOR", fill) if !fill.parse::<u8>().is_ok_and(|f| (1..=100).contains(&f)) => {
                return Err(error((
                    129,
                    1,
                    15,
                    format!(
                        "Fillfactor {fill} is not a valid percentage; fillfactor must be between 1 and 100."
                    ),
                )));
            }
            _ => {}
        }
    }
    let Some(keys) = key_names(index) else {
        anyhow::bail!("index expressions are unsupported");
    };
    // Key order is not stored; sys.index_columns reports it.
    let descending = keys
        .iter()
        .zip(1..)
        .filter_map(|((_, descending), ordinal)| descending.then_some(ordinal))
        .collect::<Vec<i32>>();
    let mut columns: Vec<Column> = vec![];
    for (key, _) in &keys {
        let Some(column) = table.column(key) else {
            return Err(error((
                1911,
                1,
                16,
                format!("Column name '{key}' does not exist in the target table, index or view."),
            )));
        };
        if columns.iter().any(|c| c.name == column.name) {
            return Err(error((
                1909,
                1,
                16,
                format!(
                    "Cannot use duplicate column names in index. Column name '{key}' listed more than once."
                ),
            )));
        }
        if !column.keyable() {
            return Err(error(value::invalid_key(&column.name, &table.name)));
        }
        column.kind().map_err(anyhow::Error::msg)?;
        columns.push(column.clone());
    }
    let mut include = vec![];
    for ident in &index.include {
        let Some(column) = table.column(&ident.value) else {
            return Err(error((
                1911,
                1,
                16,
                format!(
                    "Column name '{}' does not exist in the target table, index or view.",
                    ident.value
                ),
            )));
        };
        include.push(column.name.clone());
    }
    let lowered = match &index.predicate {
        Some(predicate) => Some(
            filter::lower(predicate, &table.columns).map_err(|e| match e {
                filter::Error::InvalidColumn(name) => error(filter::invalid_column(&name)),
                filter::Error::Unsupported => anyhow::anyhow!(filter::UNSUPPORTED),
            })?,
        ),
        None => None,
    };
    let clustered = index::clustered(index);
    let target = written(index);
    atomically(session, |session| {
        let db = &session.db;
        catalog::prune(db)?;
        crate::index_catalog::sync(db)?;
        let keys = catalog::table(db, table.object_id)?;
        let registered = crate::index_catalog::acquire(db)?;
        let existing = registered
            .iter()
            .find(|i| i.object_id == table.object_id && i.name.eq_ignore_ascii_case(&name));
        let constraint = keys
            .iter()
            .any(|k| k.constraint() && k.name.eq_ignore_ascii_case(&name));
        if drop_existing && existing.is_none() {
            if constraint {
                anyhow::bail!("unsupported DROP_EXISTING for a constraint index");
            }
            return Err(error((
                7999,
                9,
                16,
                format!("Could not find any index named '{name}' for table '{target}'."),
            )));
        }
        if !drop_existing && (existing.is_some() || constraint) {
            return Err(error((
                1913,
                1,
                16,
                format!(
                    "The operation failed because an index or statistics with name '{name}' already exists on table '{target}'."
                ),
            )));
        }
        if clustered
            && let Some(other) = keys
                .iter()
                .find(|k| k.clustered && !(drop_existing && k.name.eq_ignore_ascii_case(&name)))
        {
            return Err(error((
                1902,
                3,
                16,
                format!(
                    "Cannot create more than one clustered index on table '{target}'. Drop the existing clustered index '{}' before creating another.",
                    other.name
                ),
            )));
        }
        let tag = catalog::next_tag(db)?;
        // Every check runs before the first change: inside a user
        // transaction a later failure cannot undo one.
        if index.unique {
            check_duplicates(db, &table, &name, &columns, lowered.as_deref())?;
        }
        // DROP_EXISTING rebuilds the same index, which keeps its ID.
        let mut kept_id: Option<i32> = None;
        if let Some(existing) = existing.filter(|_| drop_existing) {
            kept_id = db.query_row(
                "SELECT min(index_id) FROM main.__msduck_index_ids WHERE incarnation=?",
                [existing.incarnation],
                |r| r.get(0),
            )?;
            crate::index_catalog::drop_index(
                db,
                existing,
                crate::index_catalog::Transaction::CallerOwned,
            )?;
            if let Some(key) = keys
                .iter()
                .find(|k| k.incarnation == Some(existing.incarnation))
            {
                catalog::remove(db, key.tag)?;
            }
        }
        let incarnation: i64 = db.query_row(
            "SELECT nextval('main.__msduck_index_incarnations')",
            [],
            |r| r.get(0),
        )?;
        let backend = format!("__msduck_index_{incarnation}");
        build_index(
            db,
            &table,
            &backend,
            tag,
            index.unique,
            &columns,
            lowered.as_deref(),
        )?;
        register(
            db,
            &table,
            &name,
            index.unique,
            incarnation,
            &backend,
            &columns,
        )?;
        for ordinal in &descending {
            db.execute(
                "INSERT INTO main.__msduck_index_descending VALUES(?,?)",
                params![incarnation, ordinal],
            )?;
        }
        catalog::insert(
            db,
            &catalog::Key {
                tag,
                object_id: table.object_id,
                name: name.clone(),
                kind: "IX".into(),
                unique: index.unique,
                clustered,
                native: false,
                backend_name: Some(backend),
                incarnation: Some(incarnation),
                columns: columns.iter().map(|c| c.name.clone()).collect(),
                include,
                filter: index.predicate.as_ref().map(|p| p.to_string()),
                filter_columns: index
                    .predicate
                    .as_ref()
                    .map(|p| filter::columns(p, &table.columns))
                    .unwrap_or_default(),
            },
        )?;
        if let Some(id) = kept_id {
            db.execute(
                "INSERT INTO main.__msduck_index_ids VALUES(?,NULL,?,?,NULL,NULL)",
                params![table.object_id, incarnation, id],
            )?;
        }
        // Record the index's sys.indexes ID now.
        crate::index_catalog::sync(db)?;
        Ok(Some(Execution::statement(vec![], None, 200)))
    })
}

/// SQL Server's 1505 when existing rows already repeat a new unique key.
fn check_duplicates(
    db: &duckdb::Connection,
    table: &tables::Table,
    name: &str,
    columns: &[Column],
    filter: Option<&str>,
) -> Result<()> {
    match first_duplicate(db, table, name, columns, filter)? {
        Some(diagnostic) => Err(error(diagnostic)),
        None => Ok(()),
    }
}

/// SQL Server's 1505 for the first duplicate in key order of existing rows
/// under a new unique key, if any.
pub(super) fn first_duplicate(
    db: &duckdb::Connection,
    table: &tables::Table,
    name: &str,
    columns: &[Column],
    filter: Option<&str>,
) -> Result<Option<(i32, u8, u8, String)>> {
    // The first duplicate in key order, NULL keys first, as SQL Server
    // reports it. The typed values order; their text is read.
    let mut components = vec![];
    let mut order = vec![];
    for column in columns {
        let parts = column.components().map_err(anyhow::Error::msg)?;
        if column.nullable {
            order.push(format!("{} DESC", components.len() + 1));
            order.push(format!("{}", components.len() + 2));
        } else {
            order.push(format!("{}", components.len() + 1));
        }
        components.extend(parts);
    }
    // Character keys drop trailing spaces and may fold case; the duplicate
    // is shown as stored, the least of the equal values.
    // One stored row's character values (the least, by their hexadecimal
    // text), so the shown tuple is an actual duplicate.
    let stored: Vec<Option<String>> = columns
        .iter()
        .map(|column| match column.kind() {
            Ok(value::Storage::Unicode) => Some(format!(
                "coalesce(hex(struct_extract({}, '__msduck_utf16le')), '-')",
                column.quoted()
            )),
            Ok(value::Storage::Ansi) => Some(format!("coalesce(hex({}), '-')", column.quoted())),
            _ => None,
        })
        .collect();
    let representative = (stored.iter().any(Option::is_some)).then(|| {
        format!(
            "min(concat_ws('|', {}))",
            stored
                .iter()
                .flatten()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        )
    });
    let selected = components
        .iter()
        .cloned()
        .chain(components.iter().map(|c| format!("CAST({c} AS VARCHAR)")))
        .chain(representative.iter().cloned())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT {selected} FROM {} WHERE {} GROUP BY ALL HAVING count(*) > 1 ORDER BY {} LIMIT 1",
        table.backend(),
        filter.unwrap_or("true"),
        order.join(", ")
    );
    let mut statement = db.prepare(&sql)?;
    let width = components.len();
    let extra = usize::from(representative.is_some());
    let mut rows = statement.query_map([], |row| {
        let values = (width..2 * width)
            .map(|i| row.get::<_, String>(i))
            .collect::<duckdb::Result<Vec<_>>>()?;
        let raw = (2 * width..2 * width + extra)
            .map(|i| row.get::<_, Option<String>>(i))
            .collect::<duckdb::Result<Vec<_>>>()?;
        Ok((values, raw))
    })?;
    let Some((values, raw)) = rows.next().transpose()? else {
        return Ok(None);
    };
    let mut shown = value::managed_values(columns, &values).unwrap_or_default();
    let representative = raw.into_iter().flatten().next().unwrap_or_default();
    let mut raw = representative.split('|');
    for ((column, stored), shown) in columns.iter().zip(&stored).zip(shown.iter_mut()) {
        if stored.is_some()
            && let Some(text) = raw.next()
            && text != "-"
            && shown != "<NULL>"
        {
            *shown = column.display(text, true);
        }
    }
    Ok(Some(creation(name, &table.qualified(), &shown)))
}

/// Register the index in the table-owned catalog, as its `create` does.
fn register(
    db: &duckdb::Connection,
    table: &tables::Table,
    name: &str,
    unique: bool,
    incarnation: i64,
    backend: &str,
    columns: &[Column],
) -> Result<()> {
    let used: std::collections::HashSet<i32> = crate::index_catalog::acquire(db)?
        .into_iter()
        .filter(|i| i.object_id == table.object_id)
        .map(|i| i.index_id)
        .collect();
    let index_id = (2..i32::MAX)
        .find(|n| !used.contains(n))
        .ok_or_else(|| anyhow::anyhow!("index ID space exhausted"))?;
    let table_oid: i64 = db.query_row(
        "SELECT CAST(table_oid AS BIGINT) FROM duckdb_tables()
         WHERE database_name=current_database() AND schema_name=? AND table_name=?",
        [&table.schema, &table.name],
        |r| r.get(0),
    )?;
    db.execute(
        "INSERT INTO main.__msduck_index_catalog VALUES(?,?,?,lower(?),?,?,?,?,?)",
        params![
            table.object_id,
            index_id,
            name,
            name,
            unique,
            incarnation,
            table.schema,
            backend,
            table_oid
        ],
    )?;
    for (ordinal, column) in columns.iter().enumerate() {
        let column_id: i32 = db.query_row(
            "SELECT column_id FROM sys.columns WHERE object_id=? AND name=?",
            params![table.object_id, column.name],
            |r| r.get(0),
        )?;
        db.execute(
            "INSERT INTO main.__msduck_index_keys VALUES(?,?,?,?)",
            params![incarnation, ordinal as i32 + 1, column_id, column.name],
        )?;
    }
    Ok(())
}
