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
//! and carrier columns mixed with text in ISNULL, COALESCE, IIF, CASE and
//! set operations convert to their declared type first ([`pin`]).
//! See docs/gaps-unicode-predicates.md.
use anyhow::Result;
use sqlparser::ast::{Expr, Query, SetExpr, Statement, Value, Visit, VisitMut, Visitor};
use std::ops::ControlFlow;

mod catalog;
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
    db.execute_batch(&format!("CREATE OR REPLACE MACRO {}(v) AS v", lower::MARK))?;
    Ok(())
}

pub(super) fn rewrite_statement(db: &duckdb::Connection, statement: &mut Statement) -> Result<()> {
    if matches!(
        statement,
        Statement::Query(_)
            | Statement::Insert(_)
            | Statement::Update(_)
            | Statement::Delete(_)
            | Statement::Merge(_)
            | Statement::CreateView(_)
    ) {
        let ordered = order::sites(statement);
        rewrite(db, statement, ordered)?;
    }
    Ok(())
}

/// Subqueries of scalar evaluations (IF, WHILE, SET, RETURN), which have no
/// statement pass; inside statements this repeats that pass, idempotently.
pub(super) fn rewrite_expr(db: &duckdb::Connection, expr: &mut Expr) -> Result<()> {
    if matches!(
        expr,
        Expr::Subquery(_) | Expr::Exists { .. } | Expr::InSubquery { .. }
    ) {
        rewrite(db, expr, false)?;
    }
    Ok(())
}

fn rewrite<T: Visit + VisitMut + 'static>(
    db: &duckdb::Connection,
    node: &mut T,
    ordered: bool,
) -> Result<()> {
    if !ordered && !predicates(node) {
        return Ok(());
    }
    let mut catalog = catalog::Catalog::load(db, node)?;
    if !catalog.has_carriers() {
        return Ok(());
    }
    mark::rewrite(&catalog, node);
    if pin::sites(node) && pin::rewrite(&catalog, node, false) {
        catalog.declare(db, node)?;
        pin::rewrite(&catalog, node, true);
    }
    if ordered && let Some(statement) = (node as &mut dyn std::any::Any).downcast_mut::<Statement>()
    {
        order::rewrite(&catalog, statement);
    }
    Ok(())
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
                        "ISNULL" | "COALESCE" | "IIF"
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
