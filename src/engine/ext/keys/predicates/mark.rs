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
use super::catalog::{Catalog, Resolution};
use super::lower::{MARK, MAYBE, MEMBER};
use sqlparser::ast::*;
use std::ops::ControlFlow;

fn marked(expr: &Expr) -> bool {
    matches!(expr, Expr::Function(f) if f.name.to_string() == MARK)
}

fn maybe(expr: &Expr) -> bool {
    matches!(expr, Expr::Function(f) if f.name.to_string() == MAYBE)
}

/// Whether `expr` is known not to be text: a column of another backend
/// type, or a number. A carrier compared with it fails either way, so it
/// needs no dispatch, and an outer join on it stays a hash join.
fn not_text(catalog: &Catalog, expr: &Expr) -> bool {
    match expr {
        Expr::Nested(inner) => not_text(catalog, inner),
        Expr::Value(value) => matches!(value.value, Value::Number(..)),
        Expr::UnaryOp { expr, .. } => not_text(catalog, expr),
        Expr::Identifier(ident) if ident.value.starts_with('@') => false,
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) => {
            matches!(catalog.resolve(expr), Resolution::Plain { text: false })
        }
        _ => false,
    }
}

/// Whether `expr` is a column reference whose type is unknown here: a
/// derived-table, CTE or outer column. The backend lowering checks its
/// type; columns known not to be carriers are left alone.
fn unknown(catalog: &Catalog, expr: &Expr) -> bool {
    match expr {
        Expr::Nested(inner) => unknown(catalog, inner),
        // Variables and parameters are VARCHAR text in the backend.
        Expr::Identifier(ident) if ident.value.starts_with('@') => false,
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) => {
            matches!(catalog.resolve(expr), Resolution::Unknown)
        }
        _ => false,
    }
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
/// of one (ISNULL and COALESCE take the type of their first argument), a
/// CASE whose results are such text or string literals, or a scalar
/// subquery selecting one.
fn unicode(catalog: &Catalog, expr: &Expr) -> bool {
    match expr {
        Expr::Nested(inner) => unicode(catalog, inner),
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) => catalog.carrier(expr).is_some(),
        Expr::Substring { expr, .. } | Expr::Trim { expr, .. } => unicode(catalog, expr),
        Expr::Case {
            conditions,
            else_result,
            ..
        } => {
            // Every result must be text: SQL Server types the CASE by
            // precedence, so `CASE … THEN n ELSE 0 END` is an int.
            let results: Vec<&Expr> = conditions
                .iter()
                .map(|c| &c.result)
                .chain(else_result.as_deref())
                .filter(|r| !matches!(r, Expr::Value(v) if matches!(v.value, Value::Null)))
                .collect();
            !results.is_empty()
                && results.iter().any(|r| unicode(catalog, r))
                && results.iter().all(|r| {
                    unicode(catalog, r)
                        || matches!(r, Expr::Value(v) if matches!(v.value,
                            Value::SingleQuotedString(_) | Value::NationalStringLiteral(_)))
                })
        }
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

/// Mark the operands of one operation when any of them is Unicode text;
/// otherwise mark operands of unknown type for the backend `typeof`
/// dispatch. Explicitly collated operations are left to the COLLATE
/// handling.
fn mark(catalog: &Catalog, operands: Vec<&mut Expr>) {
    if operands.iter().any(|o| collated(o) || marked(o)) {
        return;
    }
    // A subquery pass can type a column the statement pass could not.
    let unwrap = |e: &Expr| -> Expr {
        match e {
            Expr::Function(f) if maybe(e) => match &f.args {
                FunctionArguments::List(list) => match list.args.as_slice() {
                    [FunctionArg::Unnamed(FunctionArgExpr::Expr(inner))] => inner.clone(),
                    _ => e.clone(),
                },
                _ => e.clone(),
            },
            _ => e.clone(),
        }
    };
    let text = operands.iter().any(|o| unicode(catalog, &unwrap(o)));
    let typed = operands.iter().any(|o| not_text(catalog, o));
    for operand in operands {
        let name = if text {
            MARK
        } else if !typed && !maybe(operand) && unknown(catalog, operand) {
            MAYBE
        } else {
            continue;
        };
        let inner = unwrap(operand);
        *operand = msduck_sql::expr::unary_function(name, inner);
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
                // NULLIF(a, b) becomes CASE WHEN a = b …; marking `b` alone
                // keeps `a`, which gives the result its type, as written.
                Expr::Function(f) if f.name.to_string().eq_ignore_ascii_case("NULLIF") => {
                    if let FunctionArguments::List(list) = &mut f.args
                        && let [
                            FunctionArg::Unnamed(FunctionArgExpr::Expr(first)),
                            FunctionArg::Unnamed(FunctionArgExpr::Expr(second)),
                        ] = list.args.as_mut_slice()
                        && !collated(first)
                        && !collated(second)
                        && !marked(second)
                        && unicode(self.0, first)
                    {
                        let inner = std::mem::replace(second, Expr::Value(Value::Null.into()));
                        *second = msduck_sql::expr::unary_function(MARK, inner);
                    }
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
