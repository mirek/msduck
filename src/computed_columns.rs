//! SQL Server computed columns (`name AS expr [PERSISTED]`), stored as DuckDB
//! VIRTUAL generated columns. Behavior follows
//! reference/tedious-compat-gaps.json; see docs/tedious-compat-gaps.md.
//!
//! The declared type comes from the expression, inferred over the table's
//! other column declarations before the statement runs, and is recorded like
//! any column declaration. `main.__msduck_computed_columns` marks computed
//! columns for sys.columns and for write checks (error 271).
use anyhow::{Result, bail};
use msduck_core::diagnostic::SqlError;
use msduck_sql::dialect::computed_column::{computed, is_placeholder};
use sqlparser::ast::*;
use std::ops::ControlFlow;

/// Functions whose value changes between evaluations. A PERSISTED computed
/// column cannot use them (4936).
const NONDETERMINISTIC: &[&str] = &[
    "GETDATE",
    "GETUTCDATE",
    "SYSDATETIME",
    "SYSUTCDATETIME",
    "SYSDATETIMEOFFSET",
    "CURRENT_TIMESTAMP",
    "NEWID",
    "NEWSEQUENTIALID",
    "RAND",
];

fn nondeterministic(expr: &Expr) -> bool {
    visit_expressions(expr, |expr| match expr {
        Expr::Function(function)
            if NONDETERMINISTIC
                .iter()
                .any(|name| function.name.to_string().eq_ignore_ascii_case(name)) =>
        {
            ControlFlow::Break(())
        }
        _ => ControlFlow::Continue(()),
    })
    .is_break()
}

/// Functions and names whose value depends on the session. SQL Server
/// refuses them in a PERSISTED computed column (4936). Lowering would fix the
/// creating session's value into the definition, so msduck also refuses them
/// in non-persisted columns.
const SESSION_FUNCTIONS: &[&str] = &[
    "SUSER_SNAME",
    "SUSER_NAME",
    "ORIGINAL_LOGIN",
    "HOST_NAME",
    "APP_NAME",
    "SESSION_CONTEXT",
    "SESSIONPROPERTY",
    "XACT_STATE",
    "DB_NAME",
    "DB_ID",
    "USER_NAME",
];
const SESSION_NAMES: &[&str] = &[
    "SYSTEM_USER",
    "CURRENT_USER",
    "SESSION_USER",
    "@@SPID",
    "@@ROWCOUNT",
    "@@ERROR",
    "@@TRANCOUNT",
];

fn session_dependent(expr: &Expr) -> bool {
    visit_expressions(expr, |expr| match expr {
        Expr::Function(function)
            if SESSION_FUNCTIONS
                .iter()
                .any(|name| function.name.to_string().eq_ignore_ascii_case(name))
                || SESSION_NAMES
                    .iter()
                    .any(|name| function.name.to_string().eq_ignore_ascii_case(name)) =>
        {
            ControlFlow::Break(())
        }
        Expr::Identifier(id)
            if id.quote_style.is_none()
                && SESSION_NAMES
                    .iter()
                    .any(|name| id.value.eq_ignore_ascii_case(name)) =>
        {
            ControlFlow::Break(())
        }
        _ => ControlFlow::Continue(()),
    })
    .is_break()
}

/// Declarations whose DuckDB storage is the value itself. Other types use
/// carrier representations (UTF-16, offsets, variants, money) that a
/// generated-column expression cannot yet bind.
pub fn native(kind: &DataType) -> bool {
    use msduck_core::{character::Family, types::Type};
    match msduck_sql::sql_type::declaration(kind) {
        Ok(Type::Character(character)) => {
            matches!(character.family(), Family::Char | Family::Varchar)
        }
        Ok(
            Type::Bit
            | Type::TinyInt
            | Type::SmallInt
            | Type::Int
            | Type::BigInt
            | Type::Real
            | Type::Float
            | Type::Decimal(_)
            | Type::Date
            | Type::DateTime
            | Type::UniqueIdentifier,
        ) => true,
        _ => false,
    }
}

/// Functions whose first argument is a date part keyword, not a column.
const DATE_PART_FUNCTIONS: &[&str] = &[
    "DATEADD",
    "DATEDIFF",
    "DATEDIFF_BIG",
    "DATEPART",
    "DATENAME",
    "DATETRUNC",
    "DATE_BUCKET",
];

/// Column names referenced by an expression, excluding variables and date
/// part arguments.
pub fn references(expr: &Expr) -> Vec<Ident> {
    let mut parts = Vec::new();
    let _ = visit_expressions(expr, |expr| {
        if let Expr::Function(function) = expr
            && DATE_PART_FUNCTIONS
                .iter()
                .any(|name| function.name.to_string().eq_ignore_ascii_case(name))
            && let FunctionArguments::List(args) = &function.args
            && let Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(part))) = args.args.first()
        {
            parts.push(part as *const Expr);
        }
        ControlFlow::<()>::Continue(())
    });
    let mut found = Vec::new();
    let _ = visit_expressions(expr, |expr| {
        match expr {
            Expr::Identifier(_) if parts.contains(&(expr as *const Expr)) => {}
            Expr::Identifier(id) if !id.value.starts_with('@') => found.push(id.clone()),
            Expr::CompoundIdentifier(ids) => found.extend(ids.last().cloned()),
            _ => {}
        }
        ControlFlow::<()>::Continue(())
    });
    found
}

/// Validate the computed columns of a CREATE TABLE and replace each
/// placeholder type with the type inferred from its expression. A type that
/// cannot be inferred stays unknown and DuckDB's own type is used. Columns
/// whose expression the `computed` feature already lowered keep their
/// declared type.
pub fn plan(db: &duckdb::Connection, table: &mut CreateTable) -> Result<()> {
    if !table
        .columns
        .iter()
        .any(|column| computed(column).is_some())
    {
        return Ok(());
    }
    validate(table)?;
    let sources = sources(table);
    for column in &mut table.columns {
        let Some((expr, _)) = computed(column) else {
            continue;
        };
        if !is_placeholder(&column.data_type) {
            continue;
        }
        for reference in references(expr) {
            let source = sources
                .iter()
                .find_map(|item| match item {
                    SelectItem::ExprWithAlias {
                        expr: Expr::Cast { data_type, .. },
                        alias,
                    } if alias.value.eq_ignore_ascii_case(&reference.value) => Some(data_type),
                    _ => None,
                })
                .expect("validated reference");
            if !native(source) {
                bail!(
                    "unsupported computed column '{}' over {} column '{}'",
                    column.name.value,
                    source,
                    reference.value
                );
            }
        }
        match infer(db, expr, &sources)? {
            Some(kind) if native(&kind) => column.data_type = kind,
            Some(kind) => bail!(
                "unsupported computed column '{}' of type {kind}",
                column.name.value
            ),
            None => {}
        }
    }
    Ok(())
}

/// The table's name as SQL Server error messages print it.
fn table_name(table: &CreateTable) -> String {
    table
        .name
        .0
        .last()
        .and_then(ObjectNamePart::as_ident)
        .map_or_else(|| table.name.to_string(), |id| id.value.clone())
}

/// Check the computed columns' references (207, 1759) and determinism
/// (4936, and msduck's refusal of non-persisted non-deterministic columns).
pub fn validate(table: &CreateTable) -> Result<()> {
    let table_name = table_name(table);
    let is_computed = |name: &str| {
        table
            .columns
            .iter()
            .any(|c| computed(c).is_some() && c.name.value.eq_ignore_ascii_case(name))
    };
    let exists = |name: &str| {
        table
            .columns
            .iter()
            .any(|c| c.name.value.eq_ignore_ascii_case(name))
    };
    for column in &table.columns {
        let Some((expr, persisted)) = computed(column) else {
            continue;
        };
        for reference in references(expr) {
            if !exists(&reference.value) {
                bail!(SqlError::new(
                    207,
                    1,
                    format!("Invalid column name '{}'.", reference.value)
                ));
            }
            if is_computed(&reference.value) {
                bail!(SqlError::new(
                    1759,
                    0,
                    format!(
                        "Computed column '{}' in table '{table_name}' is not allowed to be used in another computed-column definition.",
                        reference.value
                    )
                ));
            }
        }
        let session = session_dependent(expr);
        if nondeterministic(expr) || session {
            if persisted {
                bail!(SqlError::new(
                    4936,
                    1,
                    format!(
                        "Computed column '{}' in table '{table_name}' cannot be persisted because the column is non-deterministic.",
                        column.name.value
                    )
                ));
            }
            // DuckDB evaluates generated columns when read, but msduck binds
            // current-time and session functions when it lowers them.
            bail!(
                "unsupported {} computed column '{}'",
                if session {
                    "session-dependent"
                } else {
                    "non-deterministic"
                },
                column.name.value
            );
        }
    }
    Ok(())
}

/// A row of the table's other columns, as typed NULLs, for inferring and
/// lowering computed-column expressions.
pub fn sources(table: &CreateTable) -> Vec<SelectItem> {
    table
        .columns
        .iter()
        .filter(|column| computed(column).is_none())
        .map(|column| SelectItem::ExprWithAlias {
            expr: Expr::Cast {
                kind: CastKind::Cast,
                expr: Box::new(Expr::Value(Value::Null.into())),
                data_type: column.data_type.clone(),
                format: None,
            },
            alias: column.name.clone(),
        })
        .collect()
}

/// `SELECT projection FROM (SELECT sources) AS __msduck_computed_source`.
pub fn source_query(projection: Vec<SelectItem>, sources: &[SelectItem]) -> Result<Box<Query>> {
    let Statement::Query(mut query) = msduck_sql::batch::parse(
        "SELECT __msduck_expr FROM (SELECT 1 AS __msduck_column) AS __msduck_computed_source",
    )?
    .remove(0) else {
        unreachable!()
    };
    let SetExpr::Select(select) = query.body.as_mut() else {
        unreachable!()
    };
    select.projection = projection;
    if let Some(TableFactor::Derived { subquery, .. }) =
        select.from.first_mut().map(|from| &mut from.relation)
        && let SetExpr::Select(inner) = subquery.body.as_mut()
        && !sources.is_empty()
    {
        inner.projection = sources.to_vec();
    }
    Ok(query)
}

/// The declared type of `expr` over a row of the other columns.
pub fn infer(
    db: &duckdb::Connection,
    expr: &Expr,
    sources: &[SelectItem],
) -> Result<Option<DataType>> {
    let query = source_query(vec![SelectItem::UnnamedExpr(expr.clone())], sources)?;
    let Some(fields) = crate::query_catalog::projection(db, &query)? else {
        return Ok(None);
    };
    Ok(fields
        .first()
        .and_then(|field| field.info.as_ref())
        .and_then(|info| info.logical_type())
        .map(msduck_sql::sql_type::ast)
        .filter(|kind| !is_placeholder(kind)))
}

/// Record the computed columns of a created table.
pub fn record(db: &duckdb::Connection, statement: &Statement) -> Result<()> {
    let Statement::CreateTable(table) = statement else {
        return Ok(());
    };
    let object_id: Option<i32> = db.query_row(
        "SELECT __msduck_object_id(?,'U')",
        [table.name.to_string()],
        |row| row.get(0),
    )?;
    let Some(object_id) = object_id else {
        return Ok(());
    };
    db.execute(
        "DELETE FROM main.__msduck_computed_columns WHERE object_id=?",
        [object_id],
    )?;
    for column in &table.columns {
        if let Some((_, persisted)) = computed(column) {
            db.execute(
                "INSERT INTO main.__msduck_computed_columns VALUES (?,lower(?),?)",
                duckdb::params![object_id, column.name.value, persisted],
            )?;
        }
    }
    Ok(())
}

/// Remove computed-column rows whose table or column no longer exists, so a
/// dropped computed column never marks a later column of the same name.
pub fn prune(db: &duckdb::Connection) -> Result<()> {
    db.execute_batch(
        "DELETE FROM main.__msduck_computed_columns k WHERE NOT EXISTS(SELECT 1 FROM main.__msduck_column_info c WHERE c.object_id=k.object_id AND lower(c.name)=k.name_key)",
    )?;
    Ok(())
}

/// Reject INSERT column lists and UPDATE assignments that name a computed
/// column (271), before DuckDB reports it without the column name.
pub fn check_writes(db: &duckdb::Connection, statement: &Statement) -> Result<()> {
    let (table, columns): (String, Vec<&Ident>) = match statement {
        Statement::Insert(insert) => match &insert.table {
            TableObject::TableName(name) => (
                name.to_string(),
                insert
                    .columns
                    .iter()
                    .filter_map(|column| column.0.last()?.as_ident())
                    .collect(),
            ),
            _ => return Ok(()),
        },
        Statement::Update(update) => {
            let TableFactor::Table { name, .. } = &update.table.relation else {
                return Ok(());
            };
            let targets = update
                .assignments
                .iter()
                .filter_map(|assignment| match &assignment.target {
                    AssignmentTarget::ColumnName(name) => name.0.last()?.as_ident(),
                    _ => None,
                })
                .collect();
            (name.to_string(), targets)
        }
        _ => return Ok(()),
    };
    if columns.is_empty() {
        return Ok(());
    }
    let mut query = db.prepare(
        "SELECT 1 FROM main.__msduck_computed_columns WHERE object_id=__msduck_object_id(?,'U') AND name_key=lower(?)",
    )?;
    for column in columns {
        if query.exists(duckdb::params![table, column.value])? {
            bail!(SqlError::new(
                271,
                1,
                format!(
                    "The column \"{}\" cannot be modified because it is either a computed column or is the result of a UNION operator.",
                    column.value
                )
            ));
        }
    }
    Ok(())
}

/// The lower-case names of a table's computed columns.
pub fn names(db: &duckdb::Connection, schema: &str, table: &str) -> Result<Vec<String>> {
    let mut query = db.prepare(
        "SELECT name_key FROM main.__msduck_computed_columns WHERE object_id=__msduck_object_id(?,'U')",
    )?;
    let name = ObjectName::from(vec![
        Ident::with_quote('[', schema),
        Ident::with_quote('[', table),
    ])
    .to_string();
    Ok(query
        .query_map([name], |row| row.get(0))?
        .collect::<duckdb::Result<Vec<String>>>()?)
}
