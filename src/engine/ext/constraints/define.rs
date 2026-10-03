//! Constraint definitions: CREATE TABLE constraints, and ALTER TABLE ADD,
//! DROP, CHECK and NOCHECK CONSTRAINT.
//!
//! CHECK and FOREIGN KEY constraints are never created in DuckDB: msduck
//! stores and enforces them (see `enforce`). PRIMARY KEY and UNIQUE stay
//! native; this module records their names and adds or removes them.
use super::catalog::{self, Column, Constraint, New, Table, Type};
use super::errors::{self, Verb, error};
use super::{Transaction, enforce, rebuild, translate};
use crate::engine::{Execution, Parameter, Session, ext};
use anyhow::{Result, bail};
use msduck_sql::dialect::ext::constraints::{
    Action, AddItem, Alter, DropItem, ForeignKey, Kind, Referential, Targets,
};
use sqlparser::ast::*;
use std::collections::{HashMap, HashSet};
use std::ops::ControlFlow;

/// A constraint to create, with the column it was declared on, if any.
#[derive(Clone, Debug)]
pub(crate) struct Item {
    pub name: Option<Ident>,
    pub column: Option<Ident>,
    pub kind: Kind,
}

fn same(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

fn truncated(text: &str, length: usize) -> String {
    text.chars().take(length).collect()
}

/// SQL Server's generated name for an unnamed constraint.
fn system_name(kind: &Kind, table: &str, column: Option<&str>, id: i32) -> String {
    use msduck_sql::dialect::ext::catalog::declarations::constraint_name;
    match kind {
        Kind::Check(_) => constraint_name("CK", table, column, id),
        Kind::ForeignKey(key) => {
            let column = key.columns.first().map_or("", |c| c.value.as_str());
            constraint_name("FK", table, Some(column), id)
        }
        Kind::Default { column, .. } => constraint_name("DF", table, Some(&column.value), id),
        Kind::PrimaryKey(_) | Kind::Unique(_) => {
            let prefix = if matches!(kind, Kind::PrimaryKey(_)) {
                "PK"
            } else {
                "UQ"
            };
            let hash = table.bytes().fold(0x811c9dc5u32, |h, b| {
                (h ^ b as u32).wrapping_mul(0x01000193)
            });
            format!(
                "{prefix}__{}__{hash:08X}{:08X}",
                truncated(table, 8),
                id as u32
            )
        }
    }
}

/// Items from a CREATE TABLE statement, removing CHECK and FOREIGN KEY
/// definitions from it (msduck enforces them), keeping keys native.
pub(crate) fn take_create_items(create: &mut CreateTable) -> Result<Vec<Item>> {
    let mut items = Vec::new();
    for column in &mut create.columns {
        let owner = column.name.clone();
        let mut kept = Vec::new();
        for option in column.options.drain(..) {
            let item = match &option.option {
                ColumnOption::Check(check) => Some(Item {
                    name: option.name.clone().or(check.name.clone()),
                    column: Some(owner.clone()),
                    kind: Kind::Check(*check.expr.clone()),
                }),
                ColumnOption::ForeignKey(key) => Some(Item {
                    name: option.name.clone().or(key.name.clone()),
                    column: Some(owner.clone()),
                    kind: Kind::ForeignKey(foreign(key, vec![owner.clone()])?),
                }),
                ColumnOption::PrimaryKey(key) => {
                    items.push(Item {
                        name: option.name.clone().or(key.name.clone()),
                        column: Some(owner.clone()),
                        kind: Kind::PrimaryKey(vec![owner.clone()]),
                    });
                    None
                }
                ColumnOption::Unique(key) => {
                    items.push(Item {
                        name: option.name.clone().or(key.name.clone()),
                        column: Some(owner.clone()),
                        kind: Kind::Unique(vec![owner.clone()]),
                    });
                    None
                }
                _ => None,
            };
            match item {
                Some(item) => items.push(item),
                None => kept.push(option),
            }
        }
        column.options = kept;
    }
    let mut kept = Vec::new();
    for constraint in create.constraints.drain(..) {
        match &constraint {
            TableConstraint::Check(check) => items.push(Item {
                name: check.name.clone(),
                column: None,
                kind: Kind::Check(*check.expr.clone()),
            }),
            TableConstraint::ForeignKey(key) => items.push(Item {
                name: key.name.clone(),
                column: None,
                kind: Kind::ForeignKey(foreign(key, key.columns.clone())?),
            }),
            TableConstraint::PrimaryKey(key) => {
                items.push(Item {
                    name: key.name.clone(),
                    column: None,
                    kind: Kind::PrimaryKey(index_columns(&key.columns)?),
                });
                kept.push(constraint);
            }
            TableConstraint::Unique(key) => {
                items.push(Item {
                    name: key.name.clone(),
                    column: None,
                    kind: Kind::Unique(index_columns(&key.columns)?),
                });
                kept.push(constraint);
            }
            _ => kept.push(constraint),
        }
    }
    create.constraints = kept;
    Ok(items)
}

fn index_columns(columns: &[IndexColumn]) -> Result<Vec<Ident>> {
    columns
        .iter()
        .map(|column| match &column.column.expr {
            Expr::Identifier(ident) => Ok(ident.clone()),
            other => bail!("unsupported key column {other}"),
        })
        .collect()
}

fn foreign(key: &ForeignKeyConstraint, columns: Vec<Ident>) -> Result<ForeignKey> {
    let action = |action| {
        Referential::from_ast(action).map_err(|message| anyhow::Error::new(error(102, 1, message)))
    };
    Ok(ForeignKey {
        columns,
        table: key.foreign_table.clone(),
        referred: key.referred_columns.clone(),
        on_delete: action(key.on_delete)?,
        on_update: action(key.on_update)?,
    })
}

/// A constraint with its identity, checked against the catalog.
#[derive(Clone, Debug)]
struct Planned {
    id: i32,
    name: String,
    system_named: bool,
    item: Item,
}

/// Allocate identities and names, rejecting duplicates (8168) and names
/// already used by objects of the schema (2714).
fn plan(
    session: &Session,
    table: &Table,
    items: Vec<Item>,
    new_table: bool,
) -> Result<Vec<Planned>> {
    let mut seen = HashSet::new();
    for item in &items {
        if let Some(name) = &item.name
            && !seen.insert(name.value.to_lowercase())
        {
            return Err(errors::duplicate_in_statement(&name.value));
        }
    }
    for item in &items {
        if let Some(name) = &item.name
            && (catalog::object_exists(&session.db, &table.schema, &name.value)?
                || (new_table && same(&name.value, &table.name)))
        {
            return Err(errors::duplicate_object(&name.value));
        }
    }
    let mut planned = Vec::new();
    for item in items {
        let id = catalog::next_object_id(&session.db)?;
        let (name, system_named) = match &item.name {
            Some(name) => (name.value.clone(), false),
            None => (
                system_name(
                    &item.kind,
                    &table.name,
                    item.column.as_ref().map(|c| c.value.as_str()),
                    id,
                ),
                true,
            ),
        };
        planned.push(Planned {
            id,
            name,
            system_named,
            item,
        });
    }
    Ok(planned)
}

/// Validate a CHECK expression over the table's columns; returns the
/// distinct columns it references, as declared.
///
/// Lowering and binding the expression natively then finds names that are
/// not columns (207).
fn check_columns(columns: &[String], expr: &Expr) -> Result<Vec<String>> {
    let mut referenced: Vec<String> = Vec::new();
    let mut failure = None;
    let _ = visit_expressions(expr, |expr| {
        let name = match expr {
            Expr::Subquery(_) | Expr::Exists { .. } | Expr::InSubquery { .. } => {
                failure = Some(errors::subquery());
                return ControlFlow::Break(());
            }
            Expr::Identifier(ident)
                if ident.value.starts_with('@') && ident.quote_style.is_none() =>
            {
                failure = Some(errors::variable(&ident.value));
                return ControlFlow::Break(());
            }
            Expr::Identifier(ident) => ident,
            Expr::CompoundIdentifier(parts) => match parts.last() {
                Some(ident) => ident,
                None => return ControlFlow::Continue(()),
            },
            _ => return ControlFlow::Continue(()),
        };
        // Other names, such as the datepart of DATEDIFF, are not columns;
        // `probe_check` reports names that are neither.
        if let Some(column) = columns.iter().find(|column| same(column, &name.value))
            && !referenced.iter().any(|c| same(c, column))
        {
            referenced.push(column.clone());
        }
        ControlFlow::Continue(())
    });
    if let Some(failure) = failure {
        return Err(failure);
    }
    let mut written = expr.clone();
    msduck_sql::dialect::ext::constraints::delimit_expr(&mut written);
    let probe = format!("CREATE TABLE __msduck_check_probe (CHECK ({written}))");
    let statements = msduck_sql::batch::parse(&probe)?;
    for statement in &statements {
        if let Err(error) = msduck_sql::predicate::validate(statement) {
            let mut diagnostic = errors::error(4145, 1, error.to_string());
            diagnostic.severity = 15;
            return Err(diagnostic.into());
        }
    }
    Ok(referenced)
}

/// Bind the lowered CHECK expression against the table: unknown columns
/// fail with 207, like SQL Server.
fn probe_check(session: &Session, table: &Table, expr: &Expr) -> Result<()> {
    let native = translate::check(session, table, expr)?;
    let sql = format!("SELECT 1 FROM {} WHERE NOT ({native}) LIMIT 0", table.sql());
    if let Err(error) = session.db.prepare(&sql) {
        let message = error.to_string();
        if let Some(rest) = message.split("Referenced column \"").nth(1)
            && let Some(name) = rest.split('"').next()
        {
            return Err(errors::invalid_column(name));
        }
        return Err(error.into());
    }
    Ok(())
}

/// SQL Server refuses constraints over computed columns that are not
/// persisted (1764).
fn persisted(table: &Table, columns: &[&Column], usage: &str, state: u8) -> Result<()> {
    if let Some(column) = columns.iter().find(|c| c.computed && !c.persisted) {
        return Err(errors::not_created_in_state(
            error(
                1764,
                1,
                format!(
                    "Computed Column '{}' in table '{}' is invalid for use in '{usage}' because it is not persisted.",
                    column.name, table.name
                ),
            ),
            state,
        ));
    }
    Ok(())
}

fn find_column<'a>(columns: &'a [Column], name: &str) -> Option<&'a Column> {
    columns.iter().find(|column| same(&column.name, name))
}

/// A foreign key resolved against the catalog.
#[derive(Clone, Debug)]
struct Foreign {
    columns: Vec<String>,
    referenced: Table,
    referred: Vec<String>,
    on_delete: Referential,
    on_update: Referential,
}

/// The referenced table of a foreign key, or 1767.
fn referenced_table(session: &Session, key: &ForeignKey, name: &str) -> Result<Table> {
    let missing = || {
        errors::not_created(error(
            1767,
            0,
            format!(
                "Foreign key '{name}' references invalid table '{}'.",
                key.table
            ),
        ))
    };
    let Some((schema, table)) = catalog::split_name(&key.table, &session.database.name) else {
        return Err(missing());
    };
    if table.starts_with('#') {
        return Err(missing());
    }
    catalog::table(&session.db, &schema, &table)?.ok_or_else(missing)
}

/// Validate a foreign key of an existing `child` table (1769, 8139, 1773,
/// 1770, 1776, 1778, 1753, 1761, 1762).
fn resolve_foreign(
    session: &Session,
    child: &Table,
    key: &ForeignKey,
    name: &str,
    creating: Option<&[(bool, Vec<String>)]>,
) -> Result<Foreign> {
    let child_columns = catalog::columns(&session.db, child)?;
    let mut columns = Vec::new();
    for column in &key.columns {
        let Some(found) = find_column(&child_columns, &column.value) else {
            return Err(errors::not_created(error(
                1769,
                1,
                format!(
                    "Foreign key '{name}' references invalid column '{}' in referencing table '{}'.",
                    column.value, child.name
                ),
            )));
        };
        columns.push(found.clone());
    }
    persisted(
        child,
        &columns.iter().collect::<Vec<_>>(),
        "FOREIGN KEY CONSTRAINT",
        1,
    )?;
    // A computed referencing column cannot be updated by an action.
    if let Some(column) = columns.iter().find(|c| c.computed) {
        if key.on_update != Referential::NoAction {
            return Err(errors::not_created(error(
                1715,
                1,
                format!(
                    "Foreign key '{name}' creation failed. Only NO ACTION referential update action is allowed for referencing computed column '{}'.",
                    column.name
                ),
            )));
        }
        if !matches!(key.on_delete, Referential::NoAction | Referential::Cascade) {
            return Err(errors::not_created(error(
                1765,
                1,
                format!(
                    "Foreign key '{name}' creation failed. Only NO ACTION and CASCADE referential delete actions are allowed for referencing computed column '{}'.",
                    column.name
                ),
            )));
        }
    }
    let referenced = referenced_table(session, key, name)?;
    let parent_columns = catalog::columns(&session.db, &referenced)?;
    let mut keys = catalog::native_keys(&session.db, &referenced)?;
    // A CREATE TABLE can reference its own keys before they are recorded.
    if let Some(own_keys) = creating.filter(|_| referenced.id == child.id) {
        keys.extend(own_keys.iter().cloned());
        keys.sort_by_key(|(primary, _)| !primary);
    }
    let referred_names: Vec<String> = if key.referred.is_empty() {
        match keys.iter().find(|(primary, _)| *primary) {
            Some((_, columns)) => columns.clone(),
            None => {
                return Err(errors::not_created(error(
                    1773,
                    0,
                    format!(
                        "Foreign key '{name}' has implicit reference to object '{}' which does not have a primary key defined on it.",
                        key.table
                    ),
                )));
            }
        }
    } else {
        key.referred.iter().map(|c| c.value.clone()).collect()
    };
    if referred_names.len() != columns.len() {
        return Err(error(
            8139,
            0,
            format!(
                "Number of referencing columns in foreign key differs from number of referenced columns, table '{}'.",
                child.name
            ),
        )
        .into());
    }
    let mut referred = Vec::new();
    for column in &referred_names {
        let Some(found) = find_column(&parent_columns, column) else {
            return Err(errors::not_created(error(
                1770,
                0,
                format!(
                    "Foreign key '{name}' references invalid column '{column}' in referenced table '{}'.",
                    referenced.name
                ),
            )));
        };
        referred.push(found.clone());
    }
    let wanted: HashSet<String> = referred.iter().map(|c| c.name.to_lowercase()).collect();
    let matched = keys.iter().any(|(_, key)| {
        key.len() == wanted.len() && key.iter().all(|c| wanted.contains(&c.to_lowercase()))
    });
    // The keys feature takes keys over STRUCT storage out of CREATE TABLE
    // and records them after the table exists, so a self-reference to such
    // columns cannot be matched yet.
    let pending = creating.is_some()
        && referenced.id == child.id
        && structured(session, &referenced, &referred_names)?;
    if !matched && !pending {
        return Err(errors::not_created(error(
            1776,
            0,
            format!(
                "There are no primary or candidate keys in the referenced table '{}' that match the referencing column list in the foreign key '{name}'.",
                referenced.name
            ),
        )));
    }
    for (parent, child_column) in referred.iter().zip(&columns) {
        if parent.user_type != child_column.user_type {
            return Err(errors::not_created(error(
                1778,
                0,
                format!(
                    "Column '{}.{}' is not the same data type as referencing column '{}.{}' in foreign key '{name}'.",
                    referenced.name, parent.name, child.name, child_column.name
                ),
            )));
        }
        if (parent.max_length, parent.precision, parent.scale)
            != (
                child_column.max_length,
                child_column.precision,
                child_column.scale,
            )
        {
            return Err(errors::not_created(error(
                1753,
                0,
                format!(
                    "Column '{}.{}' is not the same length or scale as referencing column '{}.{}' in foreign key '{name}'. Columns participating in a foreign key relationship must be defined with the same length and scale.",
                    referenced.name, parent.name, child.name, child_column.name
                ),
            )));
        }
    }
    let actions = [key.on_delete, key.on_update];
    if actions.contains(&Referential::SetNull) && columns.iter().any(|c| !c.nullable) {
        return Err(errors::not_created(error(
            1761,
            0,
            format!(
                "Cannot create the foreign key \"{name}\" with the SET NULL referential action, because one or more referencing columns are not nullable."
            ),
        )));
    }
    if actions.contains(&Referential::SetDefault) {
        let defaults = catalog::native_defaults(&session.db, child)?;
        if columns
            .iter()
            .any(|c| !c.nullable && !defaults.contains_key(&c.name.to_lowercase()))
        {
            return Err(errors::not_created(error(
                1762,
                0,
                format!(
                    "Cannot create the foreign key \"{name}\" with the SET DEFAULT referential action, because one or more referencing not-nullable columns lack a default constraint."
                ),
            )));
        }
    }
    Ok(Foreign {
        columns: columns.into_iter().map(|c| c.name).collect(),
        referenced,
        referred: referred.into_iter().map(|c| c.name).collect(),
        on_delete: key.on_delete,
        on_update: key.on_update,
    })
}

/// SQL Server rejects a cascading foreign key that would let one delete or
/// update reach a table along two paths, or reach a table already on its
/// path (1785).
///
/// A walk starts with a DELETE or an UPDATE of any table. A deleted row's ON
/// DELETE CASCADE deletes the referencing rows, so the walk continues with
/// their table's ON DELETE actions. ON DELETE SET NULL or SET DEFAULT updates
/// the referencing rows instead, so the walk continues with their table's ON
/// UPDATE actions only. Every ON UPDATE action updates the referencing rows,
/// whichever of their columns the next key covers. Each arrival counts,
/// whether it deletes or updates.
fn cascade_paths(
    existing: &[Constraint],
    new: &[(i32, i32, Referential, Referential)],
    added: (&str, &str),
) -> Result<()> {
    if new.iter().all(|(_, _, delete, update)| {
        *delete == Referential::NoAction && *update == Referential::NoAction
    }) {
        return Ok(());
    }
    // (referenced, referencing, ON DELETE, ON UPDATE)
    let edges: Vec<(i32, i32, Referential, Referential)> = existing
        .iter()
        .filter(|c| c.kind == Type::Foreign)
        .filter_map(|c| {
            c.referenced
                .as_ref()
                .map(|r| (r.id, c.table.id, c.on_delete, c.on_update))
        })
        .chain(new.iter().copied())
        .filter(|(_, _, delete, update)| {
            *delete != Referential::NoAction || *update != Referential::NoAction
        })
        .collect();
    let nodes: HashSet<i32> = edges.iter().flat_map(|(a, b, _, _)| [*a, *b]).collect();
    for start in &nodes {
        for deleting in [true, false] {
            // Count arrivals along every path; a cycle or a second arrival fails.
            let mut stack = vec![(*start, deleting, vec![*start])];
            let mut reached: HashMap<i32, usize> = HashMap::new();
            while let Some((node, deleting, path)) = stack.pop() {
                for (parent, child, on_delete, on_update) in &edges {
                    if parent != &node {
                        continue;
                    }
                    let next = match (deleting, on_delete, on_update) {
                        (true, Referential::NoAction, _) | (false, _, Referential::NoAction) => {
                            continue;
                        }
                        (true, Referential::Cascade, _) => true,
                        _ => false,
                    };
                    if path.contains(child) {
                        return Err(cycles(added));
                    }
                    let count = reached.entry(*child).or_default();
                    *count += 1;
                    if *count > 1 {
                        return Err(cycles(added));
                    }
                    let mut onward = path.clone();
                    onward.push(*child);
                    stack.push((*child, next, onward));
                }
            }
        }
    }
    Ok(())
}

fn cycles((name, table): (&str, &str)) -> anyhow::Error {
    errors::not_created(error(
        1785,
        0,
        format!(
            "Introducing FOREIGN KEY constraint '{name}' on table '{table}' may cause cycles or multiple cascade paths. Specify ON DELETE NO ACTION or ON UPDATE NO ACTION, or modify other FOREIGN KEY constraints."
        ),
    ))
}

/// Duplicate key values of `columns`, formatted as SQL Server shows them.
fn duplicate(session: &Session, table: &Table, columns: &[String]) -> Result<Option<String>> {
    let list = columns
        .iter()
        .map(|c| catalog::quote(c))
        .collect::<Vec<_>>()
        .join(",");
    let shown = columns
        .iter()
        .map(|c| format!("coalesce(CAST({} AS VARCHAR),'<NULL>')", catalog::quote(c)))
        .collect::<Vec<_>>()
        .join("||', '||");
    let sql = format!(
        "SELECT {shown} FROM {} GROUP BY {list} HAVING count(*)>1 LIMIT 1",
        table.sql()
    );
    let mut statement = session.db.prepare(&sql)?;
    let mut rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    Ok(rows.next().transpose()?)
}

/// Whether every one of `columns` has STRUCT storage (NVARCHAR, DATETIME2
/// and similar), which only keys-managed indexes can key.
fn structured(session: &Session, table: &Table, columns: &[String]) -> Result<bool> {
    let mut statement = session.db.prepare(
        "SELECT column_name FROM duckdb_columns() WHERE database_name=current_database()
         AND schema_name=? AND table_name=? AND data_type LIKE 'STRUCT%'",
    )?;
    let names = statement
        .query_map([&table.schema, &table.name], |row| row.get::<_, String>(0))?
        .collect::<duckdb::Result<Vec<_>>>()?;
    Ok(!columns.is_empty()
        && columns
            .iter()
            .all(|c| names.iter().any(|n| n.eq_ignore_ascii_case(c))))
}

/// A key added by ALTER TABLE is a native DuckDB constraint, which cannot
/// cover the STRUCT storage of NVARCHAR, DATETIME2 and similar columns.
fn indexable(session: &Session, table: &Table, columns: &[String]) -> Result<()> {
    let mut statement = session.db.prepare(
        "SELECT column_name,data_type FROM duckdb_columns() WHERE database_name=current_database()
         AND schema_name=? AND table_name=? AND data_type LIKE 'STRUCT%'",
    )?;
    let structured = statement
        .query_map([&table.schema, &table.name], |row| row.get::<_, String>(0))?
        .collect::<duckdb::Result<Vec<_>>>()?;
    if let Some(column) = columns
        .iter()
        .find(|c| structured.iter().any(|s| s.eq_ignore_ascii_case(c)))
    {
        bail!(
            "unsupported ALTER TABLE ADD PRIMARY KEY or UNIQUE on column {column}: its storage needs a key from CREATE TABLE or CREATE UNIQUE INDEX"
        );
    }
    Ok(())
}

/// Stored form of one planned constraint.
struct Stored {
    planned: Planned,
    columns: Vec<String>,
    column: Option<String>,
    definition: Option<String>,
    foreign: Option<Foreign>,
}

impl Stored {
    fn kind(&self) -> Type {
        match &self.planned.item.kind {
            Kind::Check(_) => Type::Check,
            Kind::ForeignKey(_) => Type::Foreign,
            Kind::PrimaryKey(_) => Type::Primary,
            Kind::Unique(_) | Kind::Default { .. } => Type::Unique,
        }
    }
    fn save(&self, session: &Session, table: &Table, untrusted: bool) -> Result<()> {
        // The keys feature records key constraints.
        if matches!(self.kind(), Type::Primary | Type::Unique) {
            return catalog::insert_key(
                &session.db,
                table,
                &self.planned.name,
                self.kind() == Type::Primary,
                &self.columns,
            );
        }
        let empty = Vec::new();
        catalog::insert(
            &session.db,
            &New {
                id: self.planned.id,
                table,
                name: &self.planned.name,
                system_named: self.planned.system_named,
                kind: self.kind(),
                columns: &self.columns,
                column: self.column.as_deref(),
                definition: self.definition.as_deref(),
                referenced: self.foreign.as_ref().map(|f| &f.referenced),
                referenced_columns: self.foreign.as_ref().map_or(&empty, |f| &f.referred),
                on_delete: self
                    .foreign
                    .as_ref()
                    .map_or(Referential::NoAction, |f| f.on_delete),
                on_update: self
                    .foreign
                    .as_ref()
                    .map_or(Referential::NoAction, |f| f.on_update),
                untrusted,
            },
        )
    }
    /// The catalog form, for validation queries.
    fn constraint(&self, table: &Table) -> Constraint {
        Constraint {
            id: self.planned.id,
            name: self.planned.name.clone(),
            kind: self.kind(),
            table: table.clone(),
            columns: self.columns.clone(),
            referenced: self.foreign.as_ref().map(|f| f.referenced.clone()),
            referenced_columns: self.foreign.as_ref().map_or(vec![], |f| f.referred.clone()),
            on_delete: self
                .foreign
                .as_ref()
                .map_or(Referential::NoAction, |f| f.on_delete),
            on_update: self
                .foreign
                .as_ref()
                .map_or(Referential::NoAction, |f| f.on_update),
            definition: self.definition.clone(),
            disabled: false,
            untrusted: false,
            column: self.column.clone(),
        }
    }
}

/// Resolve planned constraints against the (existing) table.
fn resolve(
    session: &Session,
    table: &Table,
    planned: Vec<Planned>,
    creating: Option<&[(bool, Vec<String>)]>,
) -> Result<Vec<Stored>> {
    let columns = catalog::columns(&session.db, table)?;
    let names: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();
    let mut stored = Vec::new();
    for planned in planned {
        let entry = match &planned.item.kind {
            Kind::Check(expr) => {
                let referenced = check_columns(&names, expr)?;
                let used: Vec<&Column> = referenced
                    .iter()
                    .filter_map(|name| find_column(&columns, name))
                    .collect();
                persisted(table, &used, "CHECK CONSTRAINT", 0)?;
                probe_check(session, table, expr)?;
                let column = match &planned.item.column {
                    Some(owner) => find_column(&columns, &owner.value).map(|c| c.name.clone()),
                    None if referenced.len() == 1 => Some(referenced[0].clone()),
                    None => None,
                };
                Stored {
                    columns: referenced,
                    column,
                    definition: Some({
                        let mut stored = expr.clone();
                        msduck_sql::dialect::ext::constraints::delimit_expr(&mut stored);
                        stored.to_string()
                    }),
                    foreign: None,
                    planned,
                }
            }
            Kind::ForeignKey(key) => {
                let foreign = resolve_foreign(session, table, key, &planned.name, creating)?;
                Stored {
                    columns: foreign.columns.clone(),
                    column: None,
                    definition: None,
                    foreign: Some(foreign),
                    planned,
                }
            }
            Kind::PrimaryKey(keys) | Kind::Unique(keys) => {
                let mut resolved = Vec::new();
                for key in keys {
                    let Some(column) = find_column(&columns, &key.value) else {
                        return Err(errors::not_created(error(
                            1911,
                            1,
                            format!(
                                "Column name '{}' does not exist in the target table or view.",
                                key.value
                            ),
                        )));
                    };
                    resolved.push(column.name.clone());
                }
                Stored {
                    columns: resolved,
                    column: None,
                    definition: None,
                    foreign: None,
                    planned,
                }
            }
            Kind::Default { .. } => Stored {
                columns: vec![],
                column: None,
                definition: None,
                foreign: None,
                planned,
            },
        };
        stored.push(entry);
    }
    Ok(stored)
}

fn check_cascades(session: &Session, table: &Table, stored: &[Stored]) -> Result<()> {
    let existing = catalog::load(&session.db)?;
    let mut new = Vec::new();
    for entry in stored {
        if let Some(foreign) = &entry.foreign {
            new.push((
                foreign.referenced.id,
                table.id,
                foreign.on_delete,
                foreign.on_update,
            ));
            cascade_paths(&existing, &new, (&entry.planned.name, &table.name))?;
        }
    }
    Ok(())
}

/// Reject `items` that are not valid for a new table before it exists: name
/// conflicts and missing referenced tables (all other checks need the table).
fn precheck_create(session: &Session, schema: &str, table: &str, items: &[Item]) -> Result<()> {
    // An existing table is reported by CREATE TABLE itself.
    if catalog::table(&session.db, schema, table)?.is_some() {
        return Ok(());
    }
    let mut seen = HashSet::new();
    for item in items {
        if let Some(name) = &item.name {
            if !seen.insert(name.value.to_lowercase()) {
                return Err(errors::duplicate_in_statement(&name.value));
            }
            if catalog::object_exists(&session.db, schema, &name.value)? || same(&name.value, table)
            {
                return Err(errors::duplicate_object(&name.value));
            }
        }
    }
    for item in items {
        if let Kind::ForeignKey(key) = &item.kind {
            let self_reference = catalog::split_name(&key.table, &session.database.name)
                .is_some_and(|(s, t)| same(&s, schema) && same(&t, table));
            if !self_reference {
                let name = item.name.as_ref().map_or_else(
                    || {
                        system_name(
                            &item.kind,
                            table,
                            item.column.as_ref().map(|c| c.value.as_str()),
                            0,
                        )
                    },
                    |n| n.value.clone(),
                );
                referenced_table(session, key, &name)?;
            }
        }
    }
    Ok(())
}

/// CREATE TABLE with constraints: create the table without CHECK and
/// FOREIGN KEY definitions, then store every constraint.
pub(crate) fn create_table(
    session: &mut Session,
    statement: &Statement,
    parameters: &mut HashMap<String, Parameter>,
) -> Result<Option<Execution>> {
    let Statement::CreateTable(original) = statement else {
        return Ok(None);
    };
    let has_constraints = original.constraints.iter().any(|c| {
        matches!(
            c,
            TableConstraint::Check(_)
                | TableConstraint::ForeignKey(_)
                | TableConstraint::PrimaryKey(_)
                | TableConstraint::Unique(_)
        )
    }) || original.columns.iter().any(|column| {
        column.options.iter().any(|o| {
            matches!(
                o.option,
                ColumnOption::Check(_)
                    | ColumnOption::ForeignKey(_)
                    | ColumnOption::PrimaryKey(_)
                    | ColumnOption::Unique(_)
            )
        })
    });
    if !has_constraints || original.query.is_some() {
        return Ok(None);
    }
    let Some((schema, name)) = catalog::split_name(&original.name, &session.database.name) else {
        return Ok(None);
    };
    if name.starts_with('#') || original.temporary {
        return temporary_table(session, original, parameters);
    }
    let mut create = original.clone();
    let items = take_create_items(&mut create)?;
    precheck_create(session, &schema, &name, &items)?;
    let transaction = Transaction::begin(session)?;
    let result = (|| -> Result<Execution> {
        let execution = ext::reenter(session, "constraints", |session| {
            session.execute(Statement::CreateTable(create), parameters)
        })?;
        let created = (|| -> Result<()> {
            let table = catalog::table(&session.db, &schema, &name)?.ok_or_else(|| {
                anyhow::anyhow!("created table {schema}.{name} is not in the catalog")
            })?;
            // The keys feature records this statement's key constraints,
            // after this returns; self-references may use them already.
            let own_keys: Vec<(bool, Vec<String>)> = items
                .iter()
                .filter_map(|item| match &item.kind {
                    Kind::PrimaryKey(columns) => Some((true, columns)),
                    Kind::Unique(columns) => Some((false, columns)),
                    _ => None,
                })
                .map(|(primary, columns)| {
                    (primary, columns.iter().map(|c| c.value.clone()).collect())
                })
                .collect();
            let items = items
                .into_iter()
                .filter(|item| matches!(item.kind, Kind::Check(_) | Kind::ForeignKey(_)))
                .collect();
            let planned = plan(session, &table, items, true)?;
            let stored = resolve(session, &table, planned, Some(&own_keys))?;
            check_cascades(session, &table, &stored)?;
            for entry in &stored {
                entry.save(session, &table, false)?;
            }
            Ok(())
        })();
        if let Err(error) = created {
            if !transaction.owned {
                // Undo the creation inside the caller's transaction.
                let drop = Statement::Drop {
                    object_type: ObjectType::Table,
                    if_exists: true,
                    names: vec![original.name.clone()],
                    cascade: false,
                    restrict: false,
                    purge: false,
                    temporary: false,
                    table: None,
                };
                let _ = ext::reenter(session, "constraints", |session| {
                    session.execute(drop, &mut HashMap::new())
                });
            }
            return Err(error);
        }
        Ok(execution)
    })();
    transaction.finish(session, result).map(Some)
}

/// SQL Server does not enforce foreign keys on temporary tables: it skips
/// them with warning 1756. CHECK constraints stay native.
fn temporary_table(
    session: &mut Session,
    original: &CreateTable,
    parameters: &mut HashMap<String, Parameter>,
) -> Result<Option<Execution>> {
    let mut create = original.clone();
    let table_name = original.name.to_string();
    let mut skipped = 0;
    for column in &mut create.columns {
        column.options.retain(|o| {
            let keep = !matches!(o.option, ColumnOption::ForeignKey(_));
            skipped += usize::from(!keep);
            keep
        });
    }
    create.constraints.retain(|c| {
        let keep = !matches!(c, TableConstraint::ForeignKey(_));
        skipped += usize::from(!keep);
        keep
    });
    if skipped == 0 {
        return Ok(None);
    }
    let mut execution = ext::reenter(session, "constraints", |session| {
        session.execute(Statement::CreateTable(create), parameters)
    })?;
    let mut tokens = Vec::new();
    for _ in 0..skipped {
        let message = format!(
            "Skipping FOREIGN KEY constraint '{table_name}' definition for temporary table. FOREIGN KEY constraints are not enforced on local or global temporary tables."
        );
        crate::tds::diagnostic_utf16(
            &mut tokens,
            crate::tds::DiagnosticKind::Information,
            10,
            0,
            1756,
            &message.encode_utf16().collect::<Vec<_>>(),
        );
    }
    tokens.extend(execution.tokens);
    execution.tokens = tokens;
    Ok(Some(execution))
}

fn resolve_table(session: &Session, name: &ObjectName) -> Result<Table> {
    let missing = || -> anyhow::Error {
        error(
            4902,
            1,
            format!("Cannot find the object \"{name}\" because it does not exist or you do not have permissions."),
        )
        .into()
    };
    let Some((schema, table)) = catalog::split_name(name, &session.database.name) else {
        return Err(missing());
    };
    catalog::table(&session.db, &schema, &table)?.ok_or_else(missing)
}

/// Execute a claimed ALTER TABLE statement.
pub(crate) fn alter(
    session: &mut Session,
    alter: Alter,
    parameters: &mut HashMap<String, Parameter>,
) -> Result<Execution> {
    let table = resolve_table(session, &alter.table)?;
    // DuckDB refuses DROP COLUMN on a table with indexes inside an open
    // transaction; the keys feature works around that only outside one. In
    // autocommit mode, drop the constraints first, then each column on its
    // own.
    if let Action::Drop(items) = &alter.action
        && session.transactions == 0
        && items
            .iter()
            .any(|item| matches!(item, DropItem::Column { .. }))
        && items
            .iter()
            .any(|item| matches!(item, DropItem::Constraint { .. }))
    {
        let (columns, constraints): (Vec<DropItem>, Vec<DropItem>) = items
            .iter()
            .cloned()
            .partition(|item| matches!(item, DropItem::Column { .. }));
        self::alter(
            session,
            Alter {
                table: alter.table.clone(),
                with_check: alter.with_check,
                action: Action::Drop(constraints),
            },
            parameters,
        )?;
        for item in columns {
            if let DropItem::Column { name, if_exists } = item {
                session.execute(drop_column(&alter.table, name, if_exists), parameters)?;
            }
        }
        return Ok(Execution::statement(vec![], None, 216));
    }
    let transaction = Transaction::begin(session)?;
    let mut wrote = false;
    let result = match alter.action {
        Action::Add(items) => add(
            session,
            &table,
            &alter.table,
            alter.with_check,
            items,
            parameters,
            &mut wrote,
        ),
        Action::Drop(items) => drop(session, &table, &alter.table, items, parameters, &mut wrote),
        Action::Toggle { enable, targets } => toggle(
            session,
            &table,
            alter.with_check,
            enable,
            targets,
            &mut wrote,
        ),
    }
    .map(|()| Execution::statement(vec![], None, 216));
    transaction.finish_or_abort(session, result, wrote)
}

/// ADD items: column definitions without their constraints, the
/// constraints, and named column defaults as (name, column, value).
type Split = (Vec<ColumnDef>, Vec<Item>, Vec<(Ident, Ident, Expr)>);

/// Split ADD items into column definitions (without their constraints),
/// constraints, and named column defaults.
fn split_add(items: Vec<AddItem>) -> Result<Split> {
    let mut columns = Vec::new();
    let mut constraints = Vec::new();
    let mut defaults = Vec::new();
    for item in items {
        match item {
            AddItem::Constraint(constraint) => {
                let constraint = *constraint;
                constraints.push(Item {
                    name: constraint.name,
                    column: None,
                    kind: constraint.kind,
                })
            }
            AddItem::Column(mut column) => {
                let owner = column.name.clone();
                let mut kept = Vec::new();
                for option in column.options.drain(..) {
                    match option.option {
                        ColumnOption::Check(check) => constraints.push(Item {
                            name: option.name.or(check.name),
                            column: Some(owner.clone()),
                            kind: Kind::Check(*check.expr),
                        }),
                        ColumnOption::ForeignKey(key) => constraints.push(Item {
                            name: option.name.clone().or(key.name.clone()),
                            column: Some(owner.clone()),
                            kind: Kind::ForeignKey(foreign(&key, vec![owner.clone()])?),
                        }),
                        ColumnOption::PrimaryKey(key) => constraints.push(Item {
                            name: option.name.or(key.name),
                            column: Some(owner.clone()),
                            kind: Kind::PrimaryKey(vec![owner.clone()]),
                        }),
                        ColumnOption::Unique(key) => constraints.push(Item {
                            name: option.name.or(key.name),
                            column: Some(owner.clone()),
                            kind: Kind::Unique(vec![owner.clone()]),
                        }),
                        ColumnOption::Default(value) if option.name.is_some() => {
                            defaults.push((
                                option.name.clone().unwrap(),
                                owner.clone(),
                                value.clone(),
                            ));
                            kept.push(ColumnOptionDef {
                                name: None,
                                option: ColumnOption::Default(value),
                            });
                        }
                        other => kept.push(ColumnOptionDef {
                            name: option.name,
                            option: other,
                        }),
                    }
                }
                column.options = kept;
                columns.push(column);
            }
        }
    }
    Ok((columns, constraints, defaults))
}

#[allow(clippy::too_many_arguments)]
fn add(
    session: &mut Session,
    table: &Table,
    written: &ObjectName,
    with_check: Option<bool>,
    items: Vec<AddItem>,
    parameters: &mut HashMap<String, Parameter>,
    wrote: &mut bool,
) -> Result<()> {
    let validate = with_check != Some(false);
    let (columns, constraints, defaults) = split_add(items)?;
    // Names of named column defaults share the constraint namespace.
    let mut seen = HashSet::new();
    let names = constraints
        .iter()
        .filter_map(|item| item.name.as_ref())
        .chain(defaults.iter().map(|(name, _, _)| name));
    for name in names {
        if !seen.insert(name.value.to_lowercase()) {
            return Err(errors::duplicate_in_statement(&name.value));
        }
        if catalog::object_exists(&session.db, &table.schema, &name.value)? {
            return Err(errors::duplicate_object(&name.value));
        }
    }
    if !columns.is_empty() {
        *wrote = true;
        let statement = Statement::AlterTable(AlterTable {
            name: written.clone(),
            if_exists: false,
            only: false,
            operations: columns
                .into_iter()
                .map(|column_def| AlterTableOperation::AddColumn {
                    column_keyword: false,
                    if_not_exists: false,
                    column_def,
                    column_position: None,
                })
                .collect(),
            location: None,
            on_cluster: None,
            table_type: None,
            end_token: helpers::attached_token::AttachedToken::empty(),
        });
        ext::reenter(session, "constraints", |session| {
            session.execute(statement, parameters)
        })?;
        for (name, column, value) in &defaults {
            catalog::record_default(&session.db, table, &column.value, &name.value, value)?;
        }
    }
    let planned = plan(session, table, constraints, false)?;
    let stored = resolve(session, table, planned, None)?;
    check_cascades(session, table, &stored)?;
    // Validate everything before changing anything.
    let database = session.database.name.clone();
    let mut primary = catalog::native_keys(&session.db, table)?
        .iter()
        .any(|(primary, _)| *primary);
    let mut defaults_planned = Vec::new();
    for entry in &stored {
        match &entry.planned.item.kind {
            Kind::Check(_) | Kind::ForeignKey(_) => {
                if validate {
                    let constraint = entry.constraint(table);
                    if enforce::violates(session, &constraint)?.is_some() {
                        return Err(match constraint.kind {
                            Type::Check => {
                                errors::check_conflict(Verb::AlterTable, &constraint, &database)
                            }
                            _ => errors::foreign_conflict(Verb::AlterTable, &constraint, &database),
                        }
                        .into());
                    }
                }
            }
            Kind::Default { value, column, .. } => {
                defaults_planned.push(check_default(session, table, value, column)?);
            }
            Kind::PrimaryKey(_) => {
                if primary {
                    return Err(errors::second_primary_key(&table.name));
                }
                primary = true;
                let columns = catalog::columns(&session.db, table)?;
                if entry
                    .columns
                    .iter()
                    .any(|c| find_column(&columns, c).is_some_and(|c| c.nullable))
                {
                    return Err(errors::nullable_key(&table.name));
                }
                if let Some(values) = duplicate(session, table, &entry.columns)? {
                    return Err(errors::duplicate_key(table, &entry.planned.name, &values));
                }
            }
            Kind::Unique(_) => {
                if let Some(values) = duplicate(session, table, &entry.columns)? {
                    return Err(errors::duplicate_key(table, &entry.planned.name, &values));
                }
            }
        }
        if matches!(
            entry.planned.item.kind,
            Kind::PrimaryKey(_) | Kind::Unique(_)
        ) {
            indexable(session, table, &entry.columns)?;
        }
    }
    *wrote = true;
    let mut defaults_planned = defaults_planned.into_iter();
    for entry in &stored {
        match &entry.planned.item.kind {
            Kind::Check(_) | Kind::ForeignKey(_) => entry.save(session, table, !validate)?,
            Kind::Default { value, .. } => {
                let column = defaults_planned
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("default planning lost a column"))?;
                apply_default(session, table, &entry.planned, &column, value)?;
            }
            Kind::PrimaryKey(_) => {
                // DuckDB cannot add a key to a table that has indexes.
                rebuild::keys(session, table, |keys| {
                    keys.push(rebuild::Key {
                        primary: true,
                        columns: entry.columns.clone(),
                    });
                    Ok(())
                })?;
                entry.save(session, table, false)?;
            }
            Kind::Unique(_) => {
                rebuild::keys(session, table, |keys| {
                    keys.push(rebuild::Key {
                        primary: false,
                        columns: entry.columns.clone(),
                    });
                    Ok(())
                })?;
                entry.save(session, table, false)?;
            }
        }
    }
    Ok(())
}

/// Validate `DEFAULT value FOR column`; returns the column.
fn check_default(session: &Session, table: &Table, value: &Expr, column: &Ident) -> Result<Column> {
    let columns = catalog::columns(&session.db, table)?;
    let Some(found) = find_column(&columns, &column.value) else {
        return Err(errors::not_created(error(
            1752,
            0,
            format!(
                "Column '{}' in table '{}' is invalid for creating a default constraint.",
                column.value, table.name
            ),
        )));
    };
    let mut failure = None;
    let _ = visit_expressions(value, |expr| {
        match expr {
            Expr::Identifier(ident) if ident.value.starts_with('@') => {
                failure = Some(errors::variable(&ident.value));
            }
            Expr::Identifier(ident) => failure = Some(errors::column_in_default(&ident.value)),
            Expr::CompoundIdentifier(parts) => {
                failure = Some(errors::column_in_default(
                    &parts
                        .iter()
                        .map(|p| p.value.as_str())
                        .collect::<Vec<_>>()
                        .join("."),
                ))
            }
            Expr::Subquery(_) | Expr::Exists { .. } | Expr::InSubquery { .. } => {
                failure = Some(errors::subquery())
            }
            _ => return ControlFlow::Continue(()),
        }
        ControlFlow::Break(())
    });
    if let Some(failure) = failure {
        return Err(failure);
    }
    if found.identity {
        return Err(errors::not_created(error(
            1754,
            0,
            format!(
                "Defaults cannot be created on columns with an IDENTITY attribute. Table '{}', column '{}'.",
                table.name, found.name
            ),
        )));
    }
    if found.computed
        || catalog::native_defaults(&session.db, table)?.contains_key(&found.name.to_lowercase())
    {
        return Err(errors::not_created(error(
            1781,
            1,
            "Column already has a DEFAULT bound to it.",
        )));
    }
    // Lowering errors (unsupported functions, types) surface before writing.
    translate::default(session, table, found, value)?;
    Ok(found.clone())
}

fn apply_default(
    session: &mut Session,
    table: &Table,
    planned: &Planned,
    column: &Column,
    value: &Expr,
) -> Result<()> {
    let native = translate::default(session, table, column, value)?;
    session.db.execute_batch(&format!(
        "ALTER TABLE {} ALTER COLUMN {} SET DEFAULT {native}",
        table.sql(),
        catalog::quote(&column.name)
    ))?;
    session.db.execute(
        "INSERT INTO main.__msduck_default_constraints VALUES(?,?,?,?,CAST(current_timestamp AS TIMESTAMP),CAST(current_timestamp AS TIMESTAMP))",
        duckdb::params![planned.id, table.id, column.id, planned.name],
    )?;
    catalog::record_default_source(&session.db, planned.id, value, planned.system_named)?;
    Ok(())
}

fn drop(
    session: &mut Session,
    table: &Table,
    written: &ObjectName,
    items: Vec<DropItem>,
    parameters: &mut HashMap<String, Parameter>,
    wrote: &mut bool,
) -> Result<()> {
    let constraints = catalog::load(&session.db)?;
    enum Target {
        Constraint(Constraint),
        Default(i32, String),
        Column(Ident, bool),
    }
    let mut targets = Vec::new();
    let mut seen = HashSet::new();
    for item in items {
        match item {
            DropItem::Column { name, if_exists } => targets.push(Target::Column(name, if_exists)),
            DropItem::Constraint { name, if_exists } => {
                if !seen.insert(name.value.to_lowercase()) {
                    return Err(errors::duplicate_in_statement(&name.value));
                }
                if let Some(constraint) = constraints
                    .iter()
                    .find(|c| same(&c.name, &name.value) && same(&c.table.schema, &table.schema))
                {
                    if constraint.table.id != table.id {
                        if if_exists {
                            continue;
                        }
                        return Err(errors::other_table(&constraint.name, &table.name));
                    }
                    targets.push(Target::Constraint(constraint.clone()));
                } else if let Some(default) =
                    catalog::default_named(&session.db, &table.schema, &name.value)?
                {
                    if default.table != table.id {
                        if if_exists {
                            continue;
                        }
                        return Err(errors::other_table(&name.value, &table.name));
                    }
                    targets.push(Target::Default(default.id, default.column));
                } else if !if_exists {
                    return Err(errors::not_a_constraint(&name.value));
                }
            }
        }
    }
    // A key referenced by a foreign key that stays cannot be dropped.
    for target in &targets {
        if let Target::Constraint(key) = target
            && matches!(key.kind, Type::Primary | Type::Unique)
        {
            let wanted: HashSet<String> = key.columns.iter().map(|c| c.to_lowercase()).collect();
            if let Some(foreign) = constraints.iter().find(|c| {
                c.kind == Type::Foreign
                    && c.referenced.as_ref().is_some_and(|r| r.id == table.id)
                    && c.referenced_columns.len() == wanted.len()
                    && c.referenced_columns
                        .iter()
                        .all(|c| wanted.contains(&c.to_lowercase()))
                    && !targets
                        .iter()
                        .any(|t| matches!(t, Target::Constraint(d) if d.id == c.id))
            }) {
                return Err(errors::referenced_key(
                    &key.name,
                    &foreign.table.name,
                    &foreign.name,
                ));
            }
        }
    }
    for target in targets {
        *wrote = true;
        match target {
            Target::Constraint(constraint) => {
                if matches!(constraint.kind, Type::Primary | Type::Unique) {
                    let columns: Vec<String> = constraint
                        .columns
                        .iter()
                        .map(|c| c.to_lowercase())
                        .collect();
                    let primary = constraint.kind == Type::Primary;
                    let (native, managed) = catalog::key_storage(&session.db, constraint.id)?;
                    if let Some(index) = managed {
                        session.db.execute_batch(&format!(
                            "DROP INDEX {}.{}",
                            catalog::quote(&table.schema),
                            catalog::quote(&index)
                        ))?;
                    }
                    if native {
                        rebuild::keys(session, table, |keys| {
                            let before = keys.len();
                            if let Some(index) = keys.iter().position(|key| {
                                key.primary == primary
                                    && key.columns.len() == columns.len()
                                    && key
                                        .columns
                                        .iter()
                                        .all(|c| columns.contains(&c.to_lowercase()))
                            }) {
                                keys.remove(index);
                            }
                            if keys.len() == before {
                                bail!("constraint {} has no native key", constraint.name);
                            }
                            Ok(())
                        })?;
                    }
                    catalog::delete_key(&session.db, constraint.id)?;
                } else {
                    catalog::delete(&session.db, constraint.id)?;
                }
            }
            Target::Default(id, column) => {
                session.db.execute_batch(&format!(
                    "ALTER TABLE {} ALTER COLUMN {} DROP DEFAULT",
                    table.sql(),
                    catalog::quote(&column)
                ))?;
                catalog::delete_default(&session.db, id)?;
            }
            Target::Column(name, if_exists) => {
                // The column guard checks dependencies on this statement too.
                session.execute(drop_column(written, name, if_exists), parameters)?;
            }
        }
    }
    Ok(())
}

fn drop_column(table: &ObjectName, name: Ident, if_exists: bool) -> Statement {
    Statement::AlterTable(AlterTable {
        name: table.clone(),
        if_exists: false,
        only: false,
        operations: vec![AlterTableOperation::DropColumn {
            has_column_keyword: true,
            column_names: vec![name],
            if_exists,
            drop_behavior: None,
        }],
        location: None,
        on_cluster: None,
        table_type: None,
        end_token: helpers::attached_token::AttachedToken::empty(),
    })
}

fn toggle(
    session: &mut Session,
    table: &Table,
    with_check: Option<bool>,
    enable: bool,
    targets: Targets,
    wrote: &mut bool,
) -> Result<()> {
    let constraints: Vec<Constraint> = catalog::load(&session.db)?
        .into_iter()
        .filter(|c| c.table.id == table.id)
        .collect();
    let selected: Vec<Constraint> = match targets {
        Targets::All => constraints
            .into_iter()
            .filter(|c| c.kind.toggles())
            .collect(),
        Targets::Names(names) => {
            let mut seen = HashSet::new();
            let mut selected = Vec::new();
            for name in names {
                if !seen.insert(name.value.to_lowercase()) {
                    return Err(errors::duplicate_in_statement(&name.value));
                }
                match constraints.iter().find(|c| same(&c.name, &name.value)) {
                    Some(constraint) if constraint.kind.toggles() => {
                        selected.push(constraint.clone())
                    }
                    Some(constraint) => return Err(errors::toggle_kind(&constraint.name)),
                    None => {
                        if catalog::default_named(&session.db, &table.schema, &name.value)?
                            .is_some_and(|d| d.table == table.id)
                        {
                            return Err(errors::toggle_kind(&name.value));
                        }
                        return Err(errors::toggle_missing(&name.value));
                    }
                }
            }
            selected
        }
    };
    let database = session.database.name.clone();
    let validate = enable && with_check == Some(true);
    if validate {
        for constraint in &selected {
            if enforce::violates(session, constraint)?.is_some() {
                return Err(match constraint.kind {
                    Type::Check => errors::check_conflict(Verb::AlterTable, constraint, &database),
                    _ => errors::foreign_conflict(Verb::AlterTable, constraint, &database),
                }
                .into());
            }
        }
    }
    for constraint in &selected {
        *wrote = true;
        if enable {
            catalog::set_state(
                &session.db,
                constraint.id,
                false,
                !validate && constraint.untrusted,
            )?;
        } else {
            catalog::set_state(&session.db, constraint.id, true, true)?;
        }
    }
    Ok(())
}
