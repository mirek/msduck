//! Operands known to be Unicode text before translation.
//!
//! The backend lowering ([`super::lower`]) dispatches on `typeof`, keeping
//! the original operation for operands that are not carriers. DuckDB binds
//! both branches, and refuses to bind an ordering comparison, IN or LIKE
//! between a carrier and a VARCHAR expression (only literals convert), so
//! the dispatch alone fails for `column < @variable` or `column = other`.
//! Operands that resolve to carrier columns, directly or through character
//! functions, are wrapped in a marker here; the lowering compares marked
//! operations as Unicode text without the dispatch.
use super::catalog::Catalog;
use super::lower::{MARK, MEMBER};
use sqlparser::ast::*;
use std::ops::ControlFlow;

fn marked(expr: &Expr) -> bool {
    matches!(expr, Expr::Function(f) if f.name.to_string() == MARK)
}

fn first_argument(function: &Function) -> Option<&Expr> {
    let FunctionArguments::List(list) = &function.args else {
        return None;
    };
    match list.args.first()? {
        FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => Some(e),
        _ => None,
    }
}

/// Whether `expr` is Unicode text: a carrier column, a character function
/// of one (ISNULL and COALESCE take the type of their first argument), or
/// a scalar subquery selecting one.
fn unicode(catalog: &Catalog, expr: &Expr) -> bool {
    match expr {
        Expr::Nested(inner) => unicode(catalog, inner),
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) => catalog.carrier(expr).is_some(),
        Expr::Substring { expr, .. } | Expr::Trim { expr, .. } => unicode(catalog, expr),
        Expr::Subquery(query) => match query.body.as_ref() {
            SetExpr::Select(select) => match select.projection.as_slice() {
                [SelectItem::UnnamedExpr(p) | SelectItem::ExprWithAlias { expr: p, .. }] => {
                    unicode(catalog, p)
                }
                _ => false,
            },
            _ => false,
        },
        Expr::Function(f) => {
            matches!(
                f.name.to_string().to_ascii_uppercase().as_str(),
                "LEFT"
                    | "RIGHT"
                    | "UPPER"
                    | "LOWER"
                    | "LTRIM"
                    | "RTRIM"
                    | "TRIM"
                    | "SUBSTRING"
                    | "REPLACE"
                    | "REVERSE"
                    | "ISNULL"
                    | "COALESCE"
            ) && first_argument(f).is_some_and(|a| unicode(catalog, a))
        }
        _ => false,
    }
}

fn collated(expr: &Expr) -> bool {
    match expr {
        Expr::Nested(inner) => collated(inner),
        Expr::Collate { .. } => true,
        _ => false,
    }
}

/// Mark the operands of one operation when any of them is Unicode text.
/// Explicitly collated operations are left to the COLLATE handling.
fn mark(catalog: &Catalog, operands: Vec<&mut Expr>) {
    if operands.iter().any(|o| collated(o) || marked(o))
        || !operands.iter().any(|o| unicode(catalog, o))
    {
        return;
    }
    for operand in operands {
        let inner = std::mem::replace(operand, Expr::Value(Value::Null.into()));
        *operand = msduck_sql::expr::unary_function(MARK, inner);
    }
}

pub(super) fn rewrite<T: VisitMut>(catalog: &Catalog, node: &mut T) {
    struct Mark<'a>(&'a Catalog);
    impl VisitorMut for Mark<'_> {
        type Break = ();
        fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
            match expr {
                Expr::BinaryOp {
                    left,
                    op:
                        BinaryOperator::Eq
                        | BinaryOperator::NotEq
                        | BinaryOperator::Lt
                        | BinaryOperator::Gt
                        | BinaryOperator::LtEq
                        | BinaryOperator::GtEq,
                    right,
                } => mark(self.0, vec![left, right]),
                Expr::Between {
                    expr, low, high, ..
                } => mark(self.0, vec![expr, low, high]),
                // The escape stays as written: its checks look for a literal.
                Expr::Like {
                    expr,
                    pattern,
                    any: false,
                    ..
                } => mark(self.0, vec![expr, pattern]),
                Expr::InList { expr, list, .. } => {
                    let mut operands = vec![expr.as_mut()];
                    operands.extend(list.iter_mut());
                    mark(self.0, operands)
                }
                Expr::Case {
                    operand: Some(operand),
                    conditions,
                    ..
                } => {
                    let mut operands = vec![operand.as_mut()];
                    operands.extend(conditions.iter_mut().map(|c| &mut c.condition));
                    mark(self.0, operands)
                }
                Expr::InSubquery {
                    expr: value,
                    subquery,
                    ..
                } => {
                    let projected = match subquery.body.as_ref() {
                        SetExpr::Select(select) => match select.projection.as_slice() {
                            [
                                SelectItem::UnnamedExpr(p)
                                | SelectItem::ExprWithAlias { expr: p, .. },
                            ] => Some(p),
                            _ => None,
                        },
                        _ => None,
                    };
                    let member =
                        matches!(value.as_ref(), Expr::Function(f) if f.name.to_string() == MEMBER);
                    if !member
                        && !collated(value)
                        && (unicode(self.0, value) || projected.is_some_and(|p| unicode(self.0, p)))
                    {
                        let inner =
                            std::mem::replace(value.as_mut(), Expr::Value(Value::Null.into()));
                        **value = msduck_sql::expr::unary_function(MEMBER, inner);
                    }
                }
                _ => {}
            }
            ControlFlow::Continue(())
        }
    }
    let _ = VisitMut::visit(node, &mut Mark(catalog));
}
