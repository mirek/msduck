//! Firing DML triggers.
//!
//! A statement on a table with enabled triggers runs in a transaction (the
//! caller's, or one opened for the statement). Row images are captured as:
//!
//! - INSERT: the rows whose `rowid` exceeds the table's largest `rowid`
//!   before the statement (appended rows always get larger row ids);
//! - UPDATE and DELETE: the `rowid`s the statement selects, captured first by
//!   running its FROM and WHERE clauses through the engine, then the stored
//!   rows. An UPDATE of key columns rewrites rows with new row ids, which are
//!   again larger than every earlier one.
//!
//! INSTEAD OF triggers receive images without writing the table: INSERT runs
//! against an empty copy of the table (same declarations and defaults, no
//! constraints), UPDATE against a copy of the selected rows.
use super::super::{Partial, reenter};
use super::{
    Frame, IMAGE_PREFIX, Parameter, Session, Table, Trigger, any_triggers, quote, resolve,
    rollback_statement, tokens, triggers_on,
};
use crate::engine::{Execution, RpcExecution};
use anyhow::{Result, anyhow, bail};
use msduck_core::diagnostic::SqlError;
use msduck_sql::dialect::ext::triggers::{self as syntax, Event};
use sqlparser::ast::*;
use std::collections::HashMap;

/// Errors of the triggering statement itself are statement-level; errors in
/// a trigger body abort the batch.
enum Failure {
    Statement(anyhow::Error),
    Trigger { tokens: Vec<u8>, error: SqlError },
}

impl From<anyhow::Error> for Failure {
    fn from(error: anyhow::Error) -> Self {
        Failure::Statement(error)
    }
}

impl From<duckdb::Error> for Failure {
    fn from(error: duckdb::Error) -> Self {
        Failure::Statement(error.into())
    }
}

/// The DML statement inside an optional WITH.
fn dml(statement: &Statement) -> Option<(Option<&With>, &Statement)> {
    match statement {
        Statement::Insert(_) | Statement::Update(_) | Statement::Delete(_) => {
            Some((None, statement))
        }
        Statement::Query(query) => match query.body.as_ref() {
            SetExpr::Insert(inner) | SetExpr::Update(inner) | SetExpr::Delete(inner) => {
                Some((query.with.as_ref(), inner))
            }
            _ => None,
        },
        _ => None,
    }
}

fn dml_mut(statement: &mut Statement) -> Option<&mut Statement> {
    match statement {
        Statement::Insert(_) | Statement::Update(_) | Statement::Delete(_) => Some(statement),
        Statement::Query(query) => match query.body.as_mut() {
            SetExpr::Insert(inner) | SetExpr::Update(inner) | SetExpr::Delete(inner) => Some(inner),
            _ => None,
        },
        _ => None,
    }
}

fn idents(name: &ObjectName) -> Option<Vec<String>> {
    name.0
        .iter()
        .map(|part| part.as_ident().map(|ident| ident.value.clone()))
        .collect()
}

/// How a statement reaches its target rows: the table's name, and the FROM
/// list, WHERE clause and qualifier that select its rows.
struct Shape {
    event: Event,
    target: Vec<String>,
    qualifier: Ident,
    sources: Vec<TableWithJoins>,
    selection: Option<Expr>,
}

/// The relation in `sources` that a written target (an alias or a table
/// name) refers to, as a table name.
fn find_target(sources: &[TableWithJoins], written: &[String]) -> Option<Vec<String>> {
    let relations = sources.iter().flat_map(|source| {
        std::iter::once(&source.relation).chain(source.joins.iter().map(|join| &join.relation))
    });
    for relation in relations {
        let TableFactor::Table { name, alias, .. } = relation else {
            continue;
        };
        let Some(parts) = idents(name) else {
            continue;
        };
        let matches = match (alias, written) {
            (Some(alias), [single]) => alias.name.value.eq_ignore_ascii_case(single),
            (Some(_), _) => false,
            (None, _) => {
                parts.len() >= written.len()
                    && parts[parts.len() - written.len()..]
                        .iter()
                        .zip(written)
                        .all(|(a, b)| a.eq_ignore_ascii_case(b))
            }
        };
        if matches {
            return Some(parts);
        }
    }
    None
}

fn shape(statement: &Statement) -> Option<Shape> {
    match statement {
        Statement::Insert(insert) => {
            let TableObject::TableName(name) = &insert.table else {
                return None;
            };
            let target = idents(name)?;
            Some(Shape {
                event: Event::Insert,
                qualifier: Ident::new(target.last()?.clone()),
                target,
                sources: vec![],
                selection: None,
            })
        }
        Statement::Update(update) => {
            let TableFactor::Table { name, alias, .. } = &update.table.relation else {
                return None;
            };
            let written = idents(name)?;
            let qualifier = alias
                .as_ref()
                .map(|alias| alias.name.clone())
                .unwrap_or_else(|| Ident::new(written.last().unwrap().clone()));
            let (target, sources) = match &update.from {
                Some(
                    UpdateTableFromKind::AfterSet(sources)
                    | UpdateTableFromKind::BeforeSet(sources),
                ) => match find_target(sources, &written) {
                    Some(target) => (target, sources.clone()),
                    None => {
                        let mut all = vec![update.table.clone()];
                        all.extend(sources.iter().cloned());
                        (written, all)
                    }
                },
                None => (written, vec![update.table.clone()]),
            };
            Some(Shape {
                event: Event::Update,
                target,
                qualifier,
                sources,
                selection: update.selection.clone(),
            })
        }
        Statement::Delete(delete) => {
            let (FromTable::WithFromKeyword(targets) | FromTable::WithoutKeyword(targets)) =
                &delete.from;
            let [target] = targets.as_slice() else {
                return None;
            };
            let TableFactor::Table { name, alias, .. } = &target.relation else {
                return None;
            };
            let written = idents(name)?;
            let qualifier = alias
                .as_ref()
                .map(|alias| alias.name.clone())
                .unwrap_or_else(|| Ident::new(written.last().unwrap().clone()));
            let (table, sources) = match &delete.using {
                Some(sources) => match find_target(sources, &written) {
                    Some(table) => (table, sources.clone()),
                    None => {
                        let mut all = vec![target.clone()];
                        all.extend(sources.iter().cloned());
                        (written, all)
                    }
                },
                None => (written, vec![target.clone()]),
            };
            Some(Shape {
                event: Event::Delete,
                target: table,
                qualifier,
                sources,
                selection: delete.selection.clone(),
            })
        }
        _ => None,
    }
}

fn output_without_into(statement: &Statement) -> bool {
    let output = match statement {
        Statement::Insert(insert) => insert.output.as_ref(),
        Statement::Update(update) => update.output.as_ref(),
        Statement::Delete(delete) => delete.output.as_ref(),
        _ => None,
    };
    matches!(
        output,
        Some(OutputClause::Output {
            into_table: None,
            ..
        })
    )
}

/// INSERT, UPDATE or DELETE: fire the target table's enabled triggers.
pub(super) fn statement(
    session: &mut Session,
    statement: &mut Statement,
    parameters: &mut HashMap<String, Parameter>,
) -> Result<Option<Execution>> {
    if let Statement::Merge(merge) = statement {
        merge_guard(session, merge)?;
        return Ok(None);
    }
    let Some((_, inner)) = dml(statement) else {
        return Ok(None);
    };
    if session.transaction_doomed || !any_triggers(&session.db) {
        return Ok(None);
    }
    let Some(shape) = shape(inner) else {
        return Ok(None);
    };
    let Ok(Some(table)) = resolve(session, &shape.target) else {
        return Ok(None);
    };
    if table.kind != "U" {
        return Ok(None);
    }
    let enabled: Vec<Trigger> = triggers_on(&session.db, table.object_id)?
        .into_iter()
        .filter(|trigger| !trigger.disabled && trigger.events.contains(&shape.event))
        .collect();
    if enabled.is_empty() {
        return Ok(None);
    }
    if output_without_into(inner) {
        return Err(SqlError::new(
            334,
            1,
            format!(
                "The target table '{}' of the DML statement cannot have any enabled triggers if the statement contains an OUTPUT clause without INTO clause.",
                shape.target.join(".")
            ),
        )
        .into());
    }
    let frames = &session.ext.triggers.frames;
    // An INSTEAD OF trigger is not called again by statements it runs; a
    // trigger is not fired by its own statements (RECURSIVE_TRIGGERS OFF).
    let instead = enabled
        .iter()
        .find(|trigger| {
            trigger.instead_of
                && !frames
                    .iter()
                    .any(|frame| frame.trigger == trigger.object_id)
        })
        .cloned();
    let current = frames.last().map(|frame| frame.trigger);
    let after: Vec<Trigger> = if instead.is_some() {
        Vec::new()
    } else {
        enabled
            .into_iter()
            .filter(|trigger| !trigger.instead_of && Some(trigger.object_id) != current)
            .collect()
    };
    if instead.is_none() && after.is_empty() {
        return Ok(None);
    }
    if frames.len() >= 32 {
        return Err(SqlError::new(
            217,
            1,
            "Maximum stored procedure, function, trigger, or view nesting level exceeded (limit 32).",
        )
        .into());
    }
    run(
        session,
        statement.clone(),
        parameters,
        &table,
        &shape,
        instead,
        after,
    )
    .map(Some)
}

/// MERGE does not fire triggers yet; refuse it explicitly when an enabled
/// trigger on its target matches one of its actions.
fn merge_guard(session: &mut Session, merge: &Merge) -> Result<()> {
    let TableFactor::Table { name, .. } = &merge.table else {
        return Ok(());
    };
    let Some(parts) = idents(name) else {
        return Ok(());
    };
    if !any_triggers(&session.db) {
        return Ok(());
    }
    let Ok(Some(table)) = resolve(session, &parts) else {
        return Ok(());
    };
    let events: Vec<Event> = merge
        .clauses
        .iter()
        .filter_map(|clause| match clause.action {
            MergeAction::Insert(_) => Some(Event::Insert),
            MergeAction::Update(_) => Some(Event::Update),
            MergeAction::Delete { .. } => Some(Event::Delete),
            _ => None,
        })
        .collect();
    if let Some(trigger) = triggers_on(&session.db, table.object_id)?
        .into_iter()
        .find(|trigger| {
            !trigger.disabled && trigger.events.iter().any(|event| events.contains(event))
        })
    {
        bail!(
            "MERGE into '{}' is not supported while its trigger '{}' is enabled",
            parts.join("."),
            trigger.name
        );
    }
    Ok(())
}

/// The tables of one firing, dropped when it ends.
struct Images {
    inserted: String,
    deleted: String,
    keys: String,
    before: String,
}

impl Images {
    fn new(session: &mut Session, table: &Table) -> Self {
        let state = &mut session.ext.triggers;
        state.sequence += 1;
        let suffix = format!("{}_{}", session.ext.token, state.sequence);
        Images {
            inserted: format!("{IMAGE_PREFIX}{}_{suffix}_i", table.object_id),
            deleted: format!("{IMAGE_PREFIX}{}_{suffix}_d", table.object_id),
            keys: format!("__msduck_trigger_keys_{suffix}"),
            before: format!("__msduck_trigger_before_{suffix}"),
        }
    }

    fn drop(&self, db: &duckdb::Connection) {
        for name in [&self.inserted, &self.deleted] {
            let _ = db.execute_batch(&format!("DROP TABLE IF EXISTS main.{}", quote(name)));
        }
        for name in [&self.keys, &self.before] {
            let _ = db.execute_batch(&format!("DROP TABLE IF EXISTS temp.main.{}", quote(name)));
        }
    }
}

fn image(db: &duckdb::Connection, name: &str, query: &str) -> duckdb::Result<()> {
    db.execute_batch(&format!("CREATE TABLE main.{} AS {query}", quote(name)))
}

fn count(db: &duckdb::Connection, name: &str) -> duckdb::Result<u64> {
    db.query_row(
        &format!("SELECT count(*) FROM main.{}", quote(name)),
        [],
        |row| row.get::<_, i64>(0),
    )
    .map(|count| count as u64)
}

fn largest_rowid(db: &duckdb::Connection, table: &str) -> duckdb::Result<i64> {
    db.query_row(
        &format!("SELECT coalesce(max(rowid), -1) FROM {table}"),
        [],
        |row| row.get(0),
    )
}

fn run(
    session: &mut Session,
    statement: Statement,
    parameters: &mut HashMap<String, Parameter>,
    table: &Table,
    shape: &Shape,
    instead: Option<Trigger>,
    after: Vec<Trigger>,
) -> Result<Execution> {
    let outermost = session.ext.triggers.frames.is_empty();
    let autocommit = session.transactions == 0;
    if autocommit {
        session.db.execute_batch("BEGIN TRANSACTION")?;
        session.transactions = 1;
    }
    if outermost {
        session.ext.triggers.autocommit = autocommit;
    }
    let images = Images::new(session, table);
    let result = fire(
        session, statement, parameters, table, shape, instead, after, &images,
    );
    images.drop(&session.db);
    match result {
        Ok(execution) => {
            if autocommit && session.transactions == 1 {
                if let Err(error) = session.db.execute_batch("COMMIT") {
                    rollback_statement(session);
                    return Err(error.into());
                }
                session.transactions = 0;
                super::super::transaction_end(session, true);
            } else if autocommit && session.transactions > 1 {
                // BEGIN TRANSACTION in a trigger keeps the statement's
                // transaction open as the session's transaction.
                session.transactions -= 1;
            }
            Ok(execution)
        }
        Err(Failure::Statement(error)) => {
            if autocommit {
                rollback_statement(session);
            }
            Err(error)
        }
        Err(Failure::Trigger { tokens, error }) => {
            // An error in a trigger ends the batch and the transaction, as
            // with XACT_ABORT. Inside TRY the outermost statement leaves an
            // explicit transaction doomed for CATCH to roll back.
            if autocommit {
                rollback_statement(session);
            } else if outermost && session.transactions > 0 {
                session.transaction_doomed = true;
            }
            Err(Partial {
                tokens,
                error: error.into(),
            }
            .into())
        }
    }
}

const fn command(event: Event) -> u16 {
    match event {
        Event::Insert => 0xc3,
        Event::Update => 0xc5,
        Event::Delete => 0xc4,
    }
}

#[allow(clippy::too_many_arguments)]
fn fire(
    session: &mut Session,
    statement: Statement,
    parameters: &mut HashMap<String, Parameter>,
    table: &Table,
    shape: &Shape,
    instead: Option<Trigger>,
    after: Vec<Trigger>,
    images: &Images,
) -> std::result::Result<Execution, Failure> {
    let native = table.native();
    let columns = columns(&session.db, table.object_id)?;
    let updated = match shape.event {
        Event::Insert => None,
        Event::Delete => Some(Vec::new()),
        Event::Update => Some(assigned(&statement)),
    };
    let columns_updated = columns_updated(&columns, shape.event, updated.as_deref());
    let (execution, triggers) = match instead {
        None => {
            let execution = after_images(session, statement, parameters, &native, shape, images)?;
            (execution, after)
        }
        Some(trigger) => {
            let execution = instead_images(session, statement, parameters, table, shape, images)?;
            (execution, vec![trigger])
        }
    };
    let mut execution = execution;
    let count = execution.count.unwrap_or(0);
    for trigger in triggers {
        let frame = Frame {
            trigger: trigger.object_id,
            table: native.clone(),
            inserted: images.inserted.clone(),
            deleted: images.deleted.clone(),
            updated: updated.clone(),
            columns_updated: columns_updated.clone(),
        };
        match body(session, &trigger, frame, count) {
            Ok(tokens) => execution.tokens.extend(tokens),
            Err((tokens, error)) => {
                execution.tokens.extend(tokens);
                return Err(Failure::Trigger {
                    tokens: execution.tokens,
                    error,
                });
            }
        }
    }
    Ok(execution)
}

/// Run the statement, then capture the rows it wrote and removed.
fn after_images(
    session: &mut Session,
    statement: Statement,
    parameters: &mut HashMap<String, Parameter>,
    native: &str,
    shape: &Shape,
    images: &Images,
) -> std::result::Result<Execution, Failure> {
    let keys = format!("temp.main.{}", quote(&images.keys));
    let before = format!("temp.main.{}", quote(&images.before));
    if shape.event != Event::Insert {
        capture_keys(session, &statement, shape, parameters, images)?;
        session.db.execute_batch(&format!(
            "CREATE TEMP TABLE {} AS SELECT rowid AS __msduck_rid, * FROM {native} WHERE rowid IN (SELECT rid FROM {keys})",
            quote(&images.before)
        ))?;
    }
    let largest = largest_rowid(&session.db, native)?;
    let execution = reenter(session, "triggers", |session| {
        session.execute(statement, parameters)
    })?;
    let db = &session.db;
    match shape.event {
        Event::Insert => {
            image(
                db,
                &images.inserted,
                &format!("SELECT * FROM {native} WHERE rowid > {largest}"),
            )?;
            image(
                db,
                &images.deleted,
                &format!("SELECT * FROM {native} WHERE false"),
            )?;
        }
        Event::Update => {
            image(
                db,
                &images.inserted,
                &format!(
                    "SELECT * FROM {native} WHERE rowid IN (SELECT rid FROM {keys}) OR rowid > {largest}"
                ),
            )?;
            image(
                db,
                &images.deleted,
                &format!("SELECT * EXCLUDE (__msduck_rid) FROM {before}"),
            )?;
        }
        Event::Delete => {
            image(
                db,
                &images.inserted,
                &format!("SELECT * FROM {native} WHERE false"),
            )?;
            image(
                db,
                &images.deleted,
                &format!(
                    "SELECT * EXCLUDE (__msduck_rid) FROM {before} WHERE __msduck_rid NOT IN (SELECT rowid FROM {native})"
                ),
            )?;
        }
    }
    Ok(execution)
}

/// Capture the row ids an UPDATE or DELETE selects by running its FROM and
/// WHERE clauses (and WITH) through the engine.
fn capture_keys(
    session: &mut Session,
    statement: &Statement,
    shape: &Shape,
    parameters: &mut HashMap<String, Parameter>,
    images: &Images,
) -> Result<()> {
    session.db.execute_batch(&format!(
        "CREATE TEMP TABLE {} (rid BIGINT)",
        quote(&images.keys)
    ))?;
    let mut capture = msduck_sql::batch::parse(&format!(
        "INSERT INTO {} SELECT q.rowid FROM s",
        quote(&images.keys)
    ))?
    .remove(0);
    let Statement::Insert(insert) = &mut capture else {
        bail!("invalid trigger key capture");
    };
    let source = insert
        .source
        .as_mut()
        .ok_or_else(|| anyhow!("invalid trigger key capture"))?;
    source.with = dml(statement).and_then(|(with, _)| with.cloned());
    let SetExpr::Select(select) = source.body.as_mut() else {
        bail!("invalid trigger key capture");
    };
    select.projection = vec![SelectItem::UnnamedExpr(Expr::CompoundIdentifier(vec![
        shape.qualifier.clone(),
        Ident::new("rowid"),
    ]))];
    select.from = shape.sources.clone();
    select.selection = shape.selection.clone();
    reenter(session, "triggers", |session| {
        session.execute(capture, parameters)
    })?;
    Ok(())
}

/// The images an INSTEAD OF trigger receives, computed without writing the
/// table. Returns the statement's completion (its row count is the number of
/// rows the statement would have affected).
fn instead_images(
    session: &mut Session,
    mut statement: Statement,
    parameters: &mut HashMap<String, Parameter>,
    table: &Table,
    shape: &Shape,
    images: &Images,
) -> std::result::Result<Execution, Failure> {
    let native = table.native();
    let image_name = ObjectName::from(vec![
        Ident::new("main"),
        Ident::new(images.inserted.clone()),
    ]);
    match shape.event {
        Event::Insert => {
            let definition = copy_definition(&session.db, table, &images.inserted)?;
            session.db.execute_batch(&definition)?;
            image(
                &session.db,
                &images.deleted,
                &format!("SELECT * FROM {native} WHERE false"),
            )?;
            let Some(Statement::Insert(insert)) = dml_mut(&mut statement) else {
                return Err(anyhow!("expected INSERT").into());
            };
            insert.table = TableObject::TableName(image_name);
            insert.output = None;
            reenter(session, "triggers", |session| {
                session.execute(statement, parameters)
            })?;
        }
        Event::Update | Event::Delete => {
            capture_keys(session, &statement, shape, parameters, images)?;
            let keys = format!("temp.main.{}", quote(&images.keys));
            image(
                &session.db,
                &images.deleted,
                &format!("SELECT * FROM {native} WHERE rowid IN (SELECT rid FROM {keys})"),
            )?;
            if shape.event == Event::Delete {
                image(
                    &session.db,
                    &images.inserted,
                    &format!("SELECT * FROM {native} WHERE false"),
                )?;
            } else {
                image(
                    &session.db,
                    &images.inserted,
                    &format!("SELECT * FROM main.{}", quote(&images.deleted)),
                )?;
                let Some(Statement::Update(update)) = dml_mut(&mut statement) else {
                    return Err(anyhow!("expected UPDATE").into());
                };
                retarget_update(update, shape, image_name)?;
                reenter(session, "triggers", |session| {
                    session.execute(statement, parameters)
                })?;
            }
        }
    }
    let rows = match shape.event {
        Event::Delete => count(&session.db, &images.deleted)?,
        _ => count(&session.db, &images.inserted)?,
    };
    Ok(Execution::statement(
        vec![],
        Some(rows),
        command(shape.event),
    ))
}

/// Point an UPDATE at the copy of its selected rows: `UPDATE q SET ... FROM
/// main.<image> AS q ...`, where `q` is the alias or name the statement uses.
fn retarget_update(update: &mut Update, shape: &Shape, image: ObjectName) -> Result<()> {
    let alias = TableAlias {
        explicit: true,
        name: shape.qualifier.clone(),
        columns: vec![],
        at: None,
    };
    let TableFactor::Table { name: written, .. } = &update.table.relation else {
        bail!("unsupported UPDATE target");
    };
    let written = idents(written).ok_or_else(|| anyhow!("unsupported UPDATE target"))?;
    let replacement = TableWithJoins {
        relation: TableFactor::Table {
            name: image,
            alias: Some(alias),
            args: None,
            with_hints: vec![],
            version: None,
            with_ordinality: false,
            partitions: vec![],
            json_path: None,
            sample: None,
            index_hints: vec![],
        },
        joins: vec![],
    };
    let mut replaced = false;
    if let Some(UpdateTableFromKind::AfterSet(sources) | UpdateTableFromKind::BeforeSet(sources)) =
        &mut update.from
    {
        for source in sources.iter_mut() {
            for relation in std::iter::once(&mut source.relation)
                .chain(source.joins.iter_mut().map(|join| &mut join.relation))
            {
                if replaced {
                    break;
                }
                if let TableFactor::Table { name, alias, .. } = relation
                    && let Some(parts) = idents(name)
                {
                    let matches = match (alias.as_ref(), written.as_slice()) {
                        (Some(alias), [single]) => alias.name.value.eq_ignore_ascii_case(single),
                        (Some(_), _) => false,
                        (None, _) => parts
                            .last()
                            .zip(written.last())
                            .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b)),
                    };
                    if matches {
                        *relation = replacement.relation.clone();
                        replaced = true;
                    }
                }
            }
        }
        if !replaced {
            sources.insert(0, replacement);
        }
    } else {
        update.from = Some(UpdateTableFromKind::AfterSet(vec![replacement]));
    }
    update.table.relation = TableFactor::Table {
        name: ObjectName::from(vec![shape.qualifier.clone()]),
        alias: None,
        args: None,
        with_hints: vec![],
        version: None,
        with_ordinality: false,
        partitions: vec![],
        json_path: None,
        sample: None,
        index_hints: vec![],
    };
    update.output = None;
    Ok(())
}

/// The table's DuckDB definition under another name, without constraints
/// and with identity columns defaulting to 0 (as INSTEAD OF INSERT reports
/// them).
fn copy_definition(db: &duckdb::Connection, table: &Table, image: &str) -> Result<String> {
    let sql: String = db.query_row(
        "SELECT sql FROM duckdb_tables() WHERE database_name = current_database() AND schema_name = ? AND table_name = ?",
        [&table.schema, &table.name],
        |row| row.get(0),
    )?;
    let mut statements =
        sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::DuckDbDialect {}, &sql)?;
    let [Statement::CreateTable(create)] = statements.as_mut_slice() else {
        bail!("unexpected table definition for {}", table.native());
    };
    create.name = ObjectName::from(vec![Ident::new("main"), Ident::with_quote('"', image)]);
    create.constraints.clear();
    for column in &mut create.columns {
        column.options.retain(|option| {
            !matches!(
                option.option,
                ColumnOption::NotNull
                    | ColumnOption::PrimaryKey(_)
                    | ColumnOption::Unique(_)
                    | ColumnOption::ForeignKey(_)
                    | ColumnOption::Check(_)
            )
        });
        for option in &mut column.options {
            if let ColumnOption::Default(expr) = &mut option.option
                && expr.to_string().to_ascii_lowercase().contains("nextval(")
            {
                *expr = Expr::Value(Value::Number("0".into(), false).into());
            }
        }
    }
    Ok(statements[0].to_string())
}

/// Column names and ids, in column order.
fn columns(db: &duckdb::Connection, object_id: i32) -> Result<Vec<(String, i32)>> {
    let mut statement = db.prepare(
        "SELECT name, column_id FROM sys.columns WHERE object_id = ? ORDER BY column_id",
    )?;
    Ok(statement
        .query_map([object_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<duckdb::Result<_>>()?)
}

/// The columns an UPDATE assigns, in lower case.
fn assigned(statement: &Statement) -> Vec<String> {
    let Some((_, Statement::Update(update))) = dml(statement) else {
        return Vec::new();
    };
    update
        .assignments
        .iter()
        .flat_map(|assignment| match &assignment.target {
            AssignmentTarget::ColumnName(name) => vec![name],
            AssignmentTarget::Tuple(names) => names.iter().collect(),
        })
        .filter_map(|name| name.0.last().and_then(|part| part.as_ident()))
        .filter(|ident| !ident.value.starts_with('@'))
        .map(|ident| ident.value.to_lowercase())
        .collect()
}

/// COLUMNS_UPDATED(): one bit per column id, least significant bit first;
/// empty for DELETE.
fn columns_updated(columns: &[(String, i32)], event: Event, updated: Option<&[String]>) -> Vec<u8> {
    if event == Event::Delete {
        return Vec::new();
    }
    let width = columns.iter().map(|(_, id)| *id).max().unwrap_or(0).max(0) as usize;
    let mut mask = vec![0u8; width.div_ceil(8)];
    for (name, id) in columns {
        let set = updated.is_none_or(|updated| updated.contains(&name.to_lowercase()));
        if set && *id > 0 {
            let bit = (*id - 1) as usize;
            mask[bit / 8] |= 1 << (bit % 8);
        }
    }
    mask
}

/// Run one trigger body for a firing. Returns its output, or its output and
/// the error that ended it (including 3609 when the body ended the
/// transaction).
fn body(
    session: &mut Session,
    trigger: &Trigger,
    frame: Frame,
    count: u64,
) -> std::result::Result<Vec<u8>, (Vec<u8>, SqlError)> {
    let mut body = match syntax::definition(&trigger.definition) {
        Some(Ok(definition)) => trigger.definition[definition.body..].to_string(),
        _ => {
            return Err((
                Vec::new(),
                SqlError::new(
                    50000,
                    1,
                    format!(
                        "The definition of trigger '{}' cannot be read.",
                        trigger.name
                    ),
                ),
            ));
        }
    };
    // The body's own statements run one nesting level below the statement
    // that fired it.
    let level = (session.ext.triggers.frames.len() + 1).to_string();
    for range in syntax::nest_level_references(&body).into_iter().rev() {
        body.replace_range(range, &level);
    }
    for (range, column) in syntax::update_tests(&body).into_iter().rev() {
        let updated = frame.updated.as_ref().is_none_or(|columns| {
            columns
                .iter()
                .any(|name| name.eq_ignore_ascii_case(&column))
        });
        body.replace_range(range, if updated { "(1 = 1)" } else { "(1 = 0)" });
    }
    session.ext.triggers.frames.push(frame);
    let caught = session.caught_error.take();
    session.rowcount = count;
    let entry = session.transactions;
    session.transactions = entry + 1;
    let (out, _) =
        session.batch_response_context(&body, &HashMap::new(), Some(RpcExecution::Direct), None);
    session.ext.triggers.frames.pop();
    session.caught_error = caught;
    let now = session.transactions;
    match tokens::finish(out) {
        tokens::End::Aborted { tokens, error } => {
            if now > 0 {
                session.transactions = entry;
            }
            Err((tokens, error))
        }
        tokens::End::Completed(tokens) => {
            if now == 0 {
                return Err((tokens, ended_in_trigger()));
            }
            if now <= entry {
                // COMMIT in a trigger commits the transaction and ends the batch.
                let _ = session.db.execute_batch("COMMIT");
                session.transactions = 0;
                super::super::transaction_end(session, true);
                return Err((tokens, ended_in_trigger()));
            }
            session.transactions = now - 1;
            Ok(tokens)
        }
    }
}

fn ended_in_trigger() -> SqlError {
    SqlError::new(
        3609,
        1,
        "The transaction ended in the trigger. The batch has been aborted.",
    )
}

/// After a rollback inside a trigger body the images are gone; SQL Server's
/// `inserted` and `deleted` are then empty, with their columns intact.
pub(super) fn restore_images(session: &mut Session) {
    for frame in &session.ext.triggers.frames {
        for image in [&frame.inserted, &frame.deleted] {
            let _ = session.db.execute_batch(&format!(
                "CREATE TABLE IF NOT EXISTS main.{} AS SELECT * FROM {} WHERE false",
                quote(image),
                frame.table
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape_of(sql: &str) -> Shape {
        let statement = msduck_sql::batch::parse(sql).unwrap().remove(0);
        let (_, inner) = dml(&statement).unwrap();
        shape(inner).unwrap()
    }

    #[test]
    fn shapes_find_the_target_table_behind_aliases() {
        for (sql, event, target, qualifier, sources) in [
            ("INSERT dbo.t(a) VALUES (1)", Event::Insert, "dbo.t", "t", 0),
            ("UPDATE t SET a = 1 WHERE b = 2", Event::Update, "t", "t", 1),
            (
                "UPDATE x SET a = s.a FROM dbo.t AS x JOIN s ON s.id = x.id",
                Event::Update,
                "dbo.t",
                "x",
                1,
            ),
            (
                "UPDATE t SET a = s.a FROM s WHERE s.id = t.id",
                Event::Update,
                "t",
                "t",
                2,
            ),
            ("DELETE FROM t WHERE a = 1", Event::Delete, "t", "t", 1),
            (
                "DELETE x FROM t x JOIN s ON s.id = x.id",
                Event::Delete,
                "t",
                "x",
                1,
            ),
            (
                "WITH c AS (SELECT 1 AS id) DELETE t FROM t JOIN c ON c.id = t.id",
                Event::Delete,
                "t",
                "t",
                1,
            ),
        ] {
            let shape = shape_of(sql);
            assert_eq!(shape.event, event, "{sql}");
            assert_eq!(shape.target.join("."), target, "{sql}");
            assert_eq!(shape.qualifier.value, qualifier, "{sql}");
            assert_eq!(shape.sources.len(), sources, "{sql}");
        }
    }

    #[test]
    fn columns_updated_sets_one_bit_per_column_id() {
        let columns: Vec<(String, i32)> = (1..=10).map(|id| (format!("c{id}"), id)).collect();
        assert_eq!(columns_updated(&columns, Event::Insert, None), [0xff, 0x03]);
        assert_eq!(
            columns_updated(&columns, Event::Update, Some(&["c2".into(), "c9".into()])),
            [0x02, 0x01]
        );
        assert!(columns_updated(&columns, Event::Delete, Some(&[])).is_empty());
        assert_eq!(
            columns_updated(&columns[..3], Event::Update, Some(&[])),
            [0]
        );
    }
}
