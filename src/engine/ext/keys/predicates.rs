//! Predicates, ordering, concatenation and character conversions over
//! Unicode carriers (NVARCHAR/NCHAR columns stored as
//! `STRUCT(__msduck_utf16le BLOB)`).
//!
//! DuckDB cannot compare a carrier with VARCHAR text, applies LIKE only to
//! VARCHAR, orders carriers by their little-endian payload bytes and turns
//! them into their STRUCT display text when a character cast or CONCAT
//! asks for VARCHAR. The lowering here, which runs last on the backend AST
//! ([`lower`]), dispatches each such operation on `typeof` and, for
//! carriers:
//!
//! - compares (`=`, `<>`, `<`, `>`, `<=`, `>=`, IN, BETWEEN, simple CASE,
//!   joins) byte keys that order like msduck's default binary comparison:
//!   UTF-16 code units, case-sensitive, ignoring trailing spaces ([`key`]);
//! - matches LIKE with SQL Server's Unicode semantics ([`like`]);
//! - converts character casts and CONCAT through the carrier's code units
//!   instead of its STRUCT text.
//!
//! ORDER BY items naming carrier columns sort by the same keys ([`order`]),
//! GROUP BY and SELECT DISTINCT over character columns group by them
//! ([`group`]),
//! and carrier columns mixed with text in ISNULL, COALESCE, IIF, CASE and
//! set operations convert to their declared type first ([`pin`]).
//! See docs/gaps-unicode-predicates.md.
use crate::engine::Parameter;
use anyhow::Result;
use sqlparser::ast::{Expr, Query, SetExpr, Statement, Value, Visit, VisitMut, Visitor};
use std::collections::HashMap;
use std::ops::ControlFlow;

mod catalog;
mod group;
mod key;
mod like;
mod lower;
mod mark;
mod native;
mod order;
mod pin;

/// Native functions; the lowering refers to them by name.
pub(super) fn register(db: &duckdb::Connection) -> Result<()> {
    db.register_scalar_function::<native::Input<false>>(lower::INPUT)?;
    db.register_scalar_function::<native::Input<true>>(lower::OPERAND)?;
    db.register_scalar_function::<native::OrderKey>(lower::ORDER_KEY)?;
    db.register_scalar_function::<native::Text>(lower::TEXT)?;
    db.register_scalar_function::<native::Like>(lower::LIKE)?;
    // Markers are consumed by the lowering; one that some other lowering
    // moved out of reach stays the plain value.
    for marker in [lower::MARK, lower::MAYBE, lower::MEMBER, lower::GROUP] {
        db.execute_batch(&format!("CREATE OR REPLACE MACRO {marker}(v) AS v"))?;
    }
    Ok(())
}

pub(super) fn rewrite_statement(
    db: &duckdb::Connection,
    statement: &mut Statement,
    parameters: &HashMap<String, Parameter>,
) -> Result<()> {
    if matches!(
        statement,
        Statement::Query(_)
            | Statement::Insert(_)
            | Statement::Update(_)
            | Statement::Delete(_)
            | Statement::Merge(_)
            | Statement::CreateView(_)
    ) {
        let ordered = order::sites(statement) || group::sites(statement);
        rewrite(db, statement, ordered, parameters)?;
    }
    Ok(())
}

/// Subqueries of scalar evaluations (IF, WHILE, SET, RETURN), which have no
/// statement pass; inside statements this repeats that pass, idempotently.
/// Every node also gets [`mark::literals`].
pub(super) fn rewrite_expr(
    db: &duckdb::Connection,
    expr: &mut Expr,
    parameters: &HashMap<String, Parameter>,
) -> Result<()> {
    if matches!(
        expr,
        Expr::Subquery(_) | Expr::Exists { .. } | Expr::InSubquery { .. }
    ) {
        rewrite(db, expr, false, parameters)?;
    }
    mark::literals(parameters, expr);
    Ok(())
}

fn rewrite<T: Visit + VisitMut + 'static>(
    db: &duckdb::Connection,
    node: &mut T,
    ordered: bool,
    parameters: &HashMap<String, Parameter>,
) -> Result<()> {
    if !ordered && !predicates(node) {
        return Ok(());
    }
    let mut catalog = catalog::Catalog::load(db, node)?;
    if catalog.has_text() {
        mark::pad_ranges(&catalog, parameters, node);
    }
    let grouped = ordered
        && (node as &dyn std::any::Any)
            .downcast_ref::<Statement>()
            .is_some_and(group::sites);
    // A conditional/set site alone does not establish character inputs.
    // Preserve typed temporal/currency binding when this catalog has no
    // carrier or collation facts; OPENJSON declarations are loaded above.
    let scalar_only =
        !catalog.has_carriers() && !catalog.has_collations() && !likes(node) && !grouped;
    if scalar_only && !mark::scalar_character_predicates(&catalog, parameters, node) {
        return Ok(());
    }
    mark::rewrite(&catalog, parameters, node, scalar_only)?;
    if pin::sites(node) && pin::rewrite(&catalog, parameters, node, false) {
        catalog.declare(db, node)?;
        pin::rewrite(&catalog, parameters, node, true);
    }
    if ordered && let Some(statement) = (node as &mut dyn std::any::Any).downcast_mut::<Statement>()
    {
        group::rewrite(&catalog, statement);
        order::rewrite(&catalog, statement);
    }
    Ok(())
}

/// Whether `node` has a LIKE, which matches case-insensitively over known
/// character data even without carrier columns.
fn likes<T: Visit>(node: &T) -> bool {
    struct Find;
    impl Visitor for Find {
        type Break = ();
        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
            if matches!(expr, Expr::Like { .. }) {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        }
    }
    node.visit(&mut Find).is_break()
}

/// Whether `node` has an operation the passes before translation may change.
fn predicates<T: Visit>(node: &T) -> bool {
    struct Find;
    impl Visitor for Find {
        type Break = ();
        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
            match expr {
                Expr::BinaryOp { .. }
                | Expr::Between { .. }
                | Expr::Like { .. }
                | Expr::InList { .. }
                | Expr::InSubquery { .. }
                | Expr::Case { .. } => ControlFlow::Break(()),
                Expr::Function(f)
                    if matches!(
                        f.name.to_string().to_ascii_uppercase().as_str(),
                        "ISNULL"
                            | "COALESCE"
                            | "IIF"
                            | "NULLIF"
                            | "COUNT"
                            | "COUNT_BIG"
                            | "MIN"
                            | "MAX"
                    ) =>
                {
                    ControlFlow::Break(())
                }
                _ => ControlFlow::Continue(()),
            }
        }
        fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<()> {
            if matches!(query.body.as_ref(), SetExpr::SetOperation { .. }) {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        }
    }
    node.visit(&mut Find).is_break()
}

/// SQL Server's error for a LIKE escape that is not one character.
pub(super) fn check(expr: &Expr) -> Result<(), (i32, u8, u8, String)> {
    if let Expr::Like {
        escape_char: Some(escape),
        ..
    }
    | Expr::ILike {
        escape_char: Some(escape),
        ..
    } = expr
        && let Expr::Value(value) = escape.as_ref()
        && let Value::SingleQuotedString(text) | Value::NationalStringLiteral(text) = &value.value
        && text.encode_utf16().count() != 1
    {
        return Err((506, 2, 16, native::invalid_escape(text)));
    }
    Ok(())
}

pub(super) fn lower_expr(expr: &mut Expr) {
    lower::lower(expr);
}
