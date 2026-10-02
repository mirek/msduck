//! Computed columns whose expressions read or produce carrier-stored types.
//!
//! msduck stores `nvarchar`, `nchar`, `datetime2`, `money` and similar types
//! in carrier representations that only the query pipeline knows how to
//! bind. Before the table exists, this module runs each such expression
//! through that pipeline as `SELECT expr FROM (SELECT CAST(NULL AS type) AS
//! column, ...)`. Derived rows carry Unicode values as UTF-8 VARCHAR, so the
//! lowered expression reads the table's UTF-16 carrier columns as UTF-8 text
//! and stores a Unicode result the same way (see [`super::text`]). It becomes
//! the generated column's expression, opaque to later passes, and the
//! inferred type its declaration.
use crate::engine::{Parameter, Session, Translator, ext, rand};
use anyhow::{Result, bail};
use msduck_sql::dialect::computed_column::{computed, is_placeholder};
use sqlparser::ast::*;
use std::collections::HashMap;
use std::ops::ControlFlow;

/// Whether a computed column needs the pipeline: it reads a carrier-stored
/// column or produces a carrier-stored type.
fn carrier(
    db: &duckdb::Connection,
    table: &CreateTable,
    expr: &Expr,
    sources: &[SelectItem],
) -> Result<(bool, Option<DataType>)> {
    let reads_carrier = crate::computed_columns::references(expr)
        .iter()
        .filter_map(|reference| {
            table
                .columns
                .iter()
                .find(|column| column.name.value.eq_ignore_ascii_case(&reference.value))
        })
        .any(|column| !crate::computed_columns::native(&column.data_type));
    let declared = crate::computed_columns::infer(db, expr, sources)?;
    let produces_carrier = declared
        .as_ref()
        .is_some_and(|kind| !crate::computed_columns::native(kind));
    Ok((reads_carrier || produces_carrier, declared))
}

/// Lower the carrier computed columns of a CREATE TABLE in place.
pub(super) fn lower(
    session: &mut Session,
    table: &mut CreateTable,
    parameters: &HashMap<String, Parameter>,
) -> Result<()> {
    if !table
        .columns
        .iter()
        .any(|column| computed(column).is_some() && is_placeholder(&column.data_type))
    {
        return Ok(());
    }
    crate::computed_columns::validate(table)?;
    let sources = crate::computed_columns::sources(table);
    let mut lowered = Vec::new();
    for (index, column) in table.columns.iter().enumerate() {
        let Some((expr, _)) = computed(column) else {
            continue;
        };
        if !is_placeholder(&column.data_type) {
            continue;
        }
        let (carrier, declared) = carrier(&session.db, table, expr, &sources)?;
        if !carrier {
            continue;
        }
        let query = translate(session, expr, &sources, parameters)?;
        let probe = Probe {
            db: &session.db,
            query: &query,
        };
        let mut expression = projection(&query)?;
        repair_text_inputs(&probe, &mut expression)?;
        let declared = match declared {
            Some(declared) => declared,
            None => probe.native_type(&expression)?.ok_or_else(|| {
                anyhow::anyhow!(
                    "unsupported computed column '{}' whose type cannot be inferred",
                    column.name.value
                )
            })?,
        };
        read_carriers(&mut expression, table);
        // A Unicode result is stored as UTF-8 text, like a view column; the
        // declaration recorded for the column keeps its logical type.
        let stored = if crate::character_storage::unicode_storage_type(&declared).is_some() {
            msduck_sql::expr::unary_function(super::text::COMPUTED_TEXT, expression)
        } else {
            expression
        };
        lowered.push((index, declared, stored));
    }
    for (index, declared, stored) in lowered {
        let column = &mut table.columns[index];
        column.data_type = declared;
        for option in &mut column.options {
            if let ColumnOption::Generated {
                generation_expr: Some(expr),
                ..
            } = &mut option.option
            {
                // Later passes must not translate the DuckDB expression again.
                *expr = Expr::Value(Value::Placeholder(stored.to_string()).into());
            }
        }
    }
    Ok(())
}

/// A MAX computed column cannot be an index key (1919). msduck stores a
/// Unicode computed column as text, which the ordinary index path accepts,
/// so its declaration is checked here.
pub(super) fn check_index_keys(session: &Session, index: &CreateIndex) -> Result<()> {
    let mut max = session.db.prepare(
        "SELECT 1 FROM sys.columns WHERE object_id=__msduck_object_id(?,'U') AND lower(name)=lower(?) AND is_computed AND max_length=-1 AND system_type_id IN (165,167,231)",
    )?;
    for key in &index.columns {
        let Expr::Identifier(column) = &key.column.expr else {
            continue;
        };
        if max.exists(duckdb::params![index.table_name.to_string(), column.value])? {
            let table = index
                .table_name
                .0
                .last()
                .and_then(ObjectNamePart::as_ident)
                .map_or_else(|| index.table_name.to_string(), |id| id.value.clone());
            let (number, state, severity, message) =
                msduck_sql::dialect::ext::keys::value::invalid_key(&column.value, &table);
            bail!(msduck_core::diagnostic::SqlError {
                number,
                state,
                severity,
                message,
                message_utf16: None,
            });
        }
    }
    Ok(())
}

/// Derived rows carry `nvarchar` and `nchar` values as UTF-8 VARCHAR; the
/// table stores them as UTF-16 carriers. Read each such column as text.
fn read_carriers(expression: &mut Expr, table: &CreateTable) {
    let unicode = |name: &str| {
        table.columns.iter().any(|column| {
            computed(column).is_none()
                && column.name.value.eq_ignore_ascii_case(name)
                && crate::character_storage::unicode_storage_type(&column.data_type).is_some()
        })
    };
    let _ = visit_expressions_mut(expression, |expr| {
        if let Expr::Identifier(id) = expr
            && unicode(&id.value)
        {
            // Expressions are visited after their children, so the wrapped
            // identifier is not seen again.
            *expr = msduck_sql::expr::unary_function(super::text::CARRIER_UTF8, expr.clone());
        }
        ControlFlow::<()>::Continue(())
    });
}

/// The DuckDB expression for `expr` over a row of `sources`, as the query
/// pipeline lowers a projection.
fn translate(
    session: &Session,
    expr: &Expr,
    sources: &[SelectItem],
    parameters: &HashMap<String, Parameter>,
) -> Result<Box<Query>> {
    let query = crate::computed_columns::source_query(
        vec![SelectItem::UnnamedExpr(expr.clone())],
        sources,
    )?;
    let mut statement = Statement::Query(query);
    let mut parameters = parameters.clone();
    session.lower_database_functions(&mut statement)?;
    session.qualify_databases(&mut statement)?;
    session.lower_session_functions(&mut statement, &mut parameters)?;
    ext::rewrite(session, &mut statement, &parameters)?;
    crate::query_catalog::bind_binary_operations(&session.db, &mut statement, &parameters)?;
    crate::concat_lower::statement(&mut statement, &parameters).map_err(anyhow::Error::msg)?;
    crate::aggregate_columns::annotate(&session.db, &mut statement, &parameters)
        .map_err(anyhow::Error::msg)?;
    crate::query_catalog::bind_unicode_operations(&session.db, &mut statement, &parameters)?;
    crate::concat_lower::annotated_unicode_casts(&mut statement);
    crate::for_json::lower_nested(&session.db, &mut statement, &parameters)?;
    let mut translator = Translator {
        parameters: &parameters,
        values: vec![],
        parameter_slots: HashMap::new(),
        transactions: session.transactions,
        options_mask: session.options_mask(),
        transaction_doomed: session.transaction_doomed,
        original_login: &session.original_login,
        clock: crate::current_time::now(),
        rowcount: session.rowcount,
        last_error: session.last_error,
        caught_error: session.caught_error.as_ref(),
        spid: session.process.spid(),
    };
    if let ControlFlow::Break(error) = VisitMut::visit(&mut statement, &mut translator) {
        bail!(error);
    }
    let _rand_scopes = rand::lower(
        &mut statement,
        &mut translator.values,
        &session.diagnostics.rand,
        &session.rand,
    )?;
    if !translator.values.is_empty() {
        bail!("unsupported bound values in a computed column");
    }
    // The lowered statement must still bind.
    session.db.prepare(&statement.to_string())?;
    let Statement::Query(query) = statement else {
        unreachable!()
    };
    Ok(query)
}

/// The single projected expression of a lowered query.
fn projection(query: &Query) -> Result<Expr> {
    let SetExpr::Select(select) = query.body.as_ref() else {
        bail!("unsupported computed column expression");
    };
    match select.projection.as_slice() {
        [SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. }] => {
            Ok(expr.clone())
        }
        _ => bail!("unsupported computed column expression"),
    }
}

/// Backend types of lowered subexpressions, bound over the lowered row of
/// the table's other columns.
struct Probe<'a> {
    db: &'a duckdb::Connection,
    query: &'a Query,
}

impl Probe<'_> {
    fn backend_type(&self, expr: &Expr) -> Result<String> {
        let mut query = self.query.clone();
        let SetExpr::Select(select) = query.body.as_mut() else {
            bail!("unsupported computed column expression");
        };
        select.projection = vec![SelectItem::UnnamedExpr(msduck_sql::expr::unary_function(
            "typeof",
            expr.clone(),
        ))];
        Ok(self
            .db
            .query_row(&query.to_string(), [], |row| row.get(0))?)
    }

    /// The SQL Server type of a lowered expression whose declaration could
    /// not be inferred, when its backend type has exactly one.
    fn native_type(&self, expr: &Expr) -> Result<Option<DataType>> {
        Ok(match self.backend_type(expr)?.as_str() {
            "BIGINT" => Some(DataType::BigInt(None)),
            "INTEGER" => Some(DataType::Int(None)),
            "SMALLINT" => Some(DataType::SmallInt(None)),
            "BOOLEAN" => Some(DataType::Bit(None)),
            "DOUBLE" => Some(DataType::Float(ExactNumberInfo::None)),
            _ => None,
        })
    }
}

/// Backend conversions that read their input as VARCHAR text.
const TEXT_INPUTS: &[&str] = &[
    "__msduck_cast_nvarchar",
    "__msduck_try_nvarchar",
    "__msduck_cast_varchar",
    "__msduck_try_varchar",
    "__msduck_cast_char",
    "__msduck_try_char",
];

/// Conversions that read VARCHAR text stringify a UTF-16 carrier produced
/// by a Unicode function (JSON_VALUE, LEFT, ...). Give them its UTF-8 text.
fn repair_text_inputs(probe: &Probe<'_>, expression: &mut Expr) -> Result<()> {
    let carrier = |expr: &Expr| -> Result<bool> {
        Ok(probe.backend_type(expr)? == "STRUCT(__msduck_utf16le BLOB)")
    };
    let flow = visit_expressions_mut(expression, |expr| {
        let input = match expr {
            Expr::Cast {
                expr: input,
                data_type: DataType::Varchar(None) | DataType::Text | DataType::String(None),
                ..
            } => Some(input.as_mut()),
            Expr::Function(function)
                if TEXT_INPUTS
                    .iter()
                    .any(|name| function.name.to_string().eq_ignore_ascii_case(name)) =>
            {
                match &mut function.args {
                    FunctionArguments::List(list) => match list.args.first_mut() {
                        Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(input))) => Some(input),
                        _ => None,
                    },
                    _ => None,
                }
            }
            _ => None,
        };
        if let Some(input) = input {
            match carrier(input) {
                Ok(true) => {
                    *input =
                        msduck_sql::expr::unary_function(super::text::CARRIER_UTF8, input.clone());
                }
                Ok(false) => {}
                Err(error) => return ControlFlow::Break(error),
            }
        }
        ControlFlow::Continue(())
    });
    match flow {
        ControlFlow::Continue(()) => Ok(()),
        ControlFlow::Break(error) => Err(error),
    }
}
