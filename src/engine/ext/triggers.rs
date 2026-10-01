//! DML triggers and DISABLE/ENABLE TRIGGER.
//!
//! - `ddl` stores definitions in the module store (type `TR`) and handles
//!   DROP, DISABLE/ENABLE and the triggers of dropped tables.
//! - `fire` intercepts INSERT, UPDATE and DELETE on tables with enabled
//!   triggers, captures the `inserted` and `deleted` row images and runs the
//!   trigger bodies through the ordinary batch engine.
//!
//! Row images are tables in the database's `main` schema named
//! `__msduck_trigger_<table id>_<session>_<sequence>_{i,d}`. Catalog binding
//! resolves those names to the triggering table (see [`bootstrap`]), so
//! trigger bodies see the table's declared column types. See
//! docs/gaps-triggers.md.
use super::{Execution, Feature, Parameter, Session};
use anyhow::{Result, bail};
use duckdb::OptionalExt;
use msduck_sql::dialect::ext::triggers::{self as syntax, Event};
use sqlparser::ast::{
    BinaryOperator, Expr, Function, FunctionArg, FunctionArgExpr, FunctionArguments, Ident,
    ObjectName, Statement, TableAlias, TableFactor, Value, VisitMut, VisitorMut,
};
use std::{collections::HashMap, ops::ControlFlow};

mod ddl;
mod fire;
mod tokens;

/// The image tables of one firing, and what the trigger functions report.
#[derive(Clone, Debug)]
struct Frame {
    trigger: i32,
    /// The triggering table, as a native DuckDB name.
    table: String,
    inserted: String,
    deleted: String,
    /// Columns UPDATE() reports as updated (lower case); `None` for all.
    updated: Option<Vec<String>>,
    columns_updated: Vec<u8>,
}

/// Per-session trigger state: the stack of executing trigger bodies.
#[derive(Default)]
pub(crate) struct State {
    frames: Vec<Frame>,
    sequence: u64,
    /// Whether the outermost firing runs in a transaction opened for its
    /// statement (the session was in autocommit mode).
    autocommit: bool,
}

impl State {
    /// How many trigger bodies are executing (TRIGGER_NESTLEVEL()).
    #[allow(dead_code)] // For features that scope values to trigger bodies.
    pub(crate) fn depth(&self) -> usize {
        self.frames.len()
    }
}

pub(super) struct Hooks;

impl Feature for Hooks {
    fn name(&self) -> &'static str {
        "triggers"
    }

    fn batch(
        &self,
        session: &mut Session,
        sql: &str,
        _parameters: &HashMap<String, Parameter>,
        rpc: bool,
    ) -> Option<(Vec<u8>, bool)> {
        ddl::batch(session, sql, rpc)
    }

    fn statement(
        &self,
        session: &mut Session,
        statement: &mut Statement,
        parameters: &mut HashMap<String, Parameter>,
    ) -> Result<Option<Execution>> {
        if session.ext.triggers.frames.is_empty() {
            return dispatch(session, statement, parameters);
        }
        // Inside a trigger body every runtime error ends the batch (SQL
        // Server runs trigger bodies as with XACT_ABORT), so the engine's
        // statement-level recovery must not continue the body.
        if session.ext.triggers.autocommit
            && session.transactions > 0
            && matches!(
                statement,
                Statement::Rollback {
                    savepoint: None,
                    ..
                }
            )
        {
            // The statement's own transaction: no transaction ENVCHANGE.
            rollback_statement(session);
            return Ok(Some(Execution::statement(vec![], None, 0)));
        }
        let result = match dispatch(session, statement, parameters) {
            Ok(Some(execution)) => Ok(execution),
            Ok(None) => {
                let statement = statement.clone();
                super::reenter(session, "triggers", |session| {
                    session.execute(statement, parameters)
                })
            }
            Err(error) => Err(error),
        };
        result.map(Some).map_err(abort)
    }

    fn rewrite_statement(
        &self,
        session: &Session,
        statement: &mut Statement,
        _parameters: &HashMap<String, Parameter>,
    ) -> Result<()> {
        if let Some(frame) = session.ext.triggers.frames.last() {
            let _ = VisitMut::visit(statement, &mut Images(frame));
        }
        Ok(())
    }

    fn rewrite_expr(
        &self,
        session: &Session,
        expr: &mut Expr,
        _parameters: &HashMap<String, Parameter>,
    ) -> Result<()> {
        let frames = &session.ext.triggers.frames;
        let Some(frame) = frames.last() else {
            return Ok(());
        };
        if matches!(
            expr,
            Expr::Subquery(_) | Expr::Exists { .. } | Expr::InSubquery { .. }
        ) {
            let _ = VisitMut::visit(expr, &mut Images(frame));
            return Ok(());
        }
        let Expr::Function(function) = expr else {
            return Ok(());
        };
        let name = function.name.to_string().to_ascii_uppercase();
        let arguments = arguments(function);
        match (name.as_str(), arguments.as_deref()) {
            ("UPDATE", Some([column])) => {
                let column = match column {
                    Expr::Identifier(ident) => ident.value.to_lowercase(),
                    Expr::CompoundIdentifier(parts) if !parts.is_empty() => {
                        parts.last().unwrap().value.to_lowercase()
                    }
                    _ => bail!("UPDATE() requires a column name"),
                };
                let updated = frame
                    .updated
                    .as_ref()
                    .is_none_or(|columns| columns.contains(&column));
                *expr = predicate(updated);
            }
            ("COLUMNS_UPDATED", Some([])) => {
                *expr = syntax::binary_literal(&frame.columns_updated);
            }
            ("TRIGGER_NESTLEVEL", Some([])) => {
                *expr = number(frames.len() as i64);
            }
            ("TRIGGER_NESTLEVEL", Some([object, ..])) => {
                // Levels at which the given trigger is executing.
                let mut counts: Vec<(i32, i64)> = Vec::new();
                for frame in frames {
                    match counts.iter_mut().find(|(id, _)| *id == frame.trigger) {
                        Some((_, count)) => *count += 1,
                        None => counts.push((frame.trigger, 1)),
                    }
                }
                *expr = Expr::Case {
                    case_token: sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
                    end_token: sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
                    operand: Some(Box::new(object.clone())),
                    conditions: counts
                        .into_iter()
                        .map(|(id, count)| sqlparser::ast::CaseWhen {
                            condition: number(id.into()),
                            result: number(count),
                        })
                        .collect(),
                    else_result: Some(Box::new(number(0))),
                };
            }
            _ => {}
        }
        Ok(())
    }

    fn bootstrap_database(&self, db: &duckdb::Connection) -> Result<()> {
        bootstrap(db)
    }

    fn transaction_end(&self, session: &mut Session, committed: bool) {
        if !committed {
            fire::restore_images(session);
        }
    }
}

fn dispatch(
    session: &mut Session,
    statement: &mut Statement,
    parameters: &mut HashMap<String, Parameter>,
) -> Result<Option<Execution>> {
    if let Some(command) = syntax::Command::of(statement) {
        return ddl::command(session, command).map(Some);
    }
    if let Some(execution) = ddl::drop_table(session, statement, parameters)? {
        return Ok(Some(execution));
    }
    fire::statement(session, statement, parameters)
}

/// Make a statement-level error batch-aborting: the engine continues after
/// failed DML and arithmetic errors, which a trigger body must not.
fn abort(error: anyhow::Error) -> anyhow::Error {
    let failed = error.downcast_ref::<crate::query_error::FailedQuery>();
    if failed.is_none() && error.downcast_ref::<crate::output_sink::Failed>().is_none() {
        return error;
    }
    let tokens = failed
        .map(|failed| failed.metadata.clone())
        .unwrap_or_default();
    let diagnostic = error
        .downcast_ref::<msduck_core::diagnostic::SqlError>()
        .cloned()
        .unwrap_or_else(|| super::super::sql_error_from_message(&error.to_string()));
    super::Partial {
        tokens,
        error: diagnostic.into(),
    }
    .into()
}

fn arguments(function: &Function) -> Option<Vec<Expr>> {
    match &function.args {
        FunctionArguments::List(list) => list
            .args
            .iter()
            .map(|argument| match argument {
                FunctionArg::Unnamed(FunctionArgExpr::Expr(expr)) => Some(expr.clone()),
                _ => None,
            })
            .collect(),
        FunctionArguments::None => Some(Vec::new()),
        FunctionArguments::Subquery(_) => None,
    }
}

fn number(value: i64) -> Expr {
    Expr::Value(Value::Number(value.to_string(), false).into())
}

/// `(1 = 1)` or `(1 = 0)`: a predicate the engine evaluates like any other.
fn predicate(value: bool) -> Expr {
    Expr::Nested(Box::new(Expr::BinaryOp {
        left: Box::new(number(1)),
        op: BinaryOperator::Eq,
        right: Box::new(number(i64::from(value))),
    }))
}

/// Point `inserted` and `deleted` at the current firing's image tables,
/// keeping the name as the alias so qualified column references still bind.
struct Images<'a>(&'a Frame);

impl VisitorMut for Images<'_> {
    type Break = ();
    fn pre_visit_table_factor(&mut self, factor: &mut TableFactor) -> ControlFlow<()> {
        if let TableFactor::Table {
            name, alias, args, ..
        } = factor
            && args.is_none()
            && let [part] = name.0.as_slice()
            && let Some(ident) = part.as_ident()
        {
            let image = if ident.value.eq_ignore_ascii_case("inserted") {
                &self.0.inserted
            } else if ident.value.eq_ignore_ascii_case("deleted") {
                &self.0.deleted
            } else {
                return ControlFlow::Continue(());
            };
            if alias.is_none() {
                *alias = Some(TableAlias {
                    explicit: true,
                    name: Ident::new(ident.value.clone()),
                    columns: vec![],
                    at: None,
                });
            }
            *name = ObjectName::from(vec![Ident::new("main"), Ident::new(image)]);
        }
        ControlFlow::Continue(())
    }
}

/// Image tables bind with the declarations of their triggering table.
/// `__msduck_object_id` (from the object catalog) looks a name's key up in a
/// map built from `sys.all_objects`. Extend that map with `#<id>` keys for
/// user tables, and rewrite the key of an image name to `#<table id>` (the
/// id is part of the name). The argument is still evaluated once.
fn bootstrap(db: &duckdb::Connection) -> Result<()> {
    let definition: Option<String> = db
        .query_row(
            "SELECT macro_definition FROM duckdb_functions() WHERE database_name = current_database() AND schema_name = 'main' AND function_name = '__msduck_object_id' LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let Some(definition) = definition else {
        return Ok(());
    };
    if definition.contains(IMAGE_PREFIX) {
        return Ok(());
    }
    let extended = extend_object_id(&definition).ok_or_else(|| {
        anyhow::anyhow!("unexpected __msduck_object_id definition; trigger images cannot bind")
    })?;
    db.execute_batch(&format!(
        "CREATE OR REPLACE MACRO main.__msduck_object_id(value, kind) AS {extended}"
    ))?;
    Ok(())
}

/// `map_extract_value((SELECT map(...) FROM (inner)), key)` becomes
/// `map_extract_value((SELECT map(...) FROM (inner UNION ALL ids)), image(key))`.
fn extend_object_id(definition: &str) -> Option<String> {
    let arguments = definition
        .strip_prefix("map_extract_value(")?
        .strip_suffix(')')?;
    let split = top_level(arguments, |text| text.starts_with(','))?;
    let (map, key) = (arguments[..split].trim(), arguments[split + 1..].trim());
    // The map query's FROM (...) at depth one.
    let from = top_level(map.strip_prefix('(')?, |text| {
        text.len() >= 6 && text[..6].eq_ignore_ascii_case("FROM (")
    })? + 1;
    let open = from + 5;
    let close = open + matching(&map[open..])?;
    let ids = "SELECT '#' || CAST(o.object_id AS VARCHAR) || chr(0) || k.kind, o.object_id FROM sys.objects AS o CROSS JOIN (VALUES (''), ('U')) AS k(kind) WHERE rtrim(o.type) = 'U'";
    Some(format!(
        "map_extract_value({} UNION ALL {ids}{}, regexp_replace({key}, '^main\\x00{IMAGE_PREFIX}([0-9]+)_[0-9]+_[0-9]+_[id](\\x00U?)$', '#\\1\\2'))",
        &map[..close],
        &map[close..]
    ))
}

/// Byte offset of the first position outside parentheses, quotes and
/// strings where `at` matches.
fn top_level(text: &str, at: impl Fn(&str) -> bool) -> Option<usize> {
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    for (index, character) in text.char_indices() {
        match (quote, character) {
            (Some(open), _) if character == open => quote = None,
            (Some(_), _) => {}
            (None, '\'' | '"') => quote = Some(character),
            (None, '(') => depth += 1,
            (None, ')') => depth = depth.checked_sub(1)?,
            _ if depth == 0 && at(&text[index..]) => return Some(index),
            _ => {}
        }
    }
    None
}

/// Byte offset of the parenthesis closing the one `text` starts with.
fn matching(text: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    for (index, character) in text.char_indices() {
        match (quote, character) {
            (Some(open), _) if character == open => quote = None,
            (Some(_), _) => {}
            (None, '\'' | '"') => quote = Some(character),
            (None, '(') => depth += 1,
            (None, ')') => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

const IMAGE_PREFIX: &str = "__msduck_trigger_";

/// A table or view resolved in the current database.
#[derive(Clone, Debug)]
struct Table {
    object_id: i32,
    schema: String,
    name: String,
    kind: String,
}

impl Table {
    fn native(&self) -> String {
        format!("{}.{}", quote(&self.schema), quote(&self.name))
    }
}

fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn bracket(name: &str) -> String {
    format!("[{}]", name.replace(']', "]]"))
}

/// Resolve a one- to three-part name in the current database. A database
/// part naming another database is an explicit unsupported error.
fn resolve(session: &Session, parts: &[String]) -> Result<Option<Table>> {
    let parts = match parts {
        [database, rest @ ..] if parts.len() == 3 => {
            let catalog = session.database.catalog();
            match catalog.resolve(&session.db, database)? {
                Some(alias) if alias == session.database.alias() => rest,
                Some(_) => bail!(
                    "unsupported trigger reference to {} in another database; USE {database} first",
                    parts.join(".")
                ),
                None => return Ok(None),
            }
        }
        [_] | [_, _] => parts,
        _ => return Ok(None),
    };
    let name = parts
        .iter()
        .map(|part| bracket(part))
        .collect::<Vec<_>>()
        .join(".");
    Ok(session
        .db
        .query_row(
            "SELECT o.object_id, s.name, o.name, rtrim(o.type) FROM sys.objects o JOIN sys.schemas s ON s.schema_id = o.schema_id WHERE o.object_id = __msduck_object_id(?, NULL)",
            [name],
            |row| {
                Ok(Table {
                    object_id: row.get(0)?,
                    schema: row.get(1)?,
                    name: row.get(2)?,
                    kind: row.get(3)?,
                })
            },
        )
        .optional()?)
}

/// A stored trigger.
#[derive(Clone, Debug)]
struct Trigger {
    object_id: i32,
    schema: String,
    name: String,
    parent: i32,
    definition: String,
    disabled: bool,
    events: Vec<Event>,
    instead_of: bool,
}

/// The module store's properties for a trigger.
fn properties(events: &[Event], instead_of: bool) -> String {
    let events = Event::ALL
        .into_iter()
        .filter(|event| events.contains(event))
        .map(|event| serde_json::Value::from(event.name()))
        .collect::<Vec<_>>();
    serde_json::json!({ "events": events, "instead_of": instead_of }).to_string()
}

const TRIGGER_COLUMNS: &str = "SELECT m.object_id, s.name, m.name, m.parent_object_id, m.definition, m.is_disabled, m.properties FROM main.__msduck_modules m JOIN main.__msduck_schemas s USING (schema_id) WHERE m.type_code = 'TR'";

fn trigger_row(row: &duckdb::Row<'_>) -> duckdb::Result<Trigger> {
    let properties: String = row.get(6)?;
    let properties: serde_json::Value =
        serde_json::from_str(&properties).unwrap_or(serde_json::Value::Null);
    Ok(Trigger {
        object_id: row.get(0)?,
        schema: row.get(1)?,
        name: row.get(2)?,
        parent: row.get(3)?,
        definition: row.get(4)?,
        disabled: row.get(5)?,
        events: properties["events"]
            .as_array()
            .map(|events| {
                events
                    .iter()
                    .filter_map(|event| event.as_str().and_then(Event::parse))
                    .collect()
            })
            .unwrap_or_default(),
        instead_of: properties["instead_of"].as_bool().unwrap_or(false),
    })
}

/// Every trigger on a table, in creation order (SQL Server's default firing
/// order without sp_settriggerorder).
fn triggers_on(db: &duckdb::Connection, parent: i32) -> Result<Vec<Trigger>> {
    let mut statement = db.prepare(&format!(
        "{TRIGGER_COLUMNS} AND m.parent_object_id = ? ORDER BY m.object_id"
    ))?;
    Ok(statement
        .query_map([parent], trigger_row)?
        .collect::<duckdb::Result<_>>()?)
}

/// A trigger by schema (the default schema when `None`) and name.
fn find_trigger(
    db: &duckdb::Connection,
    schema: Option<&str>,
    name: &str,
) -> Result<Option<Trigger>> {
    Ok(db
        .query_row(
            &format!("{TRIGGER_COLUMNS} AND lower(s.name) = lower(?) AND lower(m.name) = lower(?)"),
            [schema.unwrap_or("dbo"), name],
            trigger_row,
        )
        .optional()?)
}

/// Run `work` in the caller's transaction, or in a statement transaction
/// opened for the call when the session is in autocommit mode.
fn atomically<T>(session: &mut Session, work: impl FnOnce(&mut Session) -> Result<T>) -> Result<T> {
    if session.transactions > 0 {
        return work(session);
    }
    session.db.execute_batch("BEGIN TRANSACTION")?;
    session.transactions = 1;
    match work(session) {
        Ok(value) if session.transactions == 1 => {
            if let Err(error) = session.db.execute_batch("COMMIT") {
                rollback_statement(session);
                return Err(error.into());
            }
            session.transactions = 0;
            super::transaction_end(session, true);
            Ok(value)
        }
        Ok(value) => Ok(value),
        Err(error) => {
            rollback_statement(session);
            Err(error)
        }
    }
}

/// Roll back a statement transaction opened by this module.
fn rollback_statement(session: &mut Session) {
    if session.transactions > 0 {
        let _ = session.db.execute_batch("ROLLBACK");
        session.transactions = 0;
        session.transaction_doomed = false;
        super::transaction_end(session, false);
    }
}

/// Whether the current database has any triggers at all; the fast path for
/// every DML statement.
/// A database without a readable module store (or an aborted native
/// transaction) has no triggers to fire; the engine reports such failures.
fn any_triggers(db: &duckdb::Connection) -> bool {
    db.query_row(
        "SELECT EXISTS (SELECT 1 FROM main.__msduck_modules WHERE type_code = 'TR')",
        [],
        |row| row.get(0),
    )
    .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame() -> Frame {
        Frame {
            trigger: 7,
            table: "\"dbo\".\"t\"".into(),
            inserted: format!("{IMAGE_PREFIX}5_1_1_i"),
            deleted: format!("{IMAGE_PREFIX}5_1_1_d"),
            updated: None,
            columns_updated: vec![],
        }
    }

    #[test]
    fn images_replace_inserted_and_deleted_keeping_their_names_as_aliases() {
        let mut statement = msduck_sql::batch::parse(
            "SELECT i.id, deleted.id FROM inserted i JOIN [DELETED] ON 1 = 1 WHERE EXISTS (SELECT 1 FROM Inserted) AND x IN (SELECT id FROM dbo.inserted)",
        )
        .unwrap()
        .remove(0);
        let _ = VisitMut::visit(&mut statement, &mut Images(&frame()));
        assert_eq!(
            statement.to_string(),
            format!(
                "SELECT i.id, deleted.id FROM main.{IMAGE_PREFIX}5_1_1_i i JOIN main.{IMAGE_PREFIX}5_1_1_d AS DELETED ON 1 = 1 WHERE EXISTS (SELECT 1 FROM main.{IMAGE_PREFIX}5_1_1_i AS Inserted) AND x IN (SELECT id FROM dbo.inserted)"
            )
        );
    }

    #[test]
    fn image_names_bind_to_their_table_and_other_names_are_unchanged() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        session
            .db
            .execute_batch("CREATE TABLE dbo.t(id INT)")
            .unwrap();
        crate::object_catalog::sync(&session.db).unwrap();
        // Bootstrapping again keeps a single extension.
        bootstrap(&session.db).unwrap();
        let id = |name: &str, kind: Option<&str>| -> Option<i32> {
            session
                .db
                .query_row(
                    "SELECT __msduck_object_id(?, ?)",
                    duckdb::params![name, kind],
                    |row| row.get(0),
                )
                .unwrap()
        };
        let table = id("dbo.t", None).expect("table id");
        assert_eq!(id("t", Some("U")), Some(table));
        assert_eq!(id("nosuch", None), None);
        session
            .db
            .execute_batch(&format!(
                "CREATE TABLE main.{IMAGE_PREFIX}{table}_3_9_i(id INT)"
            ))
            .unwrap();
        assert_eq!(
            id(&format!("main.{IMAGE_PREFIX}{table}_3_9_i"), None),
            Some(table)
        );
        assert_eq!(
            id(&format!("main.[{IMAGE_PREFIX}{table}_3_9_i]"), Some("U")),
            Some(table)
        );
        assert_eq!(
            id(&format!("main.{IMAGE_PREFIX}{table}_3_9_i"), Some("V")),
            None
        );
        // Image names map to their table whether or not the image exists.
        assert_eq!(
            id(&format!("main.{IMAGE_PREFIX}{table}_3_9_d"), None),
            Some(table)
        );
        assert_eq!(id(&format!("main.{IMAGE_PREFIX}999_3_9_d"), None), None);
    }
}
