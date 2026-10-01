//! Declarations as written, for the catalog views.
//!
//! SQL Server's catalog shows what a batch declared: the text of a view,
//! the expressions of DEFAULT constraints and computed columns, and the
//! CLUSTERED or NONCLUSTERED keyword of a key constraint. Other features
//! rewrite those statements before this feature sees them (see
//! `msduck_sql::dialect::ext::catalog::declarations`), so each batch's own
//! text is read when it starts:
//!
//! - a CREATE or ALTER VIEW batch keeps its text, which becomes the view's
//!   definition when the statement succeeds;
//! - the keys of CREATE TABLE statements wait for the keys feature, which
//!   records their clustering and key order (`take_keys`);
//! - after the batch, DEFAULT and computed-column text replaces what the
//!   rewritten statements left, for objects the batch created, and the
//!   clustering of keys that ALTER TABLE added is recorded. An unnamed
//!   DEFAULT that ALTER TABLE ... ADD declared gets its SQL Server object
//!   here too.
//!
//! Object IDs only grow, so "created by this batch" means an ID above the
//! one observed when the batch started. Bodies of procedures and other
//! modules are not batches: what they declare keeps the rewritten text
//! (or none) and SQL Server's default clustering.
use crate::engine::{Execution, Parameter, Session, ext};
use anyhow::Result;
use msduck_sql::dialect::ext::catalog::declarations::{self, Declarations, Key, KeyKind};
use sqlparser::ast::{ObjectName, Statement};
use std::collections::HashMap;

/// Per-session state: one frame per (nested) batch.
#[derive(Default)]
pub(crate) struct State {
    frames: Vec<Frame>,
}

#[derive(Default)]
struct Frame {
    /// The text of a CREATE or ALTER VIEW batch.
    view: Option<String>,
    declarations: Declarations,
    keys: Vec<Key>,
    /// The highest object ID and key tag when the batch started.
    watermark: Option<(i64, i64)>,
    /// Columns that ALTER TABLE ... ADD declares and that already existed.
    existing: Vec<(i32, String)>,
}

/// Whether two names denote the same table: the same last part, and the
/// same schema when both give one.
fn same_table(written: &[String], name: &ObjectName) -> bool {
    let parts: Vec<String> = name
        .0
        .iter()
        .filter_map(|part| part.as_ident().map(|ident| ident.value.clone()))
        .collect();
    let (Some(a), Some(b)) = (written.last(), parts.last()) else {
        return false;
    };
    if !a.eq_ignore_ascii_case(b) {
        return false;
    }
    match (
        written.len().checked_sub(2).map(|i| &written[i]),
        parts.len().checked_sub(2).map(|i| &parts[i]),
    ) {
        (Some(a), Some(b)) if !a.is_empty() && !b.is_empty() => a.eq_ignore_ascii_case(b),
        _ => true,
    }
}

impl State {
    /// The key constraints the first not yet executed CREATE TABLE of
    /// `table` in the current batch declares, with their clustering.
    pub(in crate::engine) fn take_keys(&mut self, table: &ObjectName) -> Vec<Key> {
        let Some(frame) = self.frames.last_mut() else {
            return Vec::new();
        };
        let Some(statement) = frame
            .keys
            .iter()
            .filter(|key| key.create && same_table(&key.table, table))
            .map(|key| key.statement)
            .min()
        else {
            return Vec::new();
        };
        let (taken, kept) = std::mem::take(&mut frame.keys)
            .into_iter()
            .partition(|key| key.create && key.statement == statement);
        frame.keys = kept;
        taken
    }
}

pub(super) fn begin(session: &mut Session) {
    session.ext.catalog.frames.push(Frame::default());
}

fn contains(text: &str, word: &str) -> bool {
    text.as_bytes()
        .windows(word.len())
        .any(|window| window.eq_ignore_ascii_case(word.as_bytes()))
}

/// Read a batch's declarations before other features rewrite them.
pub(super) fn batch(session: &mut Session, sql: &str) {
    if session.ext.catalog.frames.is_empty() {
        session.ext.catalog.frames.push(Frame::default());
    }
    let mut frame = Frame::default();
    if contains(sql, "VIEW") {
        let words = msduck_sql::dialect::ext::leading_words(sql, 4);
        let words: Vec<&str> = words.iter().map(String::as_str).collect();
        if matches!(
            words.as_slice(),
            ["CREATE" | "ALTER", "VIEW", ..] | ["CREATE", "OR", "ALTER", "VIEW"]
        ) {
            frame.view = Some(sql.to_owned());
        }
    }
    if contains(sql, "TABLE") && (contains(sql, "CREATE") || contains(sql, "ALTER")) {
        frame.declarations = declarations::declarations(sql);
        frame.keys = declarations::keys(sql);
        if !frame.declarations.is_empty() || frame.keys.iter().any(|key| !key.create) {
            frame.watermark = watermark(&session.db).ok();
            frame.existing = existing(&session.db, &frame.declarations);
        }
    }
    if let Some(top) = session.ext.catalog.frames.last_mut() {
        *top = frame;
    }
}

fn watermark(db: &duckdb::Connection) -> Result<(i64, i64)> {
    Ok(db.query_row(
        "SELECT (SELECT coalesce(max(object_id),0) FROM (SELECT object_id FROM main.__msduck_objects
           UNION ALL SELECT object_id FROM main.__msduck_default_constraints
           UNION ALL SELECT object_id FROM main.__msduck_constraints
           UNION ALL SELECT object_id FROM main.__msduck_modules)),
         (SELECT coalesce(max(tag),0) FROM main.__msduck_keys)",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?)
}

fn object_id(db: &duckdb::Connection, table: &[String]) -> Option<i32> {
    let name = table.last()?;
    if name.starts_with(['#', '@']) {
        return None;
    }
    let written = table
        .iter()
        .map(|part| format!("[{}]", part.replace(']', "]]")))
        .collect::<Vec<_>>()
        .join(".");
    db.query_row("SELECT __msduck_object_id(?,'U')", [written], |row| {
        row.get(0)
    })
    .ok()
    .flatten()
}

fn column_id(db: &duckdb::Connection, object_id: i32, column: &str) -> Option<i32> {
    db.query_row(
        "SELECT column_id FROM main.__msduck_column_info WHERE object_id=? AND lower(name)=lower(?)",
        duckdb::params![object_id, column],
        |row| row.get(0),
    )
    .ok()
}

/// Columns of ALTER TABLE ... ADD that already exist before the batch.
fn existing(db: &duckdb::Connection, declarations: &Declarations) -> Vec<(i32, String)> {
    declarations
        .columns
        .iter()
        .filter(|column| !column.create)
        .filter_map(|column| {
            let id = object_id(db, &column.table)?;
            column_id(db, id, &column.column).map(|_| (id, column.column.to_lowercase()))
        })
        .collect()
}

pub(super) fn end(session: &mut Session) {
    let Some(frame) = session.ext.catalog.frames.pop() else {
        return;
    };
    if let Some(watermark) = frame.watermark {
        // Text and clustering are catalog metadata of objects that exist;
        // nothing here can fail the batch.
        let _ = attach(&session.db, &frame, watermark);
    }
}

fn set_default_source(
    db: &duckdb::Connection,
    id: i32,
    source: &str,
    system_named: bool,
) -> Result<()> {
    db.execute(
        "DELETE FROM main.__msduck_default_sources WHERE object_id=?",
        [id],
    )?;
    db.execute(
        "INSERT INTO main.__msduck_default_sources VALUES(?,?,?)",
        duckdb::params![id, source, system_named],
    )?;
    Ok(())
}

fn set_computed_source(
    db: &duckdb::Connection,
    object_id: i32,
    column_id: i32,
    source: &str,
    not_null: bool,
) -> Result<()> {
    let nullability: HashMap<String, bool> = db
        .prepare("SELECT lower(name),is_nullable FROM main.__msduck_column_info WHERE object_id=?")?
        .query_map([object_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<duckdb::Result<_>>()?;
    let nullable = !not_null
        && msduck_sql::dialect::ext::catalog::definition::parse_expression(source).is_none_or(
            |expr| {
                declarations::computed_nullable(&expr, &|name| {
                    nullability
                        .get(&name.to_lowercase())
                        .copied()
                        .unwrap_or(true)
                })
            },
        );
    db.execute(
        "DELETE FROM main.__msduck_computed_sources WHERE object_id=? AND column_id=?",
        [object_id, column_id],
    )?;
    db.execute(
        "INSERT INTO main.__msduck_computed_sources VALUES(?,?,?,?)",
        duckdb::params![object_id, column_id, source, nullable],
    )?;
    Ok(())
}

/// The DEFAULT constraint of a column: its object ID.
fn default_of(db: &duckdb::Connection, object_id: i32, column_id: i32) -> Option<i32> {
    db.query_row(
        "SELECT object_id FROM main.__msduck_default_constraints WHERE parent_object_id=? AND column_id=?",
        [object_id, column_id],
        |row| row.get(0),
    )
    .ok()
}

fn attach(db: &duckdb::Connection, frame: &Frame, (objects, tags): (i64, i64)) -> Result<()> {
    let new = |id: i32| i64::from(id) > objects;
    for column in &frame.declarations.columns {
        let Some(table) = object_id(db, &column.table) else {
            continue;
        };
        if column.create && !new(table) {
            continue;
        }
        if !column.create
            && frame
                .existing
                .iter()
                .any(|(id, name)| *id == table && *name == column.column.to_lowercase())
        {
            continue;
        }
        let Some(column_id) = column_id(db, table, &column.column) else {
            continue;
        };
        if let Some(default) = &column.default
            && let Some(id) = default_of(db, table, column_id).filter(|id| new(*id))
        {
            set_default_source(db, id, &default.source, default.name.is_none())?;
        }
        if let Some(computed) = &column.computed {
            set_computed_source(db, table, column_id, computed, column.not_null)?;
        }
    }
    for default in &frame.declarations.defaults {
        let Some(table) = object_id(db, &default.table) else {
            continue;
        };
        let Some(column_id) = column_id(db, table, &default.column) else {
            continue;
        };
        if let Some(id) = default_of(db, table, column_id).filter(|id| new(*id)) {
            set_default_source(
                db,
                id,
                &default.default.source,
                default.default.name.is_none(),
            )?;
        }
    }
    // Named CHECK constraints: their expressions as written.
    for check in &frame.declarations.checks {
        let Some(table) = object_id(db, &check.table) else {
            continue;
        };
        let id: Option<i32> = db
            .query_row(
                "SELECT max(object_id) FROM main.__msduck_constraints WHERE parent_object_id=? AND type_code='C' AND lower(name)=lower(?)",
                duckdb::params![table, check.name],
                |row| row.get(0),
            )
            .ok()
            .flatten();
        if let Some(id) = id.filter(|id| new(*id)) {
            db.execute(
                "DELETE FROM main.__msduck_check_sources WHERE object_id=?",
                [id],
            )?;
            db.execute(
                "INSERT INTO main.__msduck_check_sources VALUES(?,?)",
                duckdb::params![id, check.source],
            )?;
        }
    }
    // Keys that ALTER TABLE added: their clustering and key order.
    for key in frame.keys.iter().filter(|key| !key.create) {
        let kind = match key.kind {
            KeyKind::Primary => "PK",
            KeyKind::Unique => "UQ",
            KeyKind::Index => continue,
        };
        let Some(table) = object_id(db, &key.table) else {
            continue;
        };
        let columns = serde_json::to_string(&key.columns)?;
        let tag: Option<i64> = db
            .query_row(
                "SELECT min(tag) FROM main.__msduck_keys WHERE object_id=? AND kind=? AND tag>?
                   AND lower(key_columns)=lower(?)
                   AND NOT EXISTS(SELECT 1 FROM main.__msduck_key_layout l WHERE l.tag=main.__msduck_keys.tag)",
                duckdb::params![table, kind, tags, columns],
                |row| row.get(0),
            )
            .ok()
            .flatten();
        if let Some(tag) = tag {
            record_layout(db, tag, key)?;
        }
    }
    Ok(())
}

/// Record a declared key's clustering and DESC columns.
pub(in crate::engine) fn record_layout(db: &duckdb::Connection, tag: i64, key: &Key) -> Result<()> {
    let descending: Vec<String> = key
        .descending
        .iter()
        .enumerate()
        .filter(|(_, descending)| **descending)
        .map(|(ordinal, _)| (ordinal + 1).to_string())
        .collect();
    db.execute("DELETE FROM main.__msduck_key_layout WHERE tag=?", [tag])?;
    db.execute(
        &format!(
            "INSERT INTO main.__msduck_key_layout VALUES(?,?,CAST([{}] AS INTEGER[]))",
            descending.join(",")
        ),
        duckdb::params![tag, key.clustered],
    )?;
    Ok(())
}

/// Keep a view's batch text as its definition once the statement succeeds.
pub(super) fn statement(
    session: &mut Session,
    statement: &mut Statement,
    parameters: &mut HashMap<String, Parameter>,
) -> Result<Option<Execution>> {
    let name = match statement {
        Statement::CreateView(view) => view.name.clone(),
        Statement::AlterView { name, .. } => name.clone(),
        _ => return Ok(None),
    };
    let Some(text) = session
        .ext
        .catalog
        .frames
        .last_mut()
        .and_then(|frame| frame.view.take())
    else {
        return Ok(None);
    };
    let statement = statement.clone();
    atomically(session, |session| {
        let execution = ext::reenter(session, "catalog", |session| {
            session.execute(statement, parameters)
        })?;
        let id: Option<i32> = session.db.query_row(
            "SELECT __msduck_object_id(?,'V')",
            [name.to_string()],
            |row| row.get(0),
        )?;
        if let Some(id) = id {
            session.db.execute(
                "DELETE FROM main.__msduck_view_sources WHERE object_id=?",
                [id],
            )?;
            session.db.execute(
                "INSERT INTO main.__msduck_view_sources VALUES(?,?)",
                duckdb::params![id, text],
            )?;
        }
        Ok(execution)
    })
    .map(Some)
}

/// Run `work` in the caller's transaction, or in one of its own.
pub(super) fn atomically<T>(
    session: &mut Session,
    work: impl FnOnce(&mut Session) -> Result<T>,
) -> Result<T> {
    if session.transactions > 0 {
        return work(session);
    }
    session.db.execute_batch("BEGIN TRANSACTION")?;
    session.transactions += 1;
    let result = work(session);
    session.transactions -= 1;
    match result {
        Ok(value) => match session.db.execute_batch("COMMIT") {
            Ok(()) => Ok(value),
            Err(error) => {
                let _ = session.db.execute_batch("ROLLBACK");
                Err(error.into())
            }
        },
        Err(error) => {
            let _ = session.db.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}
