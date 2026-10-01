//! Objects that SQL Server binds to a function's (or table's) definition.
//!
//! Computed columns, DEFAULT and CHECK expressions and schema-bound
//! functions keep the folded body of the functions they call, so SQL
//! Server's rule that such references block ALTER and DROP (3729) is also
//! what keeps their stored expressions current. References are recorded in
//! `main.__msduck_function_references`; rows whose referencing object no
//! longer exists are ignored.
use super::{Session, is_function, modules};
use anyhow::{Result, bail};
use duckdb::{OptionalExt, params};
use msduck_core::diagnostic::SqlError;
use msduck_sql::dialect::ext::functions::Definition;
use sqlparser::ast::*;
use std::ops::ControlFlow;

pub(super) fn bootstrap(db: &duckdb::Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS main.__msduck_function_references(
            referenced_id INTEGER NOT NULL,
            kind VARCHAR NOT NULL,
            schema_name VARCHAR NOT NULL,
            object_name VARCHAR NOT NULL,
            column_name VARCHAR NOT NULL DEFAULT '',
            constraint_name VARCHAR NOT NULL DEFAULT '',
            definition VARCHAR NOT NULL DEFAULT '')",
    )?;
    Ok(())
}

/// Schema (default dbo) and name of a one- or two-part object name.
fn split(name: &ObjectName) -> Option<(String, String)> {
    let parts: Vec<String> = name
        .0
        .iter()
        .map(|part| part.as_ident().map(|ident| ident.value.clone()))
        .collect::<Option<_>>()?;
    match parts.as_slice() {
        [name] => Some(("dbo".into(), name.clone())),
        [schema, name] => Some((schema.clone(), name.clone())),
        _ => None,
    }
}

fn object_id(db: &duckdb::Connection, schema: &str, name: &str, kind: &str) -> Result<Option<i32>> {
    Ok(db.query_row(
        "SELECT __msduck_object_id(?, ?)",
        params![
            format!(
                "[{}].[{}]",
                schema.replace(']', "]]"),
                name.replace(']', "]]")
            ),
            kind
        ],
        |row| row.get(0),
    )?)
}

/// Scalar functions an expression calls directly.
fn called_functions(session: &Session, expr: &Expr) -> Result<Vec<i32>> {
    struct Find(Vec<ObjectName>);
    impl Visitor for Find {
        type Break = ();
        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
            if let Expr::Function(function) = expr
                && function.name.0.len() == 2
            {
                self.0.push(function.name.clone());
            }
            ControlFlow::Continue(())
        }
    }
    let mut find = Find(Vec::new());
    let _ = expr.visit(&mut find);
    let mut ids = Vec::new();
    for name in find.0 {
        let Some((schema, name)) = split(&name) else {
            continue;
        };
        if modules::schema_id(&session.db, Some(&schema)).is_err() {
            continue;
        }
        if let Some(module) = modules::find(&session.db, Some(&schema), &name)?
            && module.type_code == "FN"
            && !ids.contains(&module.object_id)
        {
            ids.push(module.object_id);
        }
    }
    Ok(ids)
}

fn insert(
    session: &Session,
    referenced: i32,
    kind: &str,
    schema: &str,
    object: &str,
    column: &str,
    constraint: &str,
) -> Result<()> {
    session.db.execute(
        "DELETE FROM main.__msduck_function_references WHERE referenced_id = ? AND kind = ? AND lower(schema_name) = lower(?) AND lower(object_name) = lower(?) AND lower(column_name) = lower(?)",
        params![referenced, kind, schema, object, column],
    )?;
    session.db.execute(
        "INSERT INTO main.__msduck_function_references VALUES (?, ?, ?, ?, ?, ?, '')",
        params![referenced, kind, schema, object, column, constraint],
    )?;
    Ok(())
}

/// Drop recorded references of tables whose CREATE or ALTER did not take
/// effect: references are recorded while the statement is rewritten, before
/// it runs, and checked here once a later statement starts.
pub(super) fn settle(session: &Session) -> Result<()> {
    let pending = std::mem::take(&mut *session.ext.functions.pending.borrow_mut());
    let mut remaining = Vec::new();
    let mut failure = None;
    for (schema, table) in pending {
        if failure.is_some() {
            remaining.push((schema, table));
            continue;
        }
        if let Err(error) = settle_table(session, &schema, &table) {
            failure = Some(error);
            remaining.push((schema, table));
        }
    }
    // A statement in an aborted transaction cannot query; retry later.
    session.ext.functions.pending.borrow_mut().extend(remaining);
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn settle_table(session: &Session, schema: &str, table: &str) -> Result<()> {
    let rows: Vec<String> = session
        .db
        .prepare(
            "SELECT DISTINCT column_name FROM main.__msduck_function_references WHERE kind IN ('computed', 'default', 'check') AND lower(schema_name) = lower(?) AND lower(object_name) = lower(?)",
        )?
        .query_map(params![schema, table], |row| row.get(0))?
        .collect::<duckdb::Result<_>>()?;
    for column in rows {
        if live_column(session, schema, table, &column)?.is_none() {
            session.db.execute(
                "DELETE FROM main.__msduck_function_references WHERE kind IN ('computed', 'default', 'check') AND lower(schema_name) = lower(?) AND lower(object_name) = lower(?) AND lower(column_name) = lower(?)",
                params![schema, table, column],
            )?;
        }
    }
    Ok(())
}

/// Forget the references of columns an ALTER TABLE drops.
pub(super) fn drop_columns(session: &Session, statement: &Statement) -> Result<()> {
    let Statement::AlterTable(alter) = statement else {
        return Ok(());
    };
    let Some((schema, table)) = split(&alter.name) else {
        return Ok(());
    };
    for operation in &alter.operations {
        if let AlterTableOperation::DropColumn { column_names, .. } = operation {
            for column in column_names {
                session.db.execute(
                    "DELETE FROM main.__msduck_function_references WHERE kind IN ('computed', 'default', 'check') AND lower(schema_name) = lower(?) AND lower(object_name) = lower(?) AND lower(column_name) = lower(?)",
                    params![schema, table, column.value],
                )?;
            }
        }
    }
    Ok(())
}

/// Record the functions that computed columns, DEFAULT and CHECK
/// expressions of CREATE TABLE or ALTER TABLE ... ADD call.
pub(super) fn record_table(session: &Session, statement: &Statement) -> Result<()> {
    let (name, columns, constraints): (&ObjectName, Vec<&ColumnDef>, Vec<&TableConstraint>) =
        match statement {
            Statement::CreateTable(table) => (
                &table.name,
                table.columns.iter().collect(),
                table.constraints.iter().collect(),
            ),
            Statement::AlterTable(alter) => {
                let mut columns = Vec::new();
                let mut constraints = Vec::new();
                for operation in &alter.operations {
                    match operation {
                        AlterTableOperation::AddColumn { column_def, .. } => {
                            columns.push(column_def)
                        }
                        AlterTableOperation::AddConstraint { constraint, .. } => {
                            constraints.push(constraint)
                        }
                        _ => {}
                    }
                }
                (&alter.name, columns, constraints)
            }
            _ => return Ok(()),
        };
    let Some((schema, table)) = split(name) else {
        return Ok(());
    };
    if table.starts_with('#') {
        return Ok(());
    }
    session
        .ext
        .functions
        .pending
        .borrow_mut()
        .push((schema.clone(), table.clone()));
    // A CREATE TABLE of an existing name fails; keep the old table's rows.
    if matches!(statement, Statement::CreateTable(_))
        && object_id(&session.db, &schema, &table, "")?.is_some()
    {
        return Ok(());
    }
    for column in columns {
        for option in &column.options {
            let (kind, expr) = match &option.option {
                ColumnOption::Default(expr) => ("default", expr),
                ColumnOption::Generated {
                    generation_expr: Some(expr),
                    ..
                } => ("computed", expr),
                ColumnOption::Check(check) => ("check", check.expr.as_ref()),
                _ => continue,
            };
            let constraint = option
                .name
                .as_ref()
                .map(|name| name.value.clone())
                .unwrap_or_default();
            for id in called_functions(session, expr)? {
                insert(
                    session,
                    id,
                    kind,
                    &schema,
                    &table,
                    &column.name.value,
                    &constraint,
                )?;
            }
        }
    }
    for constraint in constraints {
        if let TableConstraint::Check(check) = constraint {
            let name = check
                .name
                .as_ref()
                .map(|name| name.value.clone())
                .unwrap_or_default();
            for id in called_functions(session, &check.expr)? {
                insert(session, id, "check", &schema, &table, "", &name)?;
            }
        }
    }
    Ok(())
}

/// Forget the references of a dropped table and refuse to drop a table or
/// view that a schema-bound function references.
pub(super) fn drop_objects(session: &Session, statement: &Statement) -> Result<()> {
    let Statement::Drop {
        object_type: object_type @ (ObjectType::Table | ObjectType::View),
        names,
        ..
    } = statement
    else {
        return Ok(());
    };
    let (kind, word) = match object_type {
        ObjectType::Table => ("U", "TABLE"),
        _ => ("V", "VIEW"),
    };
    for name in names {
        let Some((schema, object)) = split(name) else {
            continue;
        };
        let Some(id) = object_id(&session.db, &schema, &object, kind)? else {
            continue;
        };
        if let Some(function) = binding_function(session, id)? {
            bail!(SqlError::new(
                3729,
                1,
                format!(
                    "Cannot DROP {word} '{name}' because it is being referenced by object '{function}'."
                )
            ));
        }
    }
    for name in names {
        if let Some((schema, object)) = split(name) {
            session.db.execute(
                "DELETE FROM main.__msduck_function_references WHERE kind <> 'function' AND lower(schema_name) = lower(?) AND lower(object_name) = lower(?)",
                params![schema, object],
            )?;
        }
    }
    Ok(())
}

/// A schema-bound function that still references the object.
fn binding_function(session: &Session, referenced: i32) -> Result<Option<String>> {
    let rows: Vec<(String, String)> = session
        .db
        .prepare(
            "SELECT schema_name, object_name FROM main.__msduck_function_references WHERE referenced_id = ? AND kind = 'function'",
        )?
        .query_map([referenced], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<duckdb::Result<_>>()?;
    for (schema, name) in rows {
        if modules::find(&session.db, Some(&schema), &name)?
            .is_some_and(|module| is_function(&module.type_code))
        {
            return Ok(Some(name));
        }
    }
    Ok(None)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Change {
    Alter,
    Drop,
}

/// Refuse to ALTER or DROP a function that a live object references (3729).
pub(super) fn check_function(
    session: &Session,
    object_id: i32,
    display: &str,
    change: Change,
) -> Result<()> {
    settle(session)?;
    let rows: Vec<(String, String, String, String, String)> = session
        .db
        .prepare(
            "SELECT kind, schema_name, object_name, column_name, constraint_name FROM main.__msduck_function_references WHERE referenced_id = ?",
        )?
        .query_map([object_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
        })?
        .collect::<duckdb::Result<_>>()?;
    for (kind, schema, object, column, constraint) in rows {
        // Views are rebound after an ALTER instead (see `rebind_views`).
        if kind == "view" {
            continue;
        }
        let referencing = if kind == "function" {
            modules::find(&session.db, Some(&schema), &object)?
                .filter(|module| is_function(&module.type_code))
                .map(|_| object.clone())
        } else {
            live_column(session, &schema, &object, &column)?.map(|table_id| {
                if !constraint.is_empty() {
                    return Ok(constraint.clone());
                }
                if kind == "default" {
                    let name: Option<String> = session
                        .db
                        .query_row(
                            "SELECT d.name FROM main.__msduck_default_constraints d JOIN main.__msduck_column_info c ON c.object_id = d.parent_object_id AND c.column_id = d.column_id WHERE d.parent_object_id = ? AND lower(c.name) = lower(?)",
                            params![table_id, column],
                            |row| row.get(0),
                        )
                        .optional()?;
                    if let Some(name) = name {
                        return Ok(name);
                    }
                }
                Ok::<_, anyhow::Error>(object.clone())
            })
            .transpose()?
        };
        if let Some(referencing) = referencing {
            let error = match change {
                Change::Alter => SqlError::new(
                    3729,
                    3,
                    format!(
                        "Cannot ALTER '{display}' because it is being referenced by object '{referencing}'."
                    ),
                ),
                Change::Drop => SqlError::new(
                    3729,
                    1,
                    format!(
                        "Cannot DROP FUNCTION '{display}' because it is being referenced by object '{referencing}'."
                    ),
                ),
            };
            bail!(error);
        }
    }
    Ok(())
}

/// The table's id when it still has the column (or, for table-level
/// constraints, when it exists).
fn live_column(session: &Session, schema: &str, table: &str, column: &str) -> Result<Option<i32>> {
    let Some(id) = object_id(&session.db, schema, table, "U")? else {
        return Ok(None);
    };
    if column.is_empty() {
        return Ok(Some(id));
    }
    let exists: i64 = session.db.query_row(
        "SELECT count(*) FROM main.__msduck_column_info WHERE object_id = ? AND lower(name) = lower(?)",
        params![id, column],
        |row| row.get(0),
    )?;
    Ok((exists > 0).then_some(id))
}

/// Forget what a function referenced (it is being dropped or altered).
pub(super) fn forget_function(session: &Session, schema: &str, name: &str) -> Result<()> {
    session.db.execute(
        "DELETE FROM main.__msduck_function_references WHERE kind = 'function' AND lower(schema_name) = lower(?) AND lower(object_name) = lower(?)",
        params![schema, name],
    )?;
    Ok(())
}

/// Validate a schema-bound definition (4512, 4513) and return the tables,
/// views and functions it references.
pub(super) fn bind_schema(
    session: &Session,
    definition: &Definition,
    display: &str,
) -> Result<Vec<i32>> {
    if !definition.options.schemabinding {
        return Ok(vec![]);
    }
    struct Find {
        tables: Vec<ObjectName>,
        functions: Vec<ObjectName>,
        ctes: Vec<String>,
    }
    impl Visitor for Find {
        type Break = ();
        fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<()> {
            if let Some(with) = &query.with {
                for cte in &with.cte_tables {
                    self.ctes.push(cte.alias.name.value.to_lowercase());
                }
            }
            ControlFlow::Continue(())
        }
        fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<()> {
            if let TableFactor::Table { name, args, .. } = factor {
                if args.is_some() {
                    self.functions.push(name.clone());
                } else {
                    self.tables.push(name.clone());
                }
            }
            ControlFlow::Continue(())
        }
        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
            if let Expr::Function(function) = expr
                && function.name.0.len() >= 2
            {
                self.functions.push(function.name.clone());
            }
            ControlFlow::Continue(())
        }
    }
    let mut find = Find {
        tables: vec![],
        functions: vec![],
        ctes: vec![],
    };
    match &definition.body {
        msduck_sql::dialect::ext::functions::Body::Statements(statements) => {
            for statement in statements {
                let _ = statement.visit(&mut find);
            }
        }
        msduck_sql::dialect::ext::functions::Body::Query(query) => {
            let _ = query.visit(&mut find);
        }
    }
    let invalid = |name: &ObjectName| {
        SqlError::new(
            4512,
            3,
            format!(
                "Cannot schema bind function '{display}' because name '{name}' is invalid for schema binding. Names must be in two-part format and an object cannot reference itself."
            ),
        )
    };
    let schema = definition.schema.clone().unwrap_or_else(|| "dbo".into());
    let mut references = Vec::new();
    for table in &find.tables {
        let text = table.to_string();
        if text.starts_with('@') || (table.0.len() == 1 && find.ctes.contains(&text.to_lowercase()))
        {
            continue;
        }
        if table.0.len() != 2 {
            bail!(invalid(table));
        }
        let Some((table_schema, object)) = split(table) else {
            bail!(invalid(table));
        };
        let id = match object_id(&session.db, &table_schema, &object, "U")? {
            Some(id) => Some(id),
            None => object_id(&session.db, &table_schema, &object, "V")?,
        };
        match id {
            Some(id) => references.push(id),
            None => bail!(SqlError::new(
                208,
                1,
                format!("Invalid object name '{text}'.")
            )),
        }
    }
    for function in &find.functions {
        if function.0.len() != 2 {
            bail!(invalid(function));
        }
        let Some((function_schema, object)) = split(function) else {
            bail!(invalid(function));
        };
        if function_schema.eq_ignore_ascii_case(&schema)
            && object.eq_ignore_ascii_case(&definition.name)
        {
            bail!(invalid(function));
        }
        if modules::schema_id(&session.db, Some(&function_schema)).is_err() {
            continue;
        }
        let Some(module) = modules::find(&session.db, Some(&function_schema), &object)? else {
            continue;
        };
        if !is_function(&module.type_code) {
            continue;
        }
        let bound = serde_json::from_str::<serde_json::Value>(&module.properties)
            .ok()
            .and_then(|properties| properties["options"]["schemabinding"].as_bool())
            .unwrap_or(false);
        if !bound {
            bail!(SqlError::new(
                4513,
                2,
                format!(
                    "Cannot schema bind function '{display}'. '{function}' is not schema bound."
                )
            ));
        }
        references.push(module.object_id);
    }
    Ok(references)
}

/// Replace what a stored function references.
pub(super) fn record_function(
    session: &Session,
    schema: &str,
    name: &str,
    references: &[i32],
) -> Result<()> {
    forget_function(session, schema, name)?;
    for id in references {
        insert(session, *id, "function", schema, name, "", "")?;
    }
    Ok(())
}

/// Functions (scalar and table-valued) a tree calls directly.
fn direct_calls<T: Visit>(session: &Session, node: &T) -> Result<Vec<modules::Module>> {
    struct Find(Vec<(ObjectName, bool)>);
    impl Visitor for Find {
        type Break = ();
        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
            if let Expr::Function(function) = expr
                && function.name.0.len() == 2
            {
                self.0.push((function.name.clone(), false));
            }
            ControlFlow::Continue(())
        }
        fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<()> {
            if let TableFactor::Table {
                name,
                args: Some(_),
                ..
            } = factor
            {
                self.0.push((name.clone(), true));
            }
            ControlFlow::Continue(())
        }
    }
    let mut find = Find(Vec::new());
    let _ = node.visit(&mut find);
    let mut modules_found: Vec<modules::Module> = Vec::new();
    for (name, table) in find.0 {
        let Some((schema, name)) = split(&name) else {
            continue;
        };
        if modules::schema_id(&session.db, Some(&schema)).is_err() {
            continue;
        }
        if let Some(module) = modules::find(&session.db, Some(&schema), &name)?
            && is_function(&module.type_code)
            && (module.type_code == "FN") != table
            && !modules_found
                .iter()
                .any(|m| m.object_id == module.object_id)
        {
            modules_found.push(module);
        }
    }
    Ok(modules_found)
}

/// Functions a view calls, directly or through the functions it calls:
/// the stored view keeps all of their folded bodies.
fn view_functions(session: &Session, query: &Query) -> Result<Vec<i32>> {
    let mut pending = direct_calls(session, query)?;
    let mut ids: Vec<i32> = Vec::new();
    while let Some(module) = pending.pop() {
        if ids.contains(&module.object_id) {
            continue;
        }
        ids.push(module.object_id);
        let Ok(definition) =
            msduck_sql::dialect::ext::functions::definition::parse(&module.definition)
        else {
            continue;
        };
        match &definition.body {
            msduck_sql::dialect::ext::functions::Body::Statements(statements) => {
                for statement in statements {
                    pending.extend(direct_calls(session, statement)?);
                }
            }
            msduck_sql::dialect::ext::functions::Body::Query(query) => {
                pending.extend(direct_calls(session, query.as_ref())?);
            }
        }
    }
    Ok(ids)
}

/// Remember the source of a view that calls functions, so ALTER FUNCTION
/// can rebind it: the stored view keeps the folded bodies.
pub(super) fn record_view(
    session: &Session,
    statement: &Statement,
    source: Option<&str>,
) -> Result<()> {
    let (name, query) = match statement {
        Statement::CreateView(view) => (&view.name, &view.query),
        Statement::AlterView { name, query, .. } => (name, query),
        _ => return Ok(()),
    };
    let Some((schema, view)) = split(name) else {
        return Ok(());
    };
    session.db.execute(
        "DELETE FROM main.__msduck_function_references WHERE kind = 'view' AND lower(schema_name) = lower(?) AND lower(object_name) = lower(?)",
        params![schema, view],
    )?;
    let source = source
        .map(str::to_string)
        .unwrap_or_else(|| statement.to_string());
    for id in view_functions(session, query)? {
        session.db.execute(
            "INSERT INTO main.__msduck_function_references VALUES (?, 'view', ?, ?, '', '', ?)",
            params![id, schema, view, source],
        )?;
    }
    Ok(())
}

/// Recreate the views that call an altered function from their source.
pub(super) fn rebind_views(session: &mut Session, function_id: i32) -> Result<()> {
    let views: Vec<(String, String, String)> = session
        .db
        .prepare(
            "SELECT DISTINCT schema_name, object_name, definition FROM main.__msduck_function_references WHERE kind = 'view' AND referenced_id = ?",
        )?
        .query_map([function_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<duckdb::Result<_>>()?;
    for (schema, view, source) in views {
        if object_id(&session.db, &schema, &view, "V")?.is_none() {
            continue;
        }
        let statement = match crate::engine::parse_batch(&source)?.into_iter().next() {
            Some(Statement::CreateView(created)) => Statement::AlterView {
                name: created.name,
                columns: created
                    .columns
                    .into_iter()
                    .map(|column| column.name)
                    .collect(),
                query: created.query,
                with_options: vec![],
            },
            Some(statement @ Statement::AlterView { .. }) => statement,
            _ => continue,
        };
        session.execute(statement, &mut std::collections::HashMap::new())?;
    }
    Ok(())
}
