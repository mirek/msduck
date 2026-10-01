//! Explicit COLLATE, styled CONVERT, FORMAT, SERVERPROPERTY, DATABASEPROPERTYEX
//! and ROWCOUNT_BIG. See docs/gaps-conversion.md.
//!
//! - `properties` replaces SERVERPROPERTY, DATABASEPROPERTYEX and ROWCOUNT_BIG
//!   with their values before binding (`rewrite_statement`, `rewrite_expr`).
//! - `format` turns FORMAT into a native call typed nvarchar(4000).
//! - `style` validates styled CONVERT before binding and lowers the styles the
//!   built-in conversions leave behind (`lower_expr`).
//! - `collation` validates COLLATE names and lowers explicit linguistic
//!   collations to DuckDB collations.
//! - `native` holds the DuckDB scalar functions; `temporal`, `binary` and
//!   `dotnet` the pure formatting and parsing rules they use.
use super::Feature;
use crate::engine::{Parameter, Session};
use anyhow::Result;
use sqlparser::ast::{Expr, Function, FunctionArg, FunctionArgExpr, FunctionArguments, Statement};
use std::collections::HashMap;

mod binary;
mod collation;
mod dotnet;
mod format;
mod native;
mod properties;
mod style;
mod temporal;

#[derive(Default)]
pub(crate) struct State;

pub(super) struct Hooks;

impl Feature for Hooks {
    fn name(&self) -> &'static str {
        "conversion"
    }

    fn register(&self, db: &duckdb::Connection) -> Result<()> {
        native::register(db)
    }

    fn rewrite_statement(
        &self,
        _session: &Session,
        statement: &mut Statement,
        _parameters: &HashMap<String, Parameter>,
    ) -> Result<()> {
        properties::prepare(statement)
    }

    fn rewrite_expr(
        &self,
        session: &Session,
        expr: &mut Expr,
        parameters: &HashMap<String, Parameter>,
    ) -> Result<()> {
        collation::validate(expr)?;
        properties::rewrite(session, expr, parameters)?;
        format::rewrite(expr, parameters)?;
        style::validate(expr, parameters)
    }

    fn lower_expr(&self, expr: &mut Expr) -> Result<(), String> {
        style::lower(expr)?;
        collation::lower(expr)
    }
}

/// The unnamed scalar arguments of a plain function call, or `None` when the
/// call has modifiers this feature does not interpret.
fn arguments(function: &Function) -> Option<Vec<&Expr>> {
    if function.over.is_some()
        || function.filter.is_some()
        || function.null_treatment.is_some()
        || !function.within_group.is_empty()
        || !matches!(function.parameters, FunctionArguments::None)
    {
        return None;
    }
    match &function.args {
        FunctionArguments::None => Some(Vec::new()),
        FunctionArguments::List(list)
            if list.clauses.is_empty() && list.duplicate_treatment.is_none() =>
        {
            list.args
                .iter()
                .map(|arg| match arg {
                    FunctionArg::Unnamed(FunctionArgExpr::Expr(expr)) => Some(expr),
                    _ => None,
                })
                .collect()
        }
        _ => None,
    }
}

/// The function's single unquoted name in upper case.
fn name(function: &Function) -> Option<String> {
    let [part] = function.name.0.as_slice() else {
        return None;
    };
    let ident = part.as_ident()?;
    Some(ident.value.to_ascii_uppercase())
}

/// A function call with positional arguments.
fn call(name: &str, args: Vec<Expr>) -> Expr {
    let mut expr = msduck_sql::expr::binary_function(
        name,
        msduck_sql::expr::number(0),
        msduck_sql::expr::number(0),
    );
    if let Expr::Function(function) = &mut expr
        && let FunctionArguments::List(list) = &mut function.args
    {
        list.args = args
            .into_iter()
            .map(|arg| FunctionArg::Unnamed(FunctionArgExpr::Expr(arg)))
            .collect();
    }
    expr
}

fn sql_error(number: i32, state: u8, message: impl Into<String>) -> anyhow::Error {
    anyhow::anyhow!(msduck_core::diagnostic::SqlError::new(
        number,
        state,
        message.into()
    ))
}

fn syntax_error(number: i32, state: u8, message: impl Into<String>) -> anyhow::Error {
    anyhow::anyhow!(msduck_core::diagnostic::SqlError::syntax(
        number,
        state,
        message.into()
    ))
}
