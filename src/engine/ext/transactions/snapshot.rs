//! ALTER DATABASE ... SET ALLOW_SNAPSHOT_ISOLATION and the rules SNAPSHOT
//! transactions follow, captured in the `snapshot` section of
//! reference/gaps-transactions.json:
//!
//! - a SNAPSHOT transaction that reads or writes a table of a database whose
//!   option is OFF fails with 3952, which ends the batch and rolls back an
//!   open transaction (inside TRY it dooms the transaction instead);
//! - an update or delete of a row that another transaction changed after
//!   the snapshot began fails with 3960, with the same effects;
//! - changing the option waits for the transactions that were active (ON
//!   for those that wrote, OFF also for SNAPSHOT transactions). Meanwhile
//!   the state is in transition and new SNAPSHOT transactions fail with
//!   3956 (to ON) or 3954 (to OFF), with the effects of 3952.
//!
//! DuckDB runs every transaction as snapshot isolation (see options.rs), so
//! the rows a SNAPSHOT transaction reads need no further work here.
use super::super::super::{Execution, Parameter, Session, StatementErrors};
use crate::database_catalog::SnapshotIsolation;
use anyhow::{Result, bail};
use msduck_core::diagnostic::SqlError;
use msduck_sql::dialect::alter_database::{Request, Termination};
use sqlparser::ast::{
    Delete, FromTable, Ident, ObjectName, Query, SetExpr, Statement, TableFactor, TableObject,
    TableWithJoins, UpdateTableFromKind, Visit, Visitor,
};
use std::{
    collections::HashMap,
    ops::ControlFlow,
    sync::{
        LazyLock, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

/// TDS numbering of SNAPSHOT.
const SNAPSHOT: u8 = 5;

fn failed(error: SqlError) -> anyhow::Error {
    StatementErrors(vec![
        error,
        SqlError::new(5069, 1, "ALTER DATABASE statement failed."),
    ])
    .into()
}

/// An ALTER DATABASE whose options include ALLOW_SNAPSHOT_ISOLATION.
/// Checks run in SQL Server's order: transaction, CURRENT in master,
/// conflicting values, the database, other options, the termination clause.
pub(super) fn alter(session: &mut Session, request: Request) -> Result<Execution> {
    if session.transactions > 0 {
        bail!(SqlError::new(
            226,
            6,
            "ALTER DATABASE statement not allowed within multi-statement transaction."
        ));
    }
    let name = match &request.database {
        Some(name) => name.value.clone(),
        None if session.database.database_id == crate::database_catalog::MASTER_ID => {
            bail!(SqlError::new(
                12104,
                2,
                "ALTER DATABASE CURRENT failed because 'master' is a system database. System databases cannot be altered by using the CURRENT keyword. Use the database name to alter a system database."
            ));
        }
        None => session.database.name.clone(),
    };
    let [on, rest @ ..] = request.snapshot_isolation.as_slice() else {
        bail!("ALTER DATABASE requires ALLOW_SNAPSHOT_ISOLATION");
    };
    if rest.iter().any(|value| value != on) {
        // Normally reported for the whole batch by `conflicting_values`.
        bail!(conflicting_values_error());
    }
    let catalog = session.database.catalog().clone();
    let (alias, display) = catalog
        .alter_target(&session.db, &name)
        .map_err(|error| match error.downcast::<SqlError>() {
            Ok(error) => failed(error),
            Err(error) => error,
        })?;
    if !request.settings.is_empty() {
        return Err(failed(SqlError::new(
            5082,
            1,
            format!(
                "Cannot change the versioning state on database \"{display}\" together with another database state."
            ),
        )));
    }
    if request.termination != Termination::Wait {
        return Err(failed(SqlError::new(
            5083,
            1,
            "The termination option is not supported when making versioning state changes.",
        )));
    }
    let mut tokens = Vec::new();
    session.rowcount = 0;
    if alias == catalog.primary() {
        crate::tds::diagnostic_utf16(
            &mut tokens,
            crate::tds::DiagnosticKind::Information,
            0,
            1,
            3987,
            &"SNAPSHOT ISOLATION is always enabled in this database."
                .encode_utf16()
                .collect::<Vec<_>>(),
        );
        return Ok(Execution::statement(tokens, None, 215));
    }
    let (target, transition) = if *on {
        (SnapshotIsolation::On, SnapshotIsolation::ToOn)
    } else {
        (SnapshotIsolation::Off, SnapshotIsolation::ToOff)
    };
    // Changes of one database run one at a time.
    let _reservation = reserve(session, &alias)?;
    let previous = catalog.snapshot_isolation(&session.db, &alias)?;
    if previous != target {
        // Only transactions that began before the transition are waited for.
        let cutoff = SEQUENCE.fetch_add(1, Ordering::SeqCst);
        catalog.set_snapshot_isolation(&session.db, &alias, transition)?;
        let db = session.db.try_clone()?;
        let publish = || catalog.set_snapshot_isolation(&db, &alias, target);
        if let Err(error) = wait_for_transactions(session, &alias, *on, cutoff, publish) {
            catalog.set_snapshot_isolation(&session.db, &alias, previous)?;
            return Err(error);
        }
    }
    Ok(Execution::statement(tokens, None, 215))
}

/// Databases (server catalog address and alias) whose
/// ALLOW_SNAPSHOT_ISOLATION is being changed.
static CHANGING: LazyLock<Mutex<std::collections::HashSet<(usize, String)>>> =
    LazyLock::new(Default::default);

struct Reservation(usize, String);

impl Drop for Reservation {
    fn drop(&mut self) {
        CHANGING
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&(self.0, std::mem::take(&mut self.1)));
    }
}

/// Wait until no other ALLOW_SNAPSHOT_ISOLATION change of the database
/// runs, then hold it until the returned reservation is dropped.
fn reserve(session: &mut Session, alias: &str) -> Result<Reservation> {
    let key = (catalog_id(session), alias.to_owned());
    while !CHANGING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(key.clone())
    {
        pause(session)?;
    }
    Ok(Reservation(key.0, key.1))
}

/// Wait briefly, failing when the request is cancelled or the session is
/// being terminated.
fn pause(session: &mut Session) -> Result<()> {
    let attention = session.read_cancel.clone();
    let poll = Duration::from_millis(10);
    match session.process.wait(poll, poll, &|| {
        attention
            .as_ref()
            .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::SeqCst))
    }) {
        crate::sessions::Wake::Elapsed => Ok(()),
        crate::sessions::Wake::Cancelled => Err(crate::read_cancellation::CancelledRead {
            metadata: Vec::new(),
        }
        .into()),
        crate::sessions::Wake::Terminated => {
            bail!("ALTER DATABASE ended because the session is being terminated")
        }
    }
}

/// A transaction open in a session, as ALLOW_SNAPSHOT_ISOLATION changes
/// see it.
struct Active {
    /// When it began, from [`SEQUENCE`].
    sequence: u64,
    /// The server's database catalog (its address), so that servers in one
    /// process stay apart.
    catalog: usize,
    /// The session was at SNAPSHOT when the transaction began.
    snapshot: bool,
    /// The databases (DuckDB catalog aliases) the transaction used.
    databases: HashMap<String, Usage>,
}

#[derive(Default)]
struct Usage {
    wrote: bool,
    /// It accessed data as a SNAPSHOT transaction.
    read: bool,
}

/// Open transactions by session token. A session outside a transaction
/// has an entry from its first write until its batch ends, for its
/// autocommit transactions.
static ACTIVE: LazyLock<Mutex<HashMap<u64, Active>>> = LazyLock::new(Default::default);

/// Orders transactions and the ALLOW_SNAPSHOT_ISOLATION changes.
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn active_transactions() -> std::sync::MutexGuard<'static, HashMap<u64, Active>> {
    ACTIVE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn catalog_id(session: &Session) -> usize {
    std::sync::Arc::as_ptr(session.database.catalog()) as usize
}

fn new_active(session: &Session) -> Active {
    let mut databases = HashMap::new();
    databases.insert(session.database.alias().to_owned(), Usage::default());
    Active {
        sequence: SEQUENCE.fetch_add(1, Ordering::SeqCst),
        catalog: catalog_id(session),
        snapshot: active(session),
        databases,
    }
}

/// The outermost transaction of a session began.
pub(super) fn begin(session: &Session) {
    active_transactions().insert(session.ext.token, new_active(session));
}

/// The session's transaction ended, its batch ended outside a transaction,
/// or the session ended.
pub(super) fn end(session: &Session) {
    active_transactions().remove(&session.ext.token);
}

fn usage(session: &Session, alias: &str, update: impl FnOnce(&mut Usage)) {
    if let Some(active) = active_transactions().get_mut(&session.ext.token) {
        update(active.databases.entry(alias.to_owned()).or_default());
    }
}

/// Note a write of the session's transaction, or of its autocommit
/// statement, to a database.
pub(super) fn track_write(session: &mut Session, statement: &Statement) -> Result<()> {
    let targets = match statement {
        Statement::CreateTable(table) => vec![table.name.clone()],
        Statement::AlterTable(table) => vec![table.name.clone()],
        Statement::Drop { names, .. } => names.clone(),
        Statement::Truncate(truncate) => truncate
            .table_names
            .iter()
            .map(|table| table.name.clone())
            .collect(),
        Statement::CreateIndex(index) => vec![index.table_name.clone()],
        Statement::CreateView(view) => vec![view.name.clone()],
        Statement::Query(query) => match query.body.as_ref() {
            SetExpr::Select(select) => select
                .into
                .iter()
                .flat_map(|into| &into.targets)
                .filter_map(|target| match target {
                    sqlparser::ast::Expr::Identifier(ident) => {
                        Some(ObjectName::from(vec![ident.clone()]))
                    }
                    sqlparser::ast::Expr::CompoundIdentifier(idents) => {
                        Some(ObjectName::from(idents.clone()))
                    }
                    _ => None,
                })
                .collect(),
            _ => write_target(statement).into_iter().collect(),
        },
        _ => write_target(statement).into_iter().collect(),
    };
    for target in targets {
        let idents: Vec<&Ident> = target.0.iter().filter_map(|part| part.as_ident()).collect();
        let alias = match idents.as_slice() {
            [.., table] if temporary(table) => continue,
            [database, _, _] => match session
                .database
                .catalog()
                .resolve(&session.db, &database.value)?
            {
                Some(alias) => alias,
                None => continue,
            },
            _ => session.database.alias().to_owned(),
        };
        if session.transactions == 0 {
            active_transactions()
                .entry(session.ext.token)
                .or_insert_with(|| new_active(session));
        }
        usage(session, &alias, |usage| usage.wrote = true);
    }
    Ok(())
}

/// A temporary table or table variable, also once renamed to its backend
/// table (see temp_tables/storage.rs). A delimited `[@name]` is an ordinary
/// table.
fn temporary(ident: &Ident) -> bool {
    ident.value.starts_with('#')
        || (ident.quote_style.is_none() && ident.value.starts_with('@'))
        || ["__msduck_temp_", "__msduck_tv_", "__msduck_global_"]
            .iter()
            .any(|prefix| ident.value.starts_with(prefix))
}

/// Wait until the other sessions' transactions that an
/// ALLOW_SNAPSHOT_ISOLATION change waits for have ended (ON waits for
/// transactions that wrote to the database, OFF also for SNAPSHOT
/// transactions that used it), then `publish` the final state. The last
/// check and `publish` hold the transaction table, so no waited-for
/// transaction can start writing in between.
fn wait_for_transactions(
    session: &mut Session,
    alias: &str,
    on: bool,
    cutoff: u64,
    publish: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let catalog = catalog_id(session);
    let token = session.ext.token;
    loop {
        let transactions = active_transactions();
        let blocked = transactions.iter().any(|(other, active)| {
            *other != token
                && active.sequence < cutoff
                && active.catalog == catalog
                && active
                    .databases
                    .get(alias)
                    .is_some_and(|usage| usage.wrote || (!on && active.snapshot))
        });
        if !blocked {
            return publish();
        }
        drop(transactions);
        pause(session)?;
    }
}

fn conflicting_values_error() -> SqlError {
    SqlError::new(
        5062,
        1,
        "The option \"ALLOW_SNAPSHOT_ISOLATION\" conflicts with another requested option. The options cannot both be requested at the same time.",
    )
}

/// SQL Server rejects a batch containing ALTER DATABASE SET
/// ALLOW_SNAPSHOT_ISOLATION ON together with OFF before running any of it.
pub(super) fn conflicting_values(statements: &[Statement]) -> Option<SqlError> {
    struct Conflicting;
    impl Visitor for Conflicting {
        type Break = ();
        fn pre_visit_statement(&mut self, statement: &Statement) -> ControlFlow<()> {
            match msduck_sql::dialect::alter_database::request(statement) {
                Some(request)
                    if request
                        .snapshot_isolation
                        .windows(2)
                        .any(|pair| pair[0] != pair[1]) =>
                {
                    ControlFlow::Break(())
                }
                _ => ControlFlow::Continue(()),
            }
        }
    }
    statements
        .iter()
        .any(|statement| statement.visit(&mut Conflicting).is_break())
        .then(conflicting_values_error)
}

/// Whether the session's statements run as SNAPSHOT transactions.
pub(super) fn active(session: &Session) -> bool {
    session.ext.transactions.isolation == SNAPSHOT
}

/// Fail with 3952 when a SNAPSHOT statement reads or writes a table or view
/// of a database that does not allow snapshot isolation. Catalog views,
/// temporary tables, table variables, common table expressions and missing
/// objects (which fail with their own errors) are not data access here.
pub(super) fn check_access(session: &mut Session, statement: &Statement) -> Result<()> {
    if !matches!(
        statement,
        Statement::Query(_)
            | Statement::Insert(_)
            | Statement::Update(_)
            | Statement::Delete(_)
            | Statement::Merge(_)
    ) {
        return Ok(());
    }
    let catalog = session.database.catalog().clone();
    let mut states: HashMap<String, SnapshotIsolation> = HashMap::new();
    for name in relations(statement) {
        let parts: Vec<&str> = name
            .0
            .iter()
            .filter_map(|part| part.as_ident().map(|ident| ident.value.as_str()))
            .collect();
        if parts.len() != name.0.len() || parts.is_empty() || parts.len() > 3 {
            continue;
        }
        let object = parts[parts.len() - 1];
        if name
            .0
            .last()
            .and_then(|part| part.as_ident())
            .is_some_and(temporary)
        {
            continue;
        }
        let schema = if parts.len() >= 2 {
            parts[parts.len() - 2]
        } else {
            "dbo"
        };
        if schema.is_empty()
            || schema.eq_ignore_ascii_case("sys")
            || schema.eq_ignore_ascii_case("INFORMATION_SCHEMA")
        {
            continue;
        }
        let alias = if parts.len() == 3 {
            match catalog.resolve(&session.db, parts[0])? {
                Some(alias) => alias,
                None => continue,
            }
        } else {
            session.database.alias().to_owned()
        };
        let state = match states.get(&alias) {
            Some(state) => *state,
            None => {
                let state = catalog.snapshot_isolation(&session.db, &alias)?;
                states.insert(alias.clone(), state);
                state
            }
        };
        let read = || {
            active_transactions()
                .get(&session.ext.token)
                .and_then(|active| active.databases.get(&alias))
                .is_some_and(|usage| usage.read)
        };
        // A transaction that read under SNAPSHOT before OFF began continues.
        if state == SnapshotIsolation::On || (state == SnapshotIsolation::ToOff && read()) {
            usage(session, &alias, |usage| usage.read = true);
            continue;
        }
        if !exists(session, &alias, schema, object)? {
            continue;
        }
        if session.transactions > 0 {
            session.transaction_doomed = true;
        }
        let database = catalog.display_name(&alias);
        bail!(match state {
            SnapshotIsolation::ToOn => SqlError::new(
                3956,
                1,
                format!(
                    "Snapshot isolation transaction failed to start in database '{database}' because the ALTER DATABASE command which enables snapshot isolation for this database has not finished yet. The database is in transition to pending ON state. You must wait until the ALTER DATABASE Command completes successfully."
                ),
            ),
            SnapshotIsolation::ToOff => SqlError::new(
                3954,
                1,
                format!(
                    "Snapshot isolation transaction failed to start in database '{database}' because the ALTER DATABASE command that disallows snapshot isolation had started before this transaction began. The database is in transition to OFF state. You will either need to change the isolation level of the transaction or re-enable the snapshot isolation in the database."
                ),
            ),
            _ => SqlError::new(
                3952,
                1,
                format!(
                    "Snapshot isolation transaction failed accessing database '{database}' because snapshot isolation is not allowed in this database. Use ALTER DATABASE to allow snapshot isolation."
                ),
            ),
        });
    }
    Ok(())
}

/// Every table name a statement reads or writes, excluding table-valued
/// function calls, an UPDATE or DELETE target that names the alias of a
/// FROM relation (`UPDATE x ... FROM t AS x`), and one-part names of the
/// statement's common table expressions (anywhere in the statement, so a
/// CTE also hides a table of the same name outside its own scope).
fn relations(statement: &Statement) -> Vec<ObjectName> {
    struct Relations {
        names: Vec<ObjectName>,
        functions: Vec<ObjectName>,
        ctes: Vec<String>,
    }
    impl Visitor for Relations {
        type Break = ();
        fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<()> {
            if let Some(with) = &query.with {
                self.ctes.extend(
                    with.cte_tables
                        .iter()
                        .map(|cte| cte.alias.name.value.to_lowercase()),
                );
            }
            ControlFlow::Continue(())
        }
        fn pre_visit_relation(&mut self, relation: &ObjectName) -> ControlFlow<()> {
            self.names.push(relation.clone());
            ControlFlow::Continue(())
        }
        fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<()> {
            if let TableFactor::Table {
                name,
                args: Some(_),
                ..
            } = factor
            {
                self.functions.push(name.clone());
            }
            ControlFlow::Continue(())
        }
    }
    let mut visitor = Relations {
        names: Vec::new(),
        functions: Vec::new(),
        ctes: Vec::new(),
    };
    let _ = statement.visit(&mut visitor);
    let Relations {
        mut names,
        functions,
        ctes,
    } = visitor;
    // The target is visited before its FROM relations; only that one
    // occurrence is the alias.
    if let Some(placeholder) = alias_target(statement)
        && let Some(index) = names.iter().position(|name| *name == placeholder)
    {
        names.remove(index);
    }
    names
        .into_iter()
        .filter(|name| !functions.contains(name))
        .filter(|name| match name.0.as_slice() {
            [part] => part
                .as_ident()
                .is_none_or(|ident| !ctes.contains(&ident.value.to_lowercase())),
            _ => true,
        })
        .collect()
}

/// Whether a table or view exists, matching names case-insensitively.
fn exists(session: &Session, alias: &str, schema: &str, object: &str) -> Result<bool> {
    Ok(session.db.query_row(
        "SELECT count(*) > 0 FROM (
             SELECT database_name, schema_name, table_name AS name FROM duckdb_tables()
             UNION ALL
             SELECT database_name, schema_name, view_name FROM duckdb_views() WHERE NOT internal
         ) WHERE database_name = ? AND lower(schema_name) = lower(?) AND lower(name) = lower(?)",
        [alias, schema, object],
        |row| row.get(0),
    )?)
}

/// Run an INSERT, UPDATE, DELETE or MERGE of a SNAPSHOT transaction through
/// the ordinary path, reporting DuckDB's write-write conflict as 3960.
pub(super) fn write(
    session: &mut Session,
    statement: &Statement,
    parameters: &mut HashMap<String, Parameter>,
) -> Result<Option<Execution>> {
    let Some(target) = write_target(statement) else {
        return Ok(None);
    };
    // Not `reenter`: statements nested in this one, such as trigger bodies,
    // still need their access checks, savepoint images and conflicts.
    session.ext.transactions.resumed = Some(statement.clone());
    let result = session.execute(statement.clone(), parameters);
    session.ext.transactions.resumed = None;
    let error = match result {
        Ok(execution) => return Ok(Some(execution)),
        Err(error) => error,
    };
    if !is_conflict(&error) {
        return Err(error);
    }
    let parts: Vec<&str> = target
        .0
        .iter()
        .filter_map(|part| part.as_ident().map(|ident| ident.value.as_str()))
        .collect();
    let (database, schema, table) = match parts.as_slice() {
        [database, schema, table] => (
            session
                .database
                .catalog()
                .resolve(&session.db, database)?
                .map(|alias| session.database.catalog().display_name(&alias))
                .unwrap_or_else(|| (*database).to_owned()),
            *schema,
            *table,
        ),
        [schema, table] => (session.database.name.clone(), *schema, *table),
        [table] => (session.database.name.clone(), "dbo", *table),
        _ => return Err(error),
    };
    if session.transactions > 0 {
        // DuckDB has aborted its transaction. The SQL Server transaction
        // stays open but doomed until the batch ends or the CATCH block
        // rolls it back, so its reads run in a fresh, empty transaction.
        session.db.execute_batch("ROLLBACK; BEGIN TRANSACTION")?;
        session.transaction_doomed = true;
    }
    Err(SqlError::new(
        3960,
        2,
        format!(
            "Snapshot isolation transaction aborted due to update conflict. You cannot use snapshot isolation to access table '{schema}.{table}' directly or indirectly in database '{database}' to update, delete, or insert the row that has been modified or deleted by another transaction. Retry the transaction or change the isolation level for the update/delete statement."
        ),
    )
    .into())
}

/// The table an INSERT, UPDATE, DELETE or MERGE writes, also after a WITH
/// clause.
fn write_target(statement: &Statement) -> Option<ObjectName> {
    let factor = match statement {
        Statement::Query(query) => {
            return match query.body.as_ref() {
                SetExpr::Insert(inner)
                | SetExpr::Update(inner)
                | SetExpr::Delete(inner)
                | SetExpr::Merge(inner) => write_target(inner),
                _ => None,
            };
        }
        Statement::Insert(insert) => {
            return match &insert.table {
                TableObject::TableName(name) => Some(name.clone()),
                _ => None,
            };
        }
        Statement::Update(update) => {
            // UPDATE alias SET ... FROM table AS alias names the alias.
            if let TableFactor::Table { name, .. } = &update.table.relation
                && let Some(
                    UpdateTableFromKind::BeforeSet(tables) | UpdateTableFromKind::AfterSet(tables),
                ) = &update.from
            {
                return Some(aliased(name, tables).unwrap_or_else(|| name.clone()));
            }
            &update.table.relation
        }
        Statement::Delete(delete) => {
            // DELETE alias FROM table AS alias names the alias first.
            if let Some((name, tables)) = delete_target(delete) {
                return Some(aliased(&name, tables).unwrap_or(name));
            }
            let (FromTable::WithFromKeyword(tables) | FromTable::WithoutKeyword(tables)) =
                &delete.from;
            &tables.first()?.relation
        }
        Statement::Merge(merge) => &merge.table,
        _ => return None,
    };
    match factor {
        TableFactor::Table { name, .. } => Some(name.clone()),
        _ => None,
    }
}

/// An UPDATE or DELETE target that is the alias of one of its FROM
/// relations, also after a WITH clause.
fn alias_target(statement: &Statement) -> Option<ObjectName> {
    match statement {
        Statement::Query(query) => match query.body.as_ref() {
            SetExpr::Update(inner) | SetExpr::Delete(inner) => alias_target(inner),
            _ => None,
        },
        Statement::Update(update) => {
            let TableFactor::Table {
                name, alias: None, ..
            } = &update.table.relation
            else {
                return None;
            };
            let (UpdateTableFromKind::BeforeSet(tables) | UpdateTableFromKind::AfterSet(tables)) =
                update.from.as_ref()?;
            aliased(name, tables).map(|_| name.clone())
        }
        Statement::Delete(delete) => {
            let (name, tables) = delete_target(delete)?;
            aliased(&name, tables).map(|_| name)
        }
        _ => None,
    }
}

/// A DELETE's syntactic target and the relations its alias may name: the
/// FROM relations of `DELETE x FROM t AS x`, or the USING relations of the
/// canonical `DELETE FROM x USING t AS x` the batch parser produces.
fn delete_target(delete: &Delete) -> Option<(ObjectName, &[TableWithJoins])> {
    let (FromTable::WithFromKeyword(tables) | FromTable::WithoutKeyword(tables)) = &delete.from;
    if let [name] = delete.tables.as_slice() {
        return Some((name.clone(), tables));
    }
    let [
        TableWithJoins {
            relation: TableFactor::Table {
                name, alias: None, ..
            },
            joins,
        },
    ] = tables.as_slice()
    else {
        return None;
    };
    if !joins.is_empty() {
        return None;
    }
    Some((name.clone(), delete.using.as_deref().unwrap_or_default()))
}

/// The table a one-part DML target names when it is the alias of a FROM
/// relation.
fn aliased(target: &ObjectName, tables: &[TableWithJoins]) -> Option<ObjectName> {
    let [part] = target.0.as_slice() else {
        return None;
    };
    let alias = &part.as_ident()?.value;
    tables
        .iter()
        .flat_map(|table| {
            std::iter::once(&table.relation).chain(table.joins.iter().map(|join| &join.relation))
        })
        .find_map(|factor| match factor {
            TableFactor::Table {
                name,
                alias: Some(a),
                ..
            } if a.name.value.eq_ignore_ascii_case(alias) => Some(name.clone()),
            _ => None,
        })
}

/// DuckDB's write-write conflict between concurrent transactions.
fn is_conflict(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        let message = cause.to_string();
        message.contains("TransactionContext Error") && message.contains("onflict")
    })
}
