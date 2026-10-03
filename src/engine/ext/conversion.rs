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
//! - `delimited_target` resolves CAST and CONVERT targets such as
//!   `[nvarchar](10)` or `sysname` that features build from stored
//!   declarations, and `unstyled_max` turns CONVERT without a style to a
//!   `max` type into CAST (`rewrite_expr`; docs/bracket-types.md).
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
        style::validate(expr, parameters)?;
        delimited_target(expr);
        unstyled_max(expr);
        Ok(())
    }

    fn lower_expr(&self, expr: &mut Expr) -> Result<(), String> {
        style::lower(expr)?;
        collation::lower(expr)
    }
}

/// A CAST or CONVERT target that names a system type with delimiters, such as
/// `[nvarchar](10)`, or `sysname`, as the plain type. `batch::parse` resolves
/// every target written in a statement, so the targets left here are casts
/// that features build from stored declarations, such as a scalar function's
/// parameters and RETURNS type. They keep declaration defaults: `[varchar]`
/// is `varchar(1)`, as the plain declaration is.
fn delimited_target(expr: &mut Expr) {
    if let Expr::Cast { data_type, .. }
    | Expr::Convert {
        data_type: Some(data_type),
        ..
    } = expr
        && let Some(resolved) =
            msduck_sql::dialect::ext::conversion::bracket_types::declared_value_type(data_type)
    {
        *data_type = resolved;
    }
}

/// CONVERT or TRY_CONVERT without a style to `nvarchar(max)`, `varchar(max)`
/// or `varbinary(max)`, as the equivalent CAST or TRY_CAST. No built-in
/// lowering takes over such a CONVERT, so written either plainly or as
/// `[nvarchar](max)` it reached DuckDB as a call of an unknown `convert`
/// function. Rewritten before binding, the CAST gets every lowering the
/// translator gives CAST, including the one for Unicode carriers.
/// See docs/bracket-types.md.
fn unstyled_max(expr: &mut Expr) {
    use sqlparser::ast::{BinaryLength, CastKind, CharacterLength, DataType};
    let Expr::Convert {
        is_try,
        expr: value,
        data_type: Some(data_type),
        charset: None,
        target_before_value: true,
        styles,
    } = expr
    else {
        return;
    };
    if !styles.is_empty()
        || !matches!(
            data_type,
            DataType::Nvarchar(Some(CharacterLength::Max))
                | DataType::Varchar(Some(CharacterLength::Max))
                | DataType::Varbinary(Some(BinaryLength::Max))
        )
    {
        return;
    }
    *expr = Expr::Cast {
        kind: if *is_try {
            CastKind::TryCast
        } else {
            CastKind::Cast
        },
        expr: value.clone(),
        data_type: data_type.clone(),
        format: None,
    };
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
