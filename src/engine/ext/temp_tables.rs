//! #temp tables and table variables.
//!
//! Every temporary object is an ordinary backend table in the `dbo` schema of
//! the database that was current when it was created, so the catalog,
//! declared types, IDENTITY, defaults, constraints and indexes behave exactly
//! as for permanent tables. Only the names differ (see `storage`):
//!
//! - `#t` gets a unique backend name per creation. The session's registry
//!   maps the name to its most recent visible creation, so a procedure can
//!   shadow its caller's `#t`.
//! - `##t` has one backend name per database that every session resolves.
//! - `@t` gets a unique backend name per declaration, registered in the
//!   frame of the batch that declared it.
//!
//! Lifetimes follow SQL Server. A batch (SQL batch, RPC request or nested
//! body) is a frame opened by `batch_begin` and closed by `batch_end`.
//! Closing a frame drops its table variables, and the `#` tables it created
//! unless it is a top-level SQL batch: those belong to the session. A session
//! drops what remains when it ends or is reset, and a global table when its
//! creator ends. Table variables keep their rows across ROLLBACK: writes
//! inside a transaction snapshot the variable, and a rollback restores the
//! snapshot.
use super::{Feature, reenter};
use crate::engine::{Execution, Parameter, Session};
use anyhow::Result;
use msduck_core::diagnostic::SqlError;
use msduck_sql::dialect::ext::temp_tables::{self as syntax, Kind, TempName};
use sqlparser::ast::*;
use std::collections::HashMap;

mod storage;

const NAME: &str = "temp_tables";

#[derive(Default)]
pub(crate) struct State {
    /// Open batches, innermost last.
    frames: Vec<Frame>,
    /// Local temporary tables this session created and has not dropped, in
    /// creation order.
    locals: Vec<Local>,
    /// Global temporary tables this session created.
    globals: Vec<Global>,
    /// Backend tables dropped inside a transaction because their scope
    /// ended. A rollback would bring them back, so they are dropped again.
    graveyard: Vec<Backend>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Backend {
    /// The database catalog that holds the table.
    alias: String,
    /// The table's name in `dbo`.
    physical: String,
}

struct Frame {
    rpc: bool,
    variables: Vec<Variable>,
    /// Table variables the batch declares, by name. SQL Server creates them
    /// when the batch starts, so a declaration in a branch that does not
    /// run still declares the variable.
    declared: Vec<syntax::TableVariable>,
}

struct Variable {
    key: String,
    name: String,
    backend: Backend,
    definition: String,
    /// Rows as of the last write in the current transaction, restored after a
    /// rollback; `Some` (possibly empty) once the variable changed or was
    /// declared inside the transaction.
    saved: Option<Vec<duckdb::arrow::record_batch::RecordBatch>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Level {
    /// Created by a top-level SQL batch: lives until the session ends.
    Session,
    /// Created in the frame at this depth: dropped when it ends.
    Frame(usize),
}

struct Local {
    key: String,
    name: String,
    backend: Backend,
    level: Level,
    /// Dropped by DROP TABLE inside a transaction that may still roll back.
    dropped: bool,
}

struct Global {
    name: String,
    backend: Backend,
}

pub(super) struct Hooks;

impl Feature for Hooks {
    fn name(&self) -> &'static str {
        NAME
    }

    fn batch_begin(&self, session: &mut Session, rpc: bool) {
        session.ext.temp_tables.frames.push(Frame {
            rpc,
            variables: Vec::new(),
            declared: Vec::new(),
        });
    }

    fn batch_end(&self, session: &mut Session) {
        let Some(frame) = session.ext.temp_tables.frames.pop() else {
            return;
        };
        let depth = session.ext.temp_tables.frames.len();
        let mut ended = frame
            .variables
            .into_iter()
            .map(|variable| variable.backend)
            .collect::<Vec<_>>();
        let state = &mut session.ext.temp_tables;
        let (finished, kept) = std::mem::take(&mut state.locals)
            .into_iter()
            .partition::<Vec<_>, _>(|local| local.level == Level::Frame(depth));
        state.locals = kept;
        ended.extend(finished.into_iter().map(|local| local.backend));
        for backend in ended {
            discard(session, backend);
        }
        if session.transactions == 0 {
            bury(session);
        }
    }

    fn batch(
        &self,
        session: &mut Session,
        sql: &str,
        _parameters: &HashMap<String, Parameter>,
        rpc: bool,
    ) -> Option<(Vec<u8>, bool)> {
        // Only batches that may create temporary tables or declare table
        // variables need a look before execution; parsing errors are left
        // to the engine.
        if !(sql.contains('#') || sql.contains('@') && sql.to_ascii_uppercase().contains("TABLE")) {
            return None;
        }
        let statements = msduck_sql::batch::parse(sql).ok()?;
        let compiled = compile(&statements);
        if let Some(frame) = session.ext.temp_tables.frames.last_mut() {
            frame.declared = compiled.declared;
        }
        // SQL Server compiles the whole batch first: one batch cannot
        // create the same temporary table twice, even in exclusive branches.
        let name = compiled.duplicate?;
        let error = anyhow::Error::from(SqlError::new(
            2714,
            1,
            format!("There is already an object named '{name}' in the database."),
        ));
        let mut out = Vec::new();
        session.last_error = crate::engine::emit_error(&mut out, &error);
        if rpc {
            out.push(0x79);
            out.extend(session.last_error.to_le_bytes());
            crate::tds::done(&mut out, 0xfe, 2, 224, 0);
        } else {
            crate::tds::done(&mut out, 0xfd, 2, 0, 0);
        }
        Some((out, false))
    }

    fn statement(
        &self,
        session: &mut Session,
        statement: &mut Statement,
        parameters: &mut HashMap<String, Parameter>,
    ) -> Result<Option<Execution>> {
        if let Some(variable) = syntax::table_variable(statement) {
            declare(session, &variable)?;
            return Ok(Some(Execution::statement(Vec::new(), None, 0)));
        }
        match statement {
            Statement::CreateTable(table) => {
                if let Some(name) = syntax::classify(&table.name) {
                    return create(session, statement.clone(), &name, parameters).map(Some);
                }
            }
            Statement::Query(query) => {
                if let Some(name) = into_target(query) {
                    return create(session, statement.clone(), &name, parameters).map(Some);
                }
            }
            Statement::Drop {
                object_type: ObjectType::Table,
                names,
                ..
            } if names.iter().any(|name| syntax::classify(name).is_some()) => {
                return drop_tables(session, statement.clone(), parameters).map(Some);
            }
            Statement::Truncate(truncate) => {
                for target in &truncate.table_names {
                    if let Some(name) = syntax::classify(&target.name)
                        && name.kind == Kind::Variable
                    {
                        return Err(syntax_error(&name.name).into());
                    }
                }
            }
            Statement::Rollback { .. } if !session.ext.temp_tables.is_empty() => {
                let result = reenter(session, NAME, |session| {
                    session.execute(statement.clone(), parameters)
                });
                // A full rollback restores through `transaction_end`; a
                // rollback to a savepoint keeps the transaction open.
                if session.transactions > 0 {
                    restore(session);
                }
                return result.map(Some);
            }
            _ => {}
        }
        let mut rewritten = statement.clone();
        let written = syntax::targets(&rewritten);
        let changed = syntax::rewrite(&mut rewritten, &mut |name| {
            resolve(session, name).map(Some)
        })?;
        if !changed {
            return Ok(None);
        }
        let result = reenter(session, NAME, |session| {
            session.execute(rewritten, parameters)
        })
        .map_err(|error| localize(&session.ext.temp_tables, error))?;
        if session.transactions > 0 {
            for name in written.iter().filter(|name| name.kind == Kind::Variable) {
                save(session, &name.key());
            }
        }
        Ok(Some(result))
    }

    fn rewrite_statement(
        &self,
        session: &Session,
        statement: &mut Statement,
        _parameters: &HashMap<String, Parameter>,
    ) -> Result<()> {
        // Statements that reach translation without the statement hook,
        // such as prepared statements being validated.
        syntax::rewrite(statement, &mut |name| lookup(session, name).map(Some))?;
        Ok(())
    }

    fn rewrite_expr(
        &self,
        session: &Session,
        expr: &mut Expr,
        _parameters: &HashMap<String, Parameter>,
    ) -> Result<()> {
        match expr {
            // Scalar evaluations (IF, WHILE, SET) bypass the statement hook.
            Expr::Subquery(query)
            | Expr::Exists {
                subquery: query, ..
            }
            | Expr::InSubquery {
                subquery: query, ..
            } => {
                syntax::rewrite(query.as_mut(), &mut |name| {
                    lookup(session, name).map(Some)
                })?;
            }
            Expr::Function(function) => object_id(session, function),
            _ => {}
        }
        Ok(())
    }

    fn transaction_end(&self, session: &mut Session, committed: bool) {
        if committed {
            let state = &mut session.ext.temp_tables;
            state.locals.retain(|local| !local.dropped);
            for frame in &mut state.frames {
                for variable in &mut frame.variables {
                    variable.saved = None;
                }
            }
            state.graveyard.clear();
        } else {
            bury(session);
            restore(session);
        }
    }

    fn bootstrap_database(&self, db: &duckdb::Connection) -> Result<()> {
        // No session survives a restart, so every temporary table left in a
        // database file is an orphan.
        storage::drop_orphans(db)
    }

    fn session_end(&self, session: &mut Session) {
        if session.transactions > 0 {
            let _ = session.rollback_transaction("");
        }
        let state = std::mem::take(&mut session.ext.temp_tables);
        let backends = state
            .frames
            .into_iter()
            .flat_map(|frame| frame.variables)
            .map(|variable| variable.backend)
            .chain(state.locals.into_iter().map(|local| local.backend))
            .chain(state.globals.into_iter().map(|global| global.backend))
            .chain(state.graveyard);
        for backend in backends {
            let _ = storage::drop_table(session, &backend);
        }
    }
}

impl State {
    fn is_empty(&self) -> bool {
        self.locals.is_empty()
            && self.globals.is_empty()
            && self
                .frames
                .iter()
                .all(|frame| frame.variables.is_empty())
    }
}

#[derive(Default)]
struct Compiled {
    /// Table variable declarations, including those in blocks and branches.
    declared: Vec<syntax::TableVariable>,
    /// A temporary table the batch creates more than once.
    duplicate: Option<String>,
}

fn compile(statements: &[Statement]) -> Compiled {
    #[derive(Default)]
    struct Visit {
        compiled: Compiled,
        created: Vec<String>,
    }
    impl Visitor for Visit {
        type Break = ();
        fn pre_visit_statement(&mut self, statement: &Statement) -> std::ops::ControlFlow<()> {
            self.compiled
                .declared
                .extend(syntax::table_variable(statement));
            let created = match statement {
                Statement::CreateTable(table) => syntax::classify(&table.name),
                Statement::Query(query) => into_target(query),
                _ => None,
            };
            if let Some(name) = created.filter(|name| name.kind != Kind::Variable) {
                if self.created.contains(&name.key()) {
                    self.compiled.duplicate.get_or_insert(name.name);
                } else {
                    self.created.push(name.key());
                }
            }
            std::ops::ControlFlow::Continue(())
        }
    }
    let mut visit = Visit::default();
    for statement in statements {
        let _ = statement.visit(&mut visit);
    }
    visit.compiled
}

fn into_target(query: &Query) -> Option<TempName> {
    let SetExpr::Select(select) = first_select(&query.body)? else {
        return None;
    };
    let into = select.into.as_ref()?;
    let parts = match into.targets.as_slice() {
        [Expr::Identifier(id)] => vec![id],
        [Expr::CompoundIdentifier(ids)] => ids.iter().collect(),
        _ => return None,
    };
    syntax::classify_parts(&parts)
}

fn first_select(body: &SetExpr) -> Option<&SetExpr> {
    match body {
        SetExpr::Select(_) => Some(body),
        SetExpr::SetOperation { left, .. } => first_select(left),
        SetExpr::Query(query) => first_select(&query.body),
        _ => None,
    }
}

fn first_select_mut(body: &mut SetExpr) -> Option<&mut Select> {
    match body {
        SetExpr::Select(select) => Some(select),
        SetExpr::SetOperation { left, .. } => first_select_mut(left),
        SetExpr::Query(query) => first_select_mut(&mut query.body),
        _ => None,
    }
}

fn syntax_error(near: &str) -> SqlError {
    let mut error = SqlError::new(102, 1, format!("Incorrect syntax near '{near}'."));
    error.severity = 15;
    error
}

fn invalid_object(name: &str) -> SqlError {
    SqlError::new(208, 0, format!("Invalid object name '{name}'."))
}

fn undeclared_variable(name: &str) -> SqlError {
    let mut error = SqlError::new(
        1087,
        2,
        format!("Must declare the table variable \"{name}\"."),
    );
    error.severity = 15;
    error
}

/// Resolve a reference for execution, creating a table variable the batch
/// declared in a branch that did not run.
fn resolve(session: &mut Session, name: &TempName) -> Result<String> {
    if name.kind == Kind::Variable
        && lookup(session, name).is_err()
        && let Some(frame) = session.ext.temp_tables.frames.last()
        && let Some(declared) = frame
            .declared
            .iter()
            .find(|declared| declared.name.value.eq_ignore_ascii_case(&name.name))
            .cloned()
    {
        declare(session, &declared)?;
    }
    lookup(session, name)
}

/// The backend table a reference names, without side effects.
fn lookup(session: &Session, name: &TempName) -> Result<String> {
    let state = &session.ext.temp_tables;
    let key = name.key();
    let alias = session.database.alias();
    let backend = match name.kind {
        Kind::Variable => state
            .frames
            .last()
            .and_then(|frame| frame.variables.iter().find(|variable| variable.key == key))
            .map(|variable| &variable.backend)
            .ok_or_else(|| undeclared_variable(&name.name))?
            .clone(),
        Kind::Local => state
            .locals
            .iter()
            .rev()
            .find(|local| local.key == key && !local.dropped)
            .map(|local| local.backend.clone())
            .ok_or_else(|| invalid_object(&name.name))?,
        Kind::Global => {
            let backend = Backend {
                alias: alias.to_owned(),
                physical: storage::global_name(&name.name),
            };
            if !storage::exists(&session.db, &backend.physical)? {
                return Err(invalid_object(&name.name).into());
            }
            backend
        }
    };
    if backend.alias != alias {
        anyhow::bail!(
            "unsupported reference to temporary object {} from another database; it belongs to the database that was current when it was created",
            name.name
        );
    }
    Ok(backend.physical)
}

/// `DECLARE @t TABLE (...)`: create the variable's table in the current
/// frame. Executing the declaration again (in a loop) keeps its rows.
fn declare(session: &mut Session, variable: &syntax::TableVariable) -> Result<()> {
    let key = variable.name.value.to_lowercase();
    let Some(frame) = session.ext.temp_tables.frames.last() else {
        anyhow::bail!("unsupported table variable outside a batch");
    };
    if frame.variables.iter().any(|existing| existing.key == key) {
        return Ok(());
    }
    let backend = Backend {
        alias: session.database.alias().to_owned(),
        physical: storage::unique_name("tv", &variable.name.value),
    };
    let create = syntax::create_table(
        &variable.definition,
        &Ident::with_quote('"', &backend.physical),
    )?;
    let mut parameters = HashMap::new();
    reenter(session, NAME, |session| session.execute(create, &mut parameters))
        .map_err(|error| rename_error(error, &backend.physical, &variable.name.value))?;
    let saved = (session.transactions > 0).then(Vec::new);
    if let Some(frame) = session.ext.temp_tables.frames.last_mut() {
        frame.variables.push(Variable {
            key,
            name: variable.name.value.clone(),
            backend,
            definition: variable.definition.clone(),
            saved,
        });
    }
    Ok(())
}

/// CREATE TABLE #t / ##t and SELECT ... INTO #t / ##t.
fn create(
    session: &mut Session,
    mut statement: Statement,
    name: &TempName,
    parameters: &mut HashMap<String, Parameter>,
) -> Result<Execution> {
    let select_into = matches!(statement, Statement::Query(_));
    let duplicate = || {
        SqlError::new(
            2714,
            if select_into { 1 } else { 6 },
            format!("There is already an object named '{}' in the database.", name.name),
        )
    };
    let alias = session.database.alias().to_owned();
    let depth = session.ext.temp_tables.frames.len();
    let level = match session.ext.temp_tables.frames.as_slice() {
        [] => Level::Session,
        [frame] if !frame.rpc => Level::Session,
        _ => Level::Frame(depth - 1),
    };
    let physical = match name.kind {
        Kind::Variable => return Err(syntax_error(&name.name).into()),
        Kind::Local => {
            let key = name.key();
            if session
                .ext
                .temp_tables
                .locals
                .iter()
                .any(|local| local.key == key && local.level == level && !local.dropped)
            {
                return Err(duplicate().into());
            }
            storage::unique_name("temp", &name.name)
        }
        Kind::Global => {
            let physical = storage::global_name(&name.name);
            if storage::exists(&session.db, &physical)? {
                return Err(duplicate().into());
            }
            physical
        }
    };
    let backend = syntax::backend_name(&physical);
    match &mut statement {
        Statement::CreateTable(table) => table.name = backend,
        Statement::Query(query) => {
            if let Some(select) = first_select_mut(&mut query.body)
                && let Some(into) = &mut select.into
            {
                into.targets = vec![Expr::CompoundIdentifier(
                    backend
                        .0
                        .iter()
                        .filter_map(|part| part.as_ident().cloned())
                        .collect(),
                )];
            }
        }
        _ => unreachable!("only CREATE TABLE and SELECT INTO create tables"),
    }
    // The definition or query may read other temporary objects.
    syntax::rewrite(&mut statement, &mut |other| resolve(session, other).map(Some))?;
    let execution = reenter(session, NAME, |session| {
        session.execute(statement, parameters)
    })
    .map_err(|error| localize(&session.ext.temp_tables, error))
    .map_err(|error| rename_error(error, &physical, &name.name))?;
    let backend = Backend { alias, physical };
    let state = &mut session.ext.temp_tables;
    match name.kind {
        Kind::Local => state.locals.push(Local {
            key: name.key(),
            name: name.name.clone(),
            backend,
            level,
        dropped: false,
        }),
        _ => state.globals.push(Global {
            name: name.name.clone(),
            backend,
        }),
    }
    Ok(execution)
}

/// DROP TABLE naming temporary tables (possibly with permanent ones).
fn drop_tables(
    session: &mut Session,
    mut statement: Statement,
    parameters: &mut HashMap<String, Parameter>,
) -> Result<Execution> {
    let Statement::Drop {
        names, if_exists, ..
    } = &mut statement
    else {
        unreachable!("DROP TABLE");
    };
    let mut dropped = Vec::new();
    let mut kept = Vec::new();
    for mut object in std::mem::take(names) {
        let Some(name) = syntax::classify(&object) else {
            kept.push(object);
            continue;
        };
        if name.kind == Kind::Variable {
            return Err(syntax_error(&name.name).into());
        }
        match lookup(session, &name) {
            Ok(physical) => {
                object = syntax::backend_name(&physical);
                dropped.push((name, physical));
                kept.push(object);
            }
            Err(_) if *if_exists => {}
            Err(error) => {
                if error
                    .downcast_ref::<SqlError>()
                    .is_some_and(|error| error.number == 208)
                {
                    let mut error = SqlError::new(
                        3701,
                        5,
                        format!(
                            "Cannot drop the table '{}', because it does not exist or you do not have permission.",
                            name.name
                        ),
                    );
                    error.severity = 11;
                    return Err(error.into());
                }
                return Err(error);
            }
        }
    }
    if kept.is_empty() {
        // Only missing temporary tables with IF EXISTS.
        return Ok(Execution::statement(Vec::new(), None, 199));
    }
    *names = kept;
    let execution = reenter(session, NAME, |session| {
        session.execute(statement, parameters)
    })
    .map_err(|error| localize(&session.ext.temp_tables, error))?;
    let in_transaction = session.transactions > 0;
    let state = &mut session.ext.temp_tables;
    for (name, physical) in dropped {
        match name.kind {
            Kind::Local => {
                if in_transaction {
                    for local in &mut state.locals {
                        if local.backend.physical == physical {
                            local.dropped = true;
                        }
                    }
                } else {
                    state.locals.retain(|local| local.backend.physical != physical);
                }
            }
            _ => state
                .globals
                .retain(|global| global.backend.physical != physical || in_transaction),
        }
    }
    Ok(execution)
}

/// A frame ended: drop one of its tables. Inside a transaction the drop is
/// remembered, because a rollback would restore the table.
fn discard(session: &mut Session, backend: Backend) {
    let dropped = storage::drop_table(session, &backend).is_ok();
    if session.transactions > 0 || !dropped {
        session.ext.temp_tables.graveyard.push(backend);
    }
}

/// Drop tables whose scope ended inside a transaction that is now over.
fn bury(session: &mut Session) {
    let graveyard = std::mem::take(&mut session.ext.temp_tables.graveyard);
    for backend in graveyard {
        let _ = storage::drop_table(session, &backend);
    }
}

/// Snapshot a table variable written inside a transaction.
fn save(session: &mut Session, key: &str) {
    let Some(variable) = session
        .ext
        .temp_tables
        .frames
        .last()
        .and_then(|frame| frame.variables.iter().find(|variable| variable.key == key))
    else {
        return;
    };
    if let Ok(rows) = storage::snapshot(&session.db, &variable.backend.physical)
        && let Some(variable) = session
            .ext
            .temp_tables
            .frames
            .last_mut()
            .and_then(|frame| frame.variables.iter_mut().find(|variable| variable.key == key))
    {
        variable.saved = Some(rows);
    }
}

/// After a rollback: give table variables back the rows they had, and forget
/// temporary tables whose creation was rolled back.
fn restore(session: &mut Session) {
    let in_transaction = session.transactions > 0;
    let mut restores = Vec::new();
    for frame in &mut session.ext.temp_tables.frames {
        for variable in &mut frame.variables {
            let Some(rows) = (if in_transaction {
                variable.saved.clone()
            } else {
                variable.saved.take()
            }) else {
                continue;
            };
            restores.push((
                variable.backend.clone(),
                variable.definition.clone(),
                rows,
            ));
        }
    }
    for (backend, definition, rows) in restores {
        let _ = storage::restore(session, &backend, &definition, rows);
    }
    let db = &session.db;
    let state = &mut session.ext.temp_tables;
    let current = session.database.alias();
    let live = |backend: &Backend| backend.alias != current || storage::exists(db, &backend.physical).unwrap_or(true);
    state.locals.retain(|local| live(&local.backend));
    for local in &mut state.locals {
        local.dropped = false;
    }
    state.globals.retain(|global| live(&global.backend));
}

/// Report temporary objects by the names the batch used, not their backend
/// tables.
fn localize(state: &State, error: anyhow::Error) -> anyhow::Error {
    let names = state
        .frames
        .iter()
        .flat_map(|frame| &frame.variables)
        .map(|variable| (&variable.backend.physical, &variable.name))
        .chain(
            state
                .locals
                .iter()
                .map(|local| (&local.backend.physical, &local.name)),
        )
        .chain(
            state
                .globals
                .iter()
                .map(|global| (&global.backend.physical, &global.name)),
        )
        .collect::<Vec<_>>();
    names
        .into_iter()
        .fold(error, |error, (physical, name)| rename_error(error, physical, name))
}

fn rename_error(mut error: anyhow::Error, physical: &str, name: &str) -> anyhow::Error {
    let rename = |message: &str| {
        message
            .replace(&format!("dbo.\"{physical}\""), name)
            .replace(&format!("\"{physical}\""), name)
            .replace(physical, name)
    };
    if let Some(sql) = error.downcast_mut::<SqlError>() {
        if sql.message.contains(physical) {
            sql.message = rename(&sql.message);
            sql.message_utf16 = None;
        }
        return error;
    }
    let message = error.to_string();
    if message.contains(physical) {
        let renamed = rename(&message);
        error = error.context(renamed);
    }
    error
}

/// `OBJECT_ID('tempdb..#t')` names the session's backend table. Like SQL
/// Server, an unqualified `#t` is looked up in the current database, where
/// no such object exists.
fn object_id(session: &Session, function: &mut Function) {
    if !function.name.to_string().eq_ignore_ascii_case("OBJECT_ID") {
        return;
    }
    let FunctionArguments::List(list) = &mut function.args else {
        return;
    };
    let Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(value)))) = list.args.first_mut()
    else {
        return;
    };
    let text = match &mut value.value {
        Value::SingleQuotedString(text) | Value::NationalStringLiteral(text) => text,
        _ => return,
    };
    let Ok(name) = sqlparser::parser::Parser::new(&msduck_sql::dialect::ServerDialect)
        .try_with_sql(text)
        .and_then(|mut parser| parser.parse_object_name(false))
    else {
        return;
    };
    let Some(temp) = syntax::classify(&name) else {
        return;
    };
    let qualified = name.0.len() == 3 && temp.kind != Kind::Variable;
    *text = match lookup(session, &temp) {
        Ok(physical) if qualified => format!("dbo.[{physical}]"),
        // No backend table has a name starting with '#'.
        _ => format!("dbo.[{}]", temp.name.replace(']', "]]")),
    };
}
