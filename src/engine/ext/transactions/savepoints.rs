//! SAVE TRANSACTION and ROLLBACK TRANSACTION to a savepoint.
//!
//! DuckDB has no savepoints, so a savepoint keeps before-images. The first
//! time a statement after the newest savepoint writes a table, the table's
//! rows are copied into a temporary table that belongs to that savepoint.
//! Rolling back to a savepoint restores each table from its earliest copy
//! taken at or after that savepoint, changing only the rows that differ:
//!
//! - With a primary key, rows are matched by key: rows added later are
//!   deleted, changed rows are updated back and removed rows reinserted.
//! - Without one, rows are compared as a multiset of whole rows.
//!
//! Deletes run in reverse order of first write and inserts in order, so
//! foreign keys between restored tables stay satisfied. Identity and
//! sequence values are not restored, as in SQL Server. Table variables are
//! not affected by a rollback in SQL Server and are not copied.
//!
//! Statements that change the schema (CREATE, ALTER, DROP, SELECT INTO)
//! after a savepoint fail explicitly,
//! because their effects cannot be undone without rolling back the whole
//! DuckDB transaction.
//!
//! Savepoint names compare with the database collation
//! (SQL_Latin1_General_CP1_CI_AS: case insensitive, accent sensitive) and
//! ignore trailing blanks. Rolling back to a savepoint consumes it and every
//! later savepoint; with duplicate names the newest one is used
//! (reference/savepoint.json).
use super::super::super::{Execution, Session};
use anyhow::{Context, Result};
use msduck_core::diagnostic::SqlError;
use sqlparser::ast::{
    Expr, FromTable, ObjectName, OutputClause, SetExpr, Statement, TableFactor, TableObject,
    TableWithJoins, UpdateTableFromKind,
};

#[derive(Default)]
pub(in crate::engine) struct Stack {
    savepoints: Vec<Savepoint>,
    /// Numbers copies, in the order tables were first written.
    next: u64,
}

struct Savepoint {
    name: String,
    copies: Vec<Copy>,
}

/// A table's rows when it was first written after a savepoint.
struct Copy {
    table: Table,
    /// The temporary table holding the rows.
    name: String,
    order: u64,
}

#[derive(Clone, Debug, PartialEq)]
struct Table {
    object_id: i32,
    schema: String,
    name: String,
}

impl Table {
    fn sql(&self) -> String {
        format!("{}.{}", quote(&self.schema), quote(&self.name))
    }
}

fn quote(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

/// Savepoint names compare like the database collation, ignoring trailing
/// blanks.
fn same_name(a: &str, b: &str) -> bool {
    a.trim_end_matches(' ').to_lowercase() == b.trim_end_matches(' ').to_lowercase()
}

fn stack(session: &mut Session) -> &mut Stack {
    &mut session.ext.transactions.savepoints
}

fn no_transaction() -> anyhow::Error {
    SqlError::new(
        628,
        0,
        "Cannot issue SAVE TRANSACTION when there is no active transaction.",
    )
    .into()
}

fn doomed_write() -> anyhow::Error {
    SqlError::new(
        3930,
        1,
        "The current transaction cannot be committed and cannot support operations that write to the log file. Roll back the transaction.",
    )
    .into()
}

fn doomed_rollback() -> anyhow::Error {
    SqlError::new(
        3931,
        1,
        "The current transaction cannot be committed and cannot be rolled back to a savepoint. Roll back the entire transaction.",
    )
    .into()
}

/// SQL `SAVE TRANSACTION`. A NULL variable name succeeds without a
/// savepoint, as captured.
pub(super) fn save_statement(session: &mut Session, name: Option<String>) -> Result<()> {
    if session.transactions == 0 {
        return Err(no_transaction());
    }
    if session.transaction_doomed {
        return Err(doomed_write());
    }
    if let Some(name) = name {
        stack(session).savepoints.push(Savepoint {
            name,
            copies: Vec::new(),
        });
    }
    Ok(())
}

/// A transaction-manager savepoint request. Unlike SQL, an empty name is
/// 3977 and rolls back the whole transaction, and a name longer than 32
/// characters is 103 with state 30 instead of being truncated.
pub(super) fn save_request(session: &mut Session, name: &str) -> Result<Vec<u8>> {
    if session.transactions == 0 {
        return Err(no_transaction());
    }
    if name.is_empty() {
        let tokens = session.rollback_transaction("")?;
        return Err(super::super::Partial {
            tokens,
            error: SqlError::new(
                3977,
                1,
                "The savepoint name cannot be NULL. The batch has been aborted.",
            )
            .into(),
        }
        .into());
    }
    if name.chars().count() > msduck_sql::dialect::ext::transactions::NAME_LIMIT {
        return Err(msduck_sql::dialect::ext::transactions::name_too_long(name, 30).into());
    }
    save_statement(session, Some(name.to_owned()))?;
    Ok(Vec::new())
}

fn find(session: &mut Session, name: &str) -> Option<usize> {
    stack(session)
        .savepoints
        .iter()
        .rposition(|savepoint| same_name(&savepoint.name, name))
}

/// SQL `ROLLBACK TRANSACTION name`: a savepoint of that name wins over an
/// outer transaction of the same name. `None` leaves the request to the
/// engine (the outer transaction's name, 3903 and 6401).
pub(super) fn rollback_statement(session: &mut Session, name: &str) -> Result<Option<Execution>> {
    if session.transactions == 0 {
        return Ok(None);
    }
    let Some(index) = find(session, name) else {
        return Ok(None);
    };
    if session.transaction_doomed {
        return Err(doomed_rollback());
    }
    restore(session, index)?;
    Ok(Some(Execution::statement(Vec::new(), None, 0)))
}

/// A transaction-manager rollback naming something other than the outer
/// transaction.
pub(super) fn rollback_request(session: &mut Session, name: &str) -> Option<Result<Vec<u8>>> {
    let index = find(session, name)?;
    if session.transaction_doomed {
        return Some(Err(doomed_rollback()));
    }
    Some(restore(session, index).map(|()| Vec::new()))
}

/// The outermost transaction ended: its savepoints and copies go with it.
pub(super) fn release_all(session: &mut Session) {
    let savepoints = std::mem::take(&mut stack(session).savepoints);
    drop_copies(session, savepoints);
}

fn drop_copies(session: &Session, savepoints: Vec<Savepoint>) {
    for copy in savepoints
        .into_iter()
        .flat_map(|savepoint| savepoint.copies)
    {
        // Copies made in a rolled-back DuckDB transaction no longer exist.
        let _ = session.db.execute_batch(&format!(
            "DROP TABLE IF EXISTS temp.main.{}",
            quote(&copy.name)
        ));
    }
}

/// Before a statement runs inside a transaction that has savepoints: copy
/// the tables it writes, or refuse a statement whose effects cannot be
/// rolled back to a savepoint.
pub(super) fn before_statement(session: &mut Session, statement: &Statement) -> Result<()> {
    if session.transactions == 0 || stack(session).savepoints.is_empty() {
        return Ok(());
    }
    match statement {
        Statement::Query(query) => match query.body.as_ref() {
            // `WITH ... INSERT/UPDATE/DELETE/MERGE` writes like the inner
            // statement.
            SetExpr::Insert(inner)
            | SetExpr::Update(inner)
            | SetExpr::Delete(inner)
            | SetExpr::Merge(inner) => before_statement(session, inner),
            body if selects_into(body) => Err(unsupported("SELECT INTO")),
            _ => Ok(()),
        },
        Statement::Insert(_)
        | Statement::Update(_)
        | Statement::Delete(_)
        | Statement::Merge(_)
        | Statement::Truncate(_) => {
            // Resolve names the way execution will: a name qualified with
            // the current database refers to it. Other qualification
            // errors surface from the statement itself.
            let mut qualified = statement.clone();
            if session.qualify_databases(&mut qualified).is_err() {
                return Ok(());
            }
            let mut tables = Vec::new();
            for name in targets(&qualified) {
                if let Some(table) = resolve(session, &name)? {
                    tables.push(table);
                }
            }
            // Referential actions (CASCADE, SET NULL, SET DEFAULT) write the
            // referencing tables too, so copy them first, transitively.
            let mut index = 0;
            while index < tables.len() {
                for table in referencing(session, &tables[index])? {
                    if !tables.contains(&table) {
                        tables.push(table);
                    }
                }
                index += 1;
            }
            for table in tables {
                copy_table(session, table)?;
            }
            Ok(())
        }
        Statement::Set(_)
        | Statement::Declare { .. }
        | Statement::Print(_)
        | Statement::Raise(_)
        | Statement::RaisError { .. }
        | Statement::Throw(_)
        | Statement::Return(_)
        | Statement::StartTransaction { .. }
        | Statement::Commit { .. }
        | Statement::Rollback { .. }
        | Statement::Execute { .. }
        | Statement::Use(_)
        | Statement::If(_)
        | Statement::While(_) => Ok(()),
        other => Err(unsupported(&statement_kind(other))),
    }
}

fn statement_kind(statement: &Statement) -> String {
    statement
        .to_string()
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ")
}

fn unsupported(what: &str) -> anyhow::Error {
    SqlError::new(
        40515,
        1,
        format!(
            "unsupported {what} after SAVE TRANSACTION: msduck can roll back only INSERT, UPDATE, DELETE, MERGE and TRUNCATE TABLE to a savepoint"
        ),
    )
    .into()
}

fn selects_into(body: &SetExpr) -> bool {
    match body {
        SetExpr::Select(select) => select.into.is_some(),
        SetExpr::Query(query) => selects_into(&query.body),
        SetExpr::SetOperation { left, right, .. } => selects_into(left) || selects_into(right),
        _ => false,
    }
}

/// The tables a write statement modifies, including OUTPUT INTO targets.
/// An UPDATE or DELETE target that names an alias of its FROM clause
/// resolves to that table.
fn targets(statement: &Statement) -> Vec<ObjectName> {
    fn relations(from: &[TableWithJoins]) -> Vec<(&ObjectName, Option<&str>)> {
        from.iter()
            .flat_map(|table| {
                std::iter::once(&table.relation)
                    .chain(table.joins.iter().map(|join| &join.relation))
            })
            .filter_map(|factor| match factor {
                TableFactor::Table { name, alias, .. } => {
                    Some((name, alias.as_ref().map(|alias| alias.name.value.as_str())))
                }
                _ => None,
            })
            .collect()
    }
    fn through_alias(name: &ObjectName, from: &[TableWithJoins]) -> ObjectName {
        if let [part] = name.0.as_slice()
            && let Some(ident) = part.as_ident()
            && let Some((table, _)) = relations(from).into_iter().find(|(_, alias)| {
                alias.is_some_and(|alias| alias.eq_ignore_ascii_case(&ident.value))
            })
        {
            return table.clone();
        }
        name.clone()
    }
    fn output_into(output: &Option<OutputClause>, names: &mut Vec<ObjectName>) {
        if let Some(OutputClause::Output {
            into_table: Some(into),
            ..
        }) = output
        {
            for target in &into.targets {
                match target {
                    Expr::Identifier(ident) => names.push(ObjectName::from(vec![ident.clone()])),
                    Expr::CompoundIdentifier(idents) => {
                        names.push(ObjectName::from(idents.clone()))
                    }
                    // `INTO table(column, ...)` parses as a call.
                    Expr::Function(function) => names.push(function.name.clone()),
                    _ => {}
                }
            }
        }
    }
    let mut names = Vec::new();
    match statement {
        Statement::Insert(insert) => {
            if let TableObject::TableName(name) = &insert.table {
                names.push(name.clone());
            }
            output_into(&insert.output, &mut names);
        }
        Statement::Update(update) => {
            let from = match &update.from {
                Some(
                    UpdateTableFromKind::BeforeSet(from) | UpdateTableFromKind::AfterSet(from),
                ) => from.as_slice(),
                None => &[],
            };
            if let TableFactor::Table { name, .. } = &update.table.relation {
                names.push(through_alias(name, from));
            }
            output_into(&update.output, &mut names);
        }
        Statement::Delete(delete) => {
            let from = match &delete.from {
                FromTable::WithFromKeyword(from) | FromTable::WithoutKeyword(from) => {
                    from.as_slice()
                }
            };
            if delete.tables.is_empty() {
                if let Some((name, _)) = relations(from).first() {
                    names.push((*name).clone());
                }
            } else {
                let mut sources = from.to_vec();
                sources.extend(delete.using.iter().flatten().cloned());
                names.extend(
                    delete
                        .tables
                        .iter()
                        .map(|name| through_alias(name, &sources)),
                );
            }
            output_into(&delete.output, &mut names);
        }
        Statement::Merge(merge) => {
            if let TableFactor::Table { name, .. } = &merge.table {
                names.push(name.clone());
            }
            output_into(&merge.output, &mut names);
        }
        Statement::Truncate(truncate) => {
            names.extend(
                truncate
                    .table_names
                    .iter()
                    .map(|target| target.name.clone()),
            );
        }
        _ => {}
    }
    names
}

/// The table a name refers to. `None` for a table variable, which a
/// rollback does not affect, and for a missing object, which the statement
/// itself reports.
fn resolve(session: &Session, name: &ObjectName) -> Result<Option<Table>> {
    let last = name
        .0
        .last()
        .and_then(|part| part.as_ident())
        .map(|ident| ident.value.as_str())
        .unwrap_or_default();
    // `#temp` tables and table variables are renamed to backend tables by
    // the temp_tables feature, which executes the renamed statement; it is
    // copied then.
    if last.starts_with('@') || last.starts_with('#') {
        return Ok(None);
    }
    let found = session
        .db
        .query_row(
            "SELECT o.object_id, s.name, o.name, rtrim(o.type) FROM sys.objects o JOIN sys.schemas s ON o.schema_id = s.schema_id WHERE o.object_id = __msduck_object_id(?, NULL)",
            [name.to_string()],
            |row| {
                Ok((
                    row.get::<_, i32>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        );
    match found {
        // A table variable's backend table (see the temp_tables feature)
        // keeps its rows across a rollback, as in SQL Server.
        Ok((_, _, name, _)) if name.starts_with("__msduck_tv_") => Ok(None),
        Ok((object_id, schema, name, kind)) if kind == "U" => Ok(Some(Table {
            object_id,
            schema,
            name,
        })),
        Ok((_, _, _, kind)) => Err(unsupported(&format!(
            "write through an object of type {kind}"
        ))),
        Err(duckdb::Error::QueryReturnedNoRows) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Tables whose foreign keys to `table` have a delete or update action.
fn referencing(session: &Session, table: &Table) -> Result<Vec<Table>> {
    Ok(session
        .db
        .prepare(
            "SELECT DISTINCT o.object_id, s.name, o.name FROM sys.foreign_keys f JOIN sys.objects o ON o.object_id = f.parent_object_id JOIN sys.schemas s ON s.schema_id = o.schema_id WHERE f.referenced_object_id = ? AND (f.delete_referential_action <> 0 OR f.update_referential_action <> 0) ORDER BY o.object_id",
        )?
        .query_map([table.object_id], |row| {
            Ok(Table {
                object_id: row.get(0)?,
                schema: row.get(1)?,
                name: row.get(2)?,
            })
        })?
        .collect::<duckdb::Result<Vec<_>>>()?)
}

/// Copy `table` for the newest savepoint unless it already has a copy.
fn copy_table(session: &mut Session, table: Table) -> Result<()> {
    let token = session.ext.token;
    let savepoints = stack(session);
    let Some(newest) = savepoints.savepoints.last() else {
        return Ok(());
    };
    if newest.copies.iter().any(|copy| copy.table == table) {
        return Ok(());
    }
    let order = savepoints.next;
    savepoints.next += 1;
    let name = format!("__msduck_savepoint_{token}_{order}");
    session
        .db
        .execute_batch(&format!(
            "CREATE TEMP TABLE {} AS SELECT * FROM {}",
            quote(&name),
            table.sql()
        ))
        .with_context(|| format!("copying {} for SAVE TRANSACTION", table.sql()))?;
    stack(session)
        .savepoints
        .last_mut()
        .expect("newest savepoint exists")
        .copies
        .push(Copy { table, name, order });
    Ok(())
}

/// Roll back to savepoint `index`, consuming it and every later savepoint.
fn restore(session: &mut Session, index: usize) -> Result<()> {
    let mut plan: Vec<(&Table, &str, u64)> = Vec::new();
    for savepoint in &stack(session).savepoints[index..] {
        for copy in &savepoint.copies {
            if !plan.iter().any(|(table, _, _)| **table == copy.table) {
                plan.push((&copy.table, &copy.name, copy.order));
            }
        }
    }
    plan.sort_by_key(|(_, _, order)| *order);
    let plan = plan
        .into_iter()
        .map(|(table, copy, _)| (table.clone(), copy.to_owned()))
        .collect::<Vec<_>>();
    let restored = (|| -> Result<()> {
        let shapes = plan
            .iter()
            .map(|(table, _)| shape(session, table))
            .collect::<Result<Vec<_>>>()?;
        for ((table, copy), shape) in plan.iter().zip(&shapes).rev() {
            session.db.execute_batch(&shape.delete(table, copy))?;
        }
        for ((table, copy), shape) in plan.iter().zip(&shapes) {
            if let Some(update) = shape.update(table, copy) {
                session.db.execute_batch(&update)?;
            }
        }
        for ((table, copy), shape) in plan.iter().zip(&shapes) {
            session.db.execute_batch(&shape.insert(table, copy))?;
        }
        Ok(())
    })();
    if let Err(error) = restored {
        // A failed restore can leave a partial state behind; only a full
        // rollback is safe from here.
        session.transaction_doomed = true;
        return Err(error.context("ROLLBACK TRANSACTION to a savepoint could not restore the saved rows; roll back the transaction"));
    }
    let consumed = stack(session).savepoints.split_off(index);
    drop_copies(session, consumed);
    Ok(())
}

/// Aliases of the live table and its copy in restore statements.
const LIVE: &str = "__msduck_live";
const SAVED: &str = "__msduck_saved";

/// The columns a restore writes and the primary key that matches rows.
struct Shape {
    /// Stored (not computed) columns, in table order.
    columns: Vec<String>,
    key: Vec<String>,
}

fn shape(session: &Session, table: &Table) -> Result<Shape> {
    let computed = session
        .db
        .prepare("SELECT name FROM sys.columns WHERE object_id = ? AND is_computed")?
        .query_map([table.object_id], |row| row.get::<_, String>(0))?
        .collect::<duckdb::Result<Vec<_>>>()?;
    let columns = session
        .db
        .prepare(
            "SELECT column_name FROM duckdb_columns() WHERE database_name = current_database() AND schema_name = ? AND table_name = ? ORDER BY column_index",
        )?
        .query_map([&table.schema, &table.name], |row| row.get::<_, String>(0))?
        .collect::<duckdb::Result<Vec<_>>>()?
        .into_iter()
        .filter(|column| !computed.iter().any(|c| c.eq_ignore_ascii_case(column)))
        .collect::<Vec<_>>();
    let key = session
        .db
        .prepare(
            "SELECT unnest(constraint_column_names) FROM duckdb_constraints() WHERE database_name = current_database() AND schema_name = ? AND table_name = ? AND constraint_type = 'PRIMARY KEY'",
        )?
        .query_map([&table.schema, &table.name], |row| row.get::<_, String>(0))?
        .collect::<duckdb::Result<Vec<_>>>()?;
    anyhow::ensure!(!columns.is_empty(), "{} has no stored columns", table.sql());
    let key = if key
        .iter()
        .all(|k| columns.iter().any(|c| c.eq_ignore_ascii_case(k)))
    {
        key
    } else {
        Vec::new()
    };
    Ok(Shape { columns, key })
}

impl Shape {
    fn list(&self, prefix: &str) -> String {
        self.columns
            .iter()
            .map(|column| format!("{prefix}{}", quote(column)))
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn key_match(&self, left: &str, right: &str) -> String {
        self.key
            .iter()
            .map(|k| format!("{left}.{q} = {right}.{q}", q = quote(k)))
            .collect::<Vec<_>>()
            .join(" AND ")
    }

    /// A whole stored row as one comparable value, whatever its column
    /// names (a column may be named like the alias) or collations.
    fn row(&self, alias: &str) -> String {
        let values = self
            .columns
            .iter()
            .map(|column| format!("to_json({alias}.{})", quote(column)))
            .collect::<Vec<_>>()
            .join(", ");
        format!("to_json([{values}])")
    }

    /// Delete the rows that are not in the copy: by key, or the surplus of
    /// each distinct whole row.
    fn delete(&self, table: &Table, copy: &str) -> String {
        let target = table.sql();
        let copy = format!("temp.main.{}", quote(copy));
        if self.key.is_empty() {
            format!(
                "DELETE FROM {target} WHERE rowid IN (SELECT __msduck_r FROM (SELECT {LIVE}.rowid AS __msduck_r, {live_row} AS __msduck_k, row_number() OVER (PARTITION BY {live_row} ORDER BY {LIVE}.rowid) AS __msduck_n FROM {target} {LIVE}) l LEFT JOIN (SELECT {saved_row} AS __msduck_k, count(*) AS __msduck_m FROM {copy} {SAVED} GROUP BY 1) s USING (__msduck_k) WHERE l.__msduck_n > coalesce(s.__msduck_m, 0))",
                live_row = self.row(LIVE),
                saved_row = self.row(SAVED),
            )
        } else {
            format!(
                "DELETE FROM {target} WHERE rowid IN (SELECT {LIVE}.rowid FROM {target} {LIVE} WHERE NOT EXISTS (SELECT 1 FROM {copy} {SAVED} WHERE {}))",
                self.key_match(SAVED, LIVE)
            )
        }
    }

    /// Update rows whose key survived but whose values changed.
    fn update(&self, table: &Table, copy: &str) -> Option<String> {
        let values: Vec<&String> = self
            .columns
            .iter()
            .filter(|column| !self.key.iter().any(|k| k.eq_ignore_ascii_case(column)))
            .collect();
        if self.key.is_empty() || values.is_empty() {
            return None;
        }
        let target = table.sql();
        let copy = format!("temp.main.{}", quote(copy));
        let assignments = values
            .iter()
            .map(|column| format!("{q} = __msduck_d.{q}", q = quote(column)))
            .collect::<Vec<_>>()
            .join(", ");
        Some(format!(
            "UPDATE {target} SET {assignments} FROM (SELECT {LIVE}.rowid AS __msduck_rid, {SAVED}.* FROM {target} {LIVE} JOIN {copy} {SAVED} ON {} WHERE {} IS DISTINCT FROM {}) __msduck_d WHERE {target}.rowid = __msduck_d.__msduck_rid",
            self.key_match(SAVED, LIVE),
            self.row(LIVE),
            self.row(SAVED),
        ))
    }

    /// Insert the copied rows the table no longer has.
    fn insert(&self, table: &Table, copy: &str) -> String {
        let target = table.sql();
        let copy = format!("temp.main.{}", quote(copy));
        let columns = self.list("");
        if self.key.is_empty() {
            format!(
                "INSERT INTO {target} ({columns}) SELECT {} FROM (SELECT {SAVED}.*, {saved_row} AS __msduck_k, row_number() OVER (PARTITION BY {saved_row}) AS __msduck_n FROM {copy} {SAVED}) s LEFT JOIN (SELECT {live_row} AS __msduck_k, count(*) AS __msduck_m FROM {target} {LIVE} GROUP BY 1) l USING (__msduck_k) WHERE s.__msduck_n > coalesce(l.__msduck_m, 0)",
                self.list("s."),
                live_row = self.row(LIVE),
                saved_row = self.row(SAVED),
            )
        } else {
            format!(
                "INSERT INTO {target} ({columns}) SELECT {} FROM {copy} {SAVED} WHERE NOT EXISTS (SELECT 1 FROM {target} {LIVE} WHERE {})",
                self.list(&format!("{SAVED}.")),
                self.key_match(LIVE, SAVED)
            )
        }
    }
}
