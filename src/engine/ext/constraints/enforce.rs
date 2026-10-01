//! Enforce CHECK and FOREIGN KEY constraints around INSERT, UPDATE, DELETE
//! and MERGE, and perform referential actions.
//!
//! Every statement that touches a table with constraints runs in a
//! transaction: the statement itself goes through the ordinary engine path,
//! then referential actions are applied (in dependency order, so multi-level
//! cascades work), and finally every affected constraint is validated over
//! the whole table. Trusted constraints have no violating rows, so any
//! violating row was produced by this statement. Constraints added WITH
//! NOCHECK may have old violating rows: their violations are recorded before
//! the statement and only new ones fail.
use super::catalog::{self, Constraint, Table, Type, quote};
use super::errors::{self, Verb};
use super::{Transaction, translate};
use crate::engine::{Execution, Parameter, Session, ext};
use anyhow::{Result, bail};
use msduck_sql::dialect::ext::constraints::Referential;
use sqlparser::ast::*;
use std::collections::{HashMap, HashSet};
use std::ops::ControlFlow;

/// The verb of a DML statement, looking through WITH.
pub(crate) fn verb(statement: &Statement) -> Option<Verb> {
    match statement {
        Statement::Insert(_) => Some(Verb::Insert),
        Statement::Update(_) => Some(Verb::Update),
        Statement::Delete(_) => Some(Verb::Delete),
        Statement::Merge(_) => Some(Verb::Merge),
        Statement::Query(query) => match query.body.as_ref() {
            SetExpr::Insert(inner)
            | SetExpr::Update(inner)
            | SetExpr::Delete(inner)
            | SetExpr::Merge(inner) => verb(inner),
            _ => None,
        },
        _ => None,
    }
}

/// What a statement did to a table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Event {
    Insert,
    Update,
    Delete,
    Merge,
}

impl Event {
    fn of(verb: Verb) -> Self {
        match verb {
            Verb::Insert => Self::Insert,
            Verb::Update | Verb::AlterTable => Self::Update,
            Verb::Delete => Self::Delete,
            Verb::Merge => Self::Merge,
        }
    }
    /// Rows may have been written (CHECK and referencing side).
    fn writes(self) -> bool {
        self != Self::Delete
    }
    /// Keys may have been removed (referenced side).
    fn removes(self) -> bool {
        self != Self::Insert
    }
}

fn parse_expr(text: &str) -> Result<Expr> {
    let mut parser =
        sqlparser::parser::Parser::new(&crate::dialect::ServerDialect).try_with_sql(text)?;
    Ok(parser.parse_expr()?)
}

/// The native predicate selecting rows that violate a CHECK constraint.
fn check_violation(session: &Session, constraint: &Constraint) -> Result<String> {
    let definition = constraint
        .definition
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("CHECK constraint {} has no definition", constraint.name))?;
    let native = translate::check(session, &constraint.table, &parse_expr(definition)?)?;
    Ok(format!("NOT ({native})"))
}

fn conjunction(parts: impl IntoIterator<Item = String>) -> String {
    let parts: Vec<String> = parts.into_iter().collect();
    if parts.is_empty() {
        "true".into()
    } else {
        parts.join(" AND ")
    }
}

/// Rows of the referencing table whose complete key has no referenced row.
fn orphans(constraint: &Constraint, alias: &str) -> Result<String> {
    let referenced = constraint.referenced.as_ref().ok_or_else(|| {
        anyhow::anyhow!("foreign key {} lost its referenced table", constraint.name)
    })?;
    let present = conjunction(
        constraint
            .columns
            .iter()
            .map(|c| format!("{alias}.{} IS NOT NULL", quote(c))),
    );
    let matched = conjunction(
        constraint
            .columns
            .iter()
            .zip(&constraint.referenced_columns)
            .map(|(c, r)| format!("__msduck_p.{} = {alias}.{}", quote(r), quote(c))),
    );
    Ok(format!(
        "{present} AND NOT EXISTS (SELECT 1 FROM {} __msduck_p WHERE {matched})",
        referenced.sql()
    ))
}

/// The columns identifying a violation, and the condition selecting
/// violating rows of the constrained table (aliased `__msduck_c`).
fn violation(session: &Session, constraint: &Constraint) -> Result<(Vec<String>, String)> {
    Ok(match constraint.kind {
        Type::Check => {
            let predicate = check_violation(session, constraint)?;
            let columns = if constraint.columns.is_empty() {
                vec!["1".to_string()]
            } else {
                constraint
                    .columns
                    .iter()
                    .map(|c| format!("__msduck_c.{}", quote(c)))
                    .collect()
            };
            (columns, predicate)
        }
        Type::Foreign => (
            constraint
                .columns
                .iter()
                .map(|c| format!("__msduck_c.{}", quote(c)))
                .collect(),
            orphans(constraint, "__msduck_c")?,
        ),
        Type::Primary | Type::Unique => (vec!["1".into()], "false".into()),
    })
}

fn violations_query(
    session: &Session,
    constraint: &Constraint,
    restriction: Option<String>,
) -> Result<String> {
    let (columns, condition) = violation(session, constraint)?;
    let restriction = restriction.map_or(String::new(), |r| format!(" AND {r}"));
    Ok(format!(
        "SELECT {} FROM {} __msduck_c WHERE ({condition}){restriction}",
        columns.join(","),
        constraint.table.sql()
    ))
}

fn exists(session: &Session, sql: &str) -> Result<bool> {
    Ok(session
        .db
        .query_row(&format!("SELECT EXISTS({sql})"), [], |row| row.get(0))?)
}

/// Whether existing rows violate `constraint` (WITH CHECK validation).
pub(crate) fn violates(session: &Session, constraint: &Constraint) -> Result<Option<()>> {
    let sql = violations_query(session, constraint, None)?;
    Ok(exists(session, &sql)?.then_some(()))
}

/// Temporary tables created for one statement.
struct Scratch {
    names: Vec<String>,
    mapped: Vec<String>,
    next: usize,
    token: u64,
}

impl Scratch {
    fn name(&mut self) -> String {
        self.next += 1;
        let name = format!("__msduck_ck_{}_{}", self.token, self.next);
        self.names.push(name.clone());
        name
    }
    fn create(&mut self, session: &Session, query: &str) -> Result<String> {
        let name = self.name();
        session.db.execute_batch(&format!(
            "CREATE OR REPLACE TEMP TABLE {} AS {query}",
            quote(&name)
        ))?;
        Ok(name)
    }
    fn cleanup(&self, session: &Session) {
        for name in &self.names {
            let _ = session
                .db
                .execute_batch(&format!("DROP TABLE IF EXISTS temp.main.{}", quote(name)));
        }
        for name in &self.mapped {
            let _ = session
                .db
                .execute_batch(&format!("DROP TABLE IF EXISTS main.{}", quote(name)));
        }
    }
}

/// Key values of a referenced table, recorded before it changes.
#[derive(Clone, Debug)]
struct Keys {
    table: i32,
    /// Lower-cased key columns, in key order.
    columns: Vec<String>,
    name: String,
}

/// Old and new key values of updated rows (ON UPDATE CASCADE).
#[derive(Clone, Debug)]
struct Mapping {
    table: i32,
    /// Native relation holding the pairs.
    relation: String,
    /// Lower-cased column → (old, new) column of the relation.
    columns: HashMap<String, (String, String)>,
}

struct Run<'a> {
    constraints: &'a [Constraint],
    scratch: Scratch,
    keys: Vec<Keys>,
    mappings: Vec<Mapping>,
    /// Untrusted constraint id → snapshot of its old violations.
    untrusted: HashMap<i32, String>,
    /// Tables written, and how.
    events: HashMap<i32, HashSet<Event>>,
    /// Foreign keys whose actions wrote referencing rows.
    acted: HashSet<i32>,
    /// Tables written by referential actions.
    action_tables: HashSet<i32>,
    /// Columns the statement's UPDATE assigns, when known.
    assigned: Option<HashSet<String>>,
    /// The INSERT target and its largest row id before the statement: rows
    /// above it are the new ones.
    inserted: Option<(Table, i64)>,
}

impl Run<'_> {
    /// Whether `columns` of `table` may have changed. Only referential
    /// actions and the assigned columns of the statement's UPDATE change
    /// values; constraints over other columns keep their results.
    fn may_change(&self, table: i32, columns: &[String]) -> bool {
        match &self.assigned {
            Some(assigned) if !self.action_tables.contains(&table) => columns
                .iter()
                .any(|column| assigned.contains(&column.to_lowercase())),
            _ => true,
        }
    }

    /// A condition restricting the rows of `table` to check, if only some can
    /// be new.
    fn restriction(&self, table: i32) -> Option<String> {
        self.inserted
            .as_ref()
            .filter(|(target, _)| target.id == table && !self.action_tables.contains(&table))
            .map(|(_, rowid)| format!("__msduck_c.rowid > {rowid}"))
    }

    fn keys_of(&self, table: i32, columns: &[String]) -> Option<&Keys> {
        let wanted: Vec<String> = columns.iter().map(|c| c.to_lowercase()).collect();
        self.keys
            .iter()
            .find(|k| k.table == table && k.columns == wanted)
    }

    /// Record the referenced key values of every enabled foreign key into
    /// `table`, before the table changes. Keys the statement cannot change
    /// are skipped unless `all`.
    fn snapshot_keys(&mut self, session: &Session, table: &Table, all: bool) -> Result<()> {
        for constraint in self.constraints {
            if constraint.kind != Type::Foreign
                || !constraint.enabled()
                || constraint.referenced.as_ref().map(|r| r.id) != Some(table.id)
                || self
                    .keys_of(table.id, &constraint.referenced_columns)
                    .is_some()
                || (!all && !self.may_change(table.id, &constraint.referenced_columns))
            {
                continue;
            }
            let selected = constraint
                .referenced_columns
                .iter()
                .enumerate()
                .map(|(i, c)| format!("{} AS k{i}", quote(c)))
                .collect::<Vec<_>>()
                .join(",");
            let present = conjunction(
                constraint
                    .referenced_columns
                    .iter()
                    .map(|c| format!("{} IS NOT NULL", quote(c))),
            );
            let name = self.scratch.create(
                session,
                &format!(
                    "SELECT DISTINCT {selected} FROM {} WHERE {present}",
                    table.sql()
                ),
            )?;
            self.keys.push(Keys {
                table: table.id,
                columns: constraint
                    .referenced_columns
                    .iter()
                    .map(|c| c.to_lowercase())
                    .collect(),
                name,
            });
        }
        Ok(())
    }

    /// Recorded keys that `table` no longer has.
    fn removed(&mut self, session: &Session, constraint: &Constraint) -> Result<Option<String>> {
        let Some(referenced) = constraint.referenced.clone() else {
            return Ok(None);
        };
        let Some(keys) = self
            .keys_of(referenced.id, &constraint.referenced_columns)
            .cloned()
        else {
            return Ok(None);
        };
        let current = constraint
            .referenced_columns
            .iter()
            .map(|c| quote(c))
            .collect::<Vec<_>>()
            .join(",");
        let name = self.scratch.create(
            session,
            &format!(
                "SELECT * FROM temp.main.{} EXCEPT SELECT {current} FROM {}",
                quote(&keys.name),
                referenced.sql()
            ),
        )?;
        let empty: bool = session.db.query_row(
            &format!(
                "SELECT NOT EXISTS(SELECT 1 FROM temp.main.{})",
                quote(&name)
            ),
            [],
            |row| row.get(0),
        )?;
        Ok((!empty).then_some(name))
    }

    fn written(&mut self, table: i32, event: Event) {
        self.events.entry(table).or_default().insert(event);
    }

    fn wrote(&self, table: i32, test: impl Fn(Event) -> bool) -> bool {
        self.events
            .get(&table)
            .is_some_and(|events| events.iter().any(|e| test(*e)))
    }
}

/// Every table a statement names. Over-approximating the written tables is
/// safe: validation of an unchanged table finds nothing new.
fn relations(statement: &Statement) -> Vec<ObjectName> {
    let mut names = Vec::new();
    let _ = visit_relations(statement, |name| {
        names.push(name.clone());
        ControlFlow::<()>::Continue(())
    });
    if let Statement::Insert(insert) = statement
        && let TableObject::TableName(name) = &insert.table
    {
        names.push(name.clone());
    }
    names
}

fn output_targets(statement: &Statement) -> Vec<ObjectName> {
    let mut targets = Vec::new();
    let mut add = |output: &Option<OutputClause>| {
        if let Some(OutputClause::Output {
            into_table: Some(into),
            ..
        }) = output
        {
            for target in &into.targets {
                match target {
                    Expr::Identifier(ident) => targets.push(ObjectName::from(vec![ident.clone()])),
                    Expr::CompoundIdentifier(parts) => {
                        targets.push(ObjectName::from(parts.clone()))
                    }
                    // `INTO table(columns)` parses as a call.
                    Expr::Function(function) => targets.push(function.name.clone()),
                    _ => {}
                }
            }
        }
    };
    match statement {
        Statement::Insert(insert) => add(&insert.output),
        Statement::Update(update) => add(&update.output),
        Statement::Delete(delete) => add(&delete.output),
        Statement::Merge(merge) => add(&merge.output),
        Statement::Query(query) => match query.body.as_ref() {
            SetExpr::Insert(inner)
            | SetExpr::Update(inner)
            | SetExpr::Delete(inner)
            | SetExpr::Merge(inner) => return output_targets(inner),
            _ => {}
        },
        _ => {}
    }
    targets
}

fn resolve(session: &Session, names: &[ObjectName]) -> Result<Vec<Table>> {
    let mut tables: Vec<Table> = Vec::new();
    for name in names {
        let Some((schema, table)) = catalog::split_name(name, &session.database.name) else {
            continue;
        };
        if table.starts_with('#') || table.starts_with('@') {
            continue;
        }
        if let Some(table) = catalog::table(&session.db, &schema, &table)?
            && !tables.iter().any(|t| t.id == table.id)
        {
            tables.push(table);
        }
    }
    Ok(tables)
}

/// The UPDATE inside a statement, with its WITH clause.
fn update_of(statement: &Statement) -> Option<(&Update, Option<&With>)> {
    match statement {
        Statement::Update(update) => Some((update, None)),
        Statement::Query(query) => match query.body.as_ref() {
            SetExpr::Update(Statement::Update(update)) => Some((update, query.with.as_ref())),
            _ => None,
        },
        _ => None,
    }
}

fn factor_name(factor: &TableFactor) -> Option<(&ObjectName, Option<&Ident>)> {
    match factor {
        TableFactor::Table { name, alias, .. } => Some((name, alias.as_ref().map(|a| &a.name))),
        _ => None,
    }
}

/// Materialize (old, new) values of `columns` for the rows `update` changes,
/// through the engine, before it runs.
fn map_update(
    session: &mut Session,
    parameters: &mut HashMap<String, Parameter>,
    statement: &Statement,
    table: &Table,
    columns: &[String],
    relation: &str,
) -> Result<HashMap<String, (String, String)>> {
    let unsupported = || {
        anyhow::anyhow!(
            "unsupported UPDATE shape for ON UPDATE CASCADE on {}",
            table.display()
        )
    };
    let (update, with) = update_of(statement).ok_or_else(unsupported)?;
    if update.limit.is_some() || !update.order_by.is_empty() {
        return Err(unsupported());
    }
    let (target, target_alias) = factor_name(&update.table.relation).ok_or_else(unsupported)?;
    let sources: Vec<TableWithJoins> = match &update.from {
        Some(UpdateTableFromKind::AfterSet(from) | UpdateTableFromKind::BeforeSet(from)) => {
            from.clone()
        }
        None => vec![],
    };
    // `UPDATE alias SET ... FROM table alias ...` names its target by alias.
    let aliased_in_from = target.0.len() == 1
        && sources.iter().any(|source| {
            std::iter::once(&source.relation)
                .chain(source.joins.iter().map(|j| &j.relation))
                .any(|factor| {
                    factor_name(factor).is_some_and(|(_, alias)| {
                        alias.is_some_and(|alias| {
                            target.0[0]
                                .as_ident()
                                .is_some_and(|t| t.value.eq_ignore_ascii_case(&alias.value))
                        })
                    })
                })
        });
    let qualifier: Vec<Ident> = match target_alias {
        Some(alias) => vec![alias.clone()],
        None => target
            .0
            .iter()
            .map(|part| part.as_ident().cloned())
            .collect::<Option<Vec<_>>>()
            .ok_or_else(unsupported)?,
    };
    let from = if aliased_in_from {
        sources
    } else {
        std::iter::once(update.table.clone())
            .chain(sources)
            .collect()
    };
    let mut assigned: HashMap<String, Expr> = HashMap::new();
    for assignment in &update.assignments {
        let AssignmentTarget::ColumnName(name) = &assignment.target else {
            return Err(unsupported());
        };
        let Some(column) = name.0.last().and_then(|part| part.as_ident()) else {
            return Err(unsupported());
        };
        assigned.insert(column.value.to_lowercase(), assignment.value.clone());
    }
    let mut projection = Vec::new();
    let mut pairs = HashMap::new();
    for (index, column) in columns.iter().enumerate() {
        let mut parts = qualifier.clone();
        parts.push(Ident::with_quote('[', column));
        let old = Expr::CompoundIdentifier(parts);
        let new = assigned
            .get(&column.to_lowercase())
            .cloned()
            .unwrap_or_else(|| old.clone());
        projection.push(SelectItem::ExprWithAlias {
            expr: old,
            alias: Ident::new(format!("o{index}")),
        });
        projection.push(SelectItem::ExprWithAlias {
            expr: new,
            alias: Ident::new(format!("n{index}")),
        });
        pairs.insert(
            column.to_lowercase(),
            (format!("o{index}"), format!("n{index}")),
        );
    }
    let mut select = match msduck_sql::batch::parse("SELECT 1")?.remove(0) {
        Statement::Query(query) => query,
        _ => unreachable!(),
    };
    select.with = with.cloned();
    let SetExpr::Select(body) = select.body.as_mut() else {
        unreachable!()
    };
    body.projection = projection;
    body.from = from;
    body.selection = update.selection.clone();
    body.into = Some(SelectInto {
        temporary: false,
        unlogged: false,
        table: false,
        targets: vec![Expr::CompoundIdentifier(vec![
            Ident::new("main"),
            Ident::new(relation),
        ])],
    });
    ext::reenter(session, "constraints", |session| {
        session.execute(Statement::Query(select), parameters)
    })?;
    Ok(pairs)
}

/// Lower-cased columns a top-level UPDATE assigns; None when unknown.
fn assigned_columns(statement: &Statement) -> Option<HashSet<String>> {
    let (update, _) = update_of(statement)?;
    let mut columns = HashSet::new();
    for assignment in &update.assignments {
        let AssignmentTarget::ColumnName(name) = &assignment.target else {
            return None;
        };
        columns.insert(name.0.last()?.as_ident()?.value.to_lowercase());
    }
    Some(columns)
}

fn assigns_any(statement: &Statement, columns: &[String]) -> bool {
    let Some((update, _)) = update_of(statement) else {
        return true;
    };
    update
        .assignments
        .iter()
        .any(|assignment| match &assignment.target {
            AssignmentTarget::ColumnName(name) => name
                .0
                .last()
                .and_then(|part| part.as_ident())
                .is_some_and(|column| {
                    columns
                        .iter()
                        .any(|c| c.eq_ignore_ascii_case(&column.value))
                }),
            AssignmentTarget::Tuple(_) => true,
        })
}

/// Tables reachable from `start` through enabled foreign keys with actions.
fn cascade_scope(constraints: &[Constraint], start: &[i32]) -> HashSet<i32> {
    let mut scope: HashSet<i32> = start.iter().copied().collect();
    let mut queue: Vec<i32> = start.to_vec();
    while let Some(table) = queue.pop() {
        for constraint in constraints {
            if constraint.kind == Type::Foreign
                && constraint.enabled()
                && constraint.referenced.as_ref().map(|r| r.id) == Some(table)
                && (constraint.on_delete != Referential::NoAction
                    || constraint.on_update != Referential::NoAction)
                && scope.insert(constraint.table.id)
            {
                queue.push(constraint.table.id);
            }
        }
    }
    scope
}

fn relevant(constraint: &Constraint, scope: &HashSet<i32>) -> bool {
    constraint.enabled()
        && match constraint.kind {
            Type::Check => scope.contains(&constraint.table.id),
            Type::Foreign => {
                scope.contains(&constraint.table.id)
                    || constraint
                        .referenced
                        .as_ref()
                        .is_some_and(|r| scope.contains(&r.id))
            }
            Type::Primary | Type::Unique => false,
        }
}

/// INSERT, UPDATE, DELETE or MERGE on tables with constraints.
pub(crate) fn dml(
    session: &mut Session,
    statement: &Statement,
    parameters: &mut HashMap<String, Parameter>,
) -> Result<Option<Execution>> {
    let Some(verb) = verb(statement) else {
        return Ok(None);
    };
    if !catalog::any_enforced(&session.db)? {
        return Ok(None);
    }
    let constraints = catalog::load(&session.db)?;
    // OUTPUT INTO targets cannot have enabled CHECK constraints (333) or
    // take part in foreign keys.
    for target in resolve(session, &output_targets(statement))? {
        if let Some(check) = constraints
            .iter()
            .find(|c| c.kind == Type::Check && c.enabled() && c.table.id == target.id)
        {
            return Err(errors::error(333, 1, format!("The target table '{}' of the OUTPUT INTO clause cannot have any enabled check constraints or any enabled rules. Found check constraint or rule '{}'.", target.name, check.name)).into());
        }
        if constraints.iter().any(|c| {
            c.kind == Type::Foreign
                && (c.table.id == target.id
                    || c.referenced.as_ref().is_some_and(|r| r.id == target.id))
        }) {
            bail!("OUTPUT INTO foreign-key destinations are not supported yet");
        }
    }
    let touched = resolve(session, &relations(statement))?;
    let event = Event::of(verb);
    let touched_ids: Vec<i32> = touched.iter().map(|t| t.id).collect();
    let scope = if event.removes() {
        cascade_scope(&constraints, &touched_ids)
    } else {
        touched_ids.iter().copied().collect()
    };
    let applies = constraints.iter().any(|c| {
        relevant(c, &scope)
            && match c.kind {
                Type::Check => event.writes(),
                _ => {
                    (event.writes() && scope.contains(&c.table.id))
                        || (event.removes()
                            && c.referenced.as_ref().is_some_and(|r| scope.contains(&r.id)))
                }
            }
    });
    if !applies {
        return Ok(None);
    }
    let transaction = Transaction::begin(session)?;
    let mut run = Run {
        constraints: &constraints,
        scratch: Scratch {
            names: vec![],
            mapped: vec![],
            next: 0,
            token: session.ext.token,
        },
        keys: vec![],
        mappings: vec![],
        untrusted: HashMap::new(),
        events: HashMap::new(),
        acted: HashSet::new(),
        action_tables: HashSet::new(),
        assigned: assigned_columns(statement),
        inserted: None,
    };
    let result = execute(
        session,
        statement,
        parameters,
        verb,
        &touched,
        &scope,
        &mut run,
        &transaction,
    );
    run.scratch.cleanup(session);
    let result = match result {
        Ok(execution) => Ok(execution),
        Err(Failure::Engine(error)) => Err(error),
        Err(Failure::Violation(error, undo)) => {
            if !transaction.owned {
                match undo {
                    Some((table, rowid)) => {
                        let undone = session.db.execute_batch(&format!(
                            "DELETE FROM {} WHERE rowid > {rowid}",
                            table.sql()
                        ));
                        if undone.is_err() {
                            super::invalidate(session);
                        }
                    }
                    None => super::invalidate(session),
                }
            }
            Err(match verb.command() {
                Some(command) => crate::query_error::attach_context(error.into(), vec![], command),
                None => error.into(),
            })
        }
    };
    transaction.finish(session, result).map(Some)
}

enum Failure {
    /// The statement or an action failed; the engine reports it.
    Engine(anyhow::Error),
    /// A constraint is violated; undo with the INSERT rows above a row id.
    Violation(msduck_core::diagnostic::SqlError, Option<(Table, i64)>),
}

impl From<anyhow::Error> for Failure {
    fn from(error: anyhow::Error) -> Self {
        Self::Engine(error)
    }
}

impl From<duckdb::Error> for Failure {
    fn from(error: duckdb::Error) -> Self {
        Self::Engine(error.into())
    }
}

#[allow(clippy::too_many_arguments)]
fn execute(
    session: &mut Session,
    statement: &Statement,
    parameters: &mut HashMap<String, Parameter>,
    verb: Verb,
    touched: &[Table],
    scope: &HashSet<i32>,
    run: &mut Run<'_>,
    transaction: &Transaction,
) -> Result<Execution, Failure> {
    let event = Event::of(verb);
    let constraints = run.constraints;
    let target = match statement {
        Statement::Insert(insert) if verb == Verb::Insert => match &insert.table {
            TableObject::TableName(name) => resolve(session, std::slice::from_ref(name))?
                .into_iter()
                .next(),
            _ => None,
        },
        _ => None,
    };
    if let Some(table) = target {
        let rowid: i64 = session.db.query_row(
            &format!("SELECT coalesce(max(rowid),-1) FROM {}", table.sql()),
            [],
            |row| row.get(0),
        )?;
        run.inserted = Some((table, rowid));
    }
    // Old violations of untrusted constraints stay acceptable.
    // An INSERT checks only its new rows, so it needs no record of them.
    for constraint in constraints {
        if constraint.untrusted && run.inserted.is_none() && relevant(constraint, scope) {
            let query = violations_query(session, constraint, None)?;
            let name = run.scratch.create(session, &query)?;
            run.untrusted.insert(constraint.id, name);
        }
    }
    if event.removes() {
        for table in touched {
            run.snapshot_keys(session, table, false)?;
        }
    }
    // ON UPDATE CASCADE needs each updated row's old and new key.
    if event == Event::Update {
        for table in touched {
            let cascading: Vec<&Constraint> = constraints
                .iter()
                .filter(|c| {
                    c.kind == Type::Foreign
                        && c.enabled()
                        && c.on_update == Referential::Cascade
                        && c.referenced.as_ref().is_some_and(|r| r.id == table.id)
                        && assigns_any(statement, &c.referenced_columns)
                })
                .collect();
            if cascading.is_empty() {
                continue;
            }
            let mut columns: Vec<String> = Vec::new();
            for constraint in &cascading {
                for column in &constraint.referenced_columns {
                    if !columns.iter().any(|c| c.eq_ignore_ascii_case(column)) {
                        columns.push(column.clone());
                    }
                }
            }
            let relation = run.scratch.name();
            run.scratch.names.pop();
            run.scratch.mapped.push(relation.clone());
            let pairs = map_update(session, parameters, statement, table, &columns, &relation)?;
            run.mappings.push(Mapping {
                table: table.id,
                relation: format!("main.{}", quote(&relation)),
                columns: pairs,
            });
        }
    }
    // Rows above the old largest row id are this INSERT's: deleting them
    // undoes it inside the caller's transaction.
    let undo = run
        .inserted
        .clone()
        .filter(|_| !transaction.owned && output_targets(statement).is_empty());
    let execution = ext::reenter(session, "constraints", |session| {
        session.execute(statement.clone(), parameters)
    })?;
    // An INSERT writes only its target; other named tables are read.
    match &run.inserted {
        Some((target, _)) => {
            let id = target.id;
            run.written(id, event);
        }
        None => {
            for table in touched {
                run.written(table.id, event);
            }
        }
    }
    if event.removes() {
        actions(session, run, touched, event)?;
    }
    if let Some(error) = validate(session, run, verb)? {
        return Err(Failure::Violation(error, undo));
    }
    Ok(execution)
}

fn set_list(constraint: &Constraint, values: impl Fn(&str) -> String) -> String {
    constraint
        .columns
        .iter()
        .map(|c| format!("{} = {}", quote(c), values(c)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn matches_removed(constraint: &Constraint, removed: &str) -> String {
    let matched = conjunction(
        constraint
            .columns
            .iter()
            .enumerate()
            .map(|(i, c)| format!("__msduck_r.k{i} = {}.{}", constraint.table.sql(), quote(c))),
    );
    format!(
        "EXISTS (SELECT 1 FROM temp.main.{} __msduck_r WHERE {matched})",
        quote(removed)
    )
}

/// Referential actions, breadth first from the tables the statement wrote.
fn actions(
    session: &mut Session,
    run: &mut Run<'_>,
    touched: &[Table],
    event: Event,
) -> Result<()> {
    let constraints = run.constraints;
    let mut queue: std::collections::VecDeque<(i32, Event)> =
        touched.iter().map(|t| (t.id, event)).collect();
    let mut steps = 0usize;
    while let Some((table, event)) = queue.pop_front() {
        steps += 1;
        if steps > 10_000 {
            bail!("referential actions did not converge");
        }
        for constraint in constraints {
            if constraint.kind != Type::Foreign
                || !constraint.enabled()
                || constraint.referenced.as_ref().map(|r| r.id) != Some(table)
            {
                continue;
            }
            let child = constraint.table.clone();
            let action = match event {
                Event::Insert => continue,
                Event::Delete => constraint.on_delete,
                Event::Update => constraint.on_update,
                Event::Merge => {
                    if constraint.on_delete == constraint.on_update {
                        constraint.on_delete
                    } else {
                        Referential::Cascade
                    }
                }
            };
            if action == Referential::NoAction {
                continue;
            }
            if event == Event::Update && action == Referential::Cascade {
                let mapping = run
                    .mappings
                    .iter()
                    .find(|m| {
                        m.table == table
                            && constraint
                                .referenced_columns
                                .iter()
                                .all(|c| m.columns.contains_key(&c.to_lowercase()))
                    })
                    .cloned();
                let Some(mapping) = mapping else {
                    // Without old and new key pairs, only unchanged keys are fine.
                    if run.removed(session, constraint)?.is_some() {
                        bail!(
                            "unsupported ON UPDATE CASCADE through foreign key {}",
                            constraint.name
                        );
                    }
                    continue;
                };
                let pairs: Vec<(String, (String, String))> = constraint
                    .columns
                    .iter()
                    .zip(&constraint.referenced_columns)
                    .map(|(column, referenced)| {
                        (
                            column.clone(),
                            mapping.columns[&referenced.to_lowercase()].clone(),
                        )
                    })
                    .collect();
                let changed = pairs
                    .iter()
                    .map(|(_, (old, new))| format!("m.{old} IS DISTINCT FROM m.{new}"))
                    .collect::<Vec<_>>()
                    .join(" OR ");
                let matched = conjunction(pairs.iter().map(|(column, (old, _))| {
                    format!("m.{old} = {}.{}", child.sql(), quote(column))
                }));
                let assignments = pairs
                    .iter()
                    .map(|(column, (_, new))| format!("{} = m.{new}", quote(column)))
                    .collect::<Vec<_>>()
                    .join(", ");
                run.snapshot_keys(session, &child, true)?;
                session.db.execute_batch(&format!(
                    "UPDATE {} SET {assignments} FROM {} m WHERE {matched} AND ({changed})",
                    child.sql(),
                    mapping.relation
                ))?;
                // Keys of the referencing table that were copied from the
                // referenced key move the same way.
                run.mappings.push(Mapping {
                    table: child.id,
                    relation: mapping.relation.clone(),
                    columns: pairs
                        .iter()
                        .map(|(column, pair)| (column.to_lowercase(), pair.clone()))
                        .collect(),
                });
                run.acted.insert(constraint.id);
                run.action_tables.insert(child.id);
                run.written(child.id, Event::Update);
                queue.push_back((child.id, Event::Update));
                continue;
            }
            let Some(removed) = run.removed(session, constraint)? else {
                continue;
            };
            if event == Event::Merge
                && (constraint.on_delete != constraint.on_update || action == Referential::Cascade)
            {
                bail!(
                    "unsupported MERGE that removes keys referenced by foreign key {} with different or cascading actions",
                    constraint.name
                );
            }
            run.snapshot_keys(session, &child, true)?;
            let condition = matches_removed(constraint, &removed);
            match action {
                Referential::Cascade => {
                    session
                        .db
                        .execute_batch(&format!("DELETE FROM {} WHERE {condition}", child.sql()))?;
                    run.action_tables.insert(child.id);
                    run.written(child.id, Event::Delete);
                    queue.push_back((child.id, Event::Delete));
                }
                Referential::SetNull => {
                    session.db.execute_batch(&format!(
                        "UPDATE {} SET {} WHERE {condition}",
                        child.sql(),
                        set_list(constraint, |_| "NULL".into())
                    ))?;
                    run.acted.insert(constraint.id);
                    run.action_tables.insert(child.id);
                    run.written(child.id, Event::Update);
                    queue.push_back((child.id, Event::Update));
                }
                Referential::SetDefault => {
                    let defaults = catalog::native_defaults(&session.db, &child)?;
                    session.db.execute_batch(&format!(
                        "UPDATE {} SET {} WHERE {condition}",
                        child.sql(),
                        set_list(constraint, |c| defaults
                            .get(&c.to_lowercase())
                            .cloned()
                            .unwrap_or_else(|| "NULL".into()))
                    ))?;
                    run.acted.insert(constraint.id);
                    run.action_tables.insert(child.id);
                    run.written(child.id, Event::Update);
                    queue.push_back((child.id, Event::Update));
                }
                Referential::NoAction => {}
            }
        }
    }
    Ok(())
}

/// The first violated constraint, CHECK constraints first, each in creation
/// order.
fn validate(
    session: &Session,
    run: &mut Run<'_>,
    verb: Verb,
) -> Result<Option<msduck_core::diagnostic::SqlError>> {
    let database = session.database.name.clone();
    let constraints = run.constraints;
    for pass in [Type::Check, Type::Foreign] {
        for constraint in constraints {
            if constraint.kind != pass || !constraint.enabled() {
                continue;
            }
            let child_written = run.wrote(constraint.table.id, Event::writes)
                && run.may_change(constraint.table.id, &constraint.columns);
            let parent_changed = constraint.referenced.as_ref().is_some_and(|r| {
                run.wrote(r.id, Event::removes)
                    && run.may_change(r.id, &constraint.referenced_columns)
            });
            let affected = match pass {
                Type::Check => child_written,
                _ => child_written || parent_changed,
            };
            if !affected {
                continue;
            }
            let restriction = if parent_changed {
                None
            } else {
                run.restriction(constraint.table.id)
            };
            // Old violations of untrusted constraints stay acceptable; when
            // only new rows are checked, every violation is new.
            let tolerated = restriction.is_none();
            let mut query = violations_query(session, constraint, restriction)?;
            if let Some(old) = run.untrusted.get(&constraint.id).filter(|_| tolerated) {
                query = format!("{query} EXCEPT ALL SELECT * FROM temp.main.{}", quote(old));
            }
            if !exists(session, &query)? {
                continue;
            }
            if pass == Type::Check {
                return Ok(Some(errors::check_conflict(verb, constraint, &database)));
            }
            // A violation whose key was removed from the referenced table is
            // reported against the referencing table.
            // Rows an action wrote are reported against the referenced table.
            let reference = parent_changed
                && !run.acted.contains(&constraint.id)
                && (!child_written
                    || {
                        let referenced = constraint.referenced.as_ref().unwrap();
                        match run.keys_of(referenced.id, &constraint.referenced_columns) {
                            Some(keys) => {
                                let matched = conjunction(
                                    constraint.columns.iter().enumerate().map(|(i, c)| {
                                        format!("__msduck_k.k{i} = __msduck_c.{}", quote(c))
                                    }),
                                );
                                let (_, condition) = violation(session, constraint)?;
                                exists(
                                    session,
                                    &format!(
                                        "SELECT 1 FROM {} __msduck_c WHERE {condition} AND EXISTS (SELECT 1 FROM temp.main.{} __msduck_k WHERE {matched})",
                                        constraint.table.sql(),
                                        quote(&keys.name)
                                    ),
                                )?
                            }
                            None => false,
                        }
                    });
            return Ok(Some(if reference {
                errors::reference_conflict(verb, constraint, &database)
            } else {
                errors::foreign_conflict(verb, constraint, &database)
            }));
        }
    }
    Ok(None)
}
