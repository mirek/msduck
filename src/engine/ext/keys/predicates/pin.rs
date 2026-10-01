//! Carrier columns among the alternatives of ISNULL, COALESCE, IIF, CASE
//! and set operations.
//!
//! DuckDB gives such an expression one type, and cannot unify a carrier
//! STRUCT with VARCHAR text (a literal, parameter or VARCHAR column). When
//! the alternatives mix a carrier column with other values, each carrier
//! column becomes `CAST(column AS <its declaration>)`, which keeps the
//! logical type (and so the result metadata) while the backend conversion
//! produces text ([`super::lower`]).
use super::catalog::Catalog;
use sqlparser::ast::*;
use std::ops::ControlFlow;

fn null(expr: &Expr) -> bool {
    match expr {
        Expr::Nested(inner) => null(inner),
        Expr::Value(value) => matches!(value.value, Value::Null),
        _ => false,
    }
}

/// Pin the carrier columns among `values` when they mix with other values.
fn pin(catalog: &Catalog, values: Vec<&mut Expr>) {
    let declared: Vec<Option<DataType>> = values
        .iter()
        .map(|v| catalog.carrier(v).and_then(|c| c.declared.clone()))
        .collect();
    let mixed = declared.iter().any(Option::is_some)
        && values
            .iter()
            .zip(&declared)
            .any(|(v, d)| d.is_none() && !null(v));
    if !mixed {
        return;
    }
    for (value, declared) in values.into_iter().zip(declared) {
        if let Some(data_type) = declared {
            let column = std::mem::replace(value, Expr::Value(Value::Null.into()));
            *value = Expr::Cast {
                kind: CastKind::Cast,
                expr: Box::new(column),
                data_type,
                format: None,
            };
        }
    }
}

fn arguments(function: &mut Function) -> Vec<&mut Expr> {
    let FunctionArguments::List(list) = &mut function.args else {
        return Vec::new();
    };
    list.args
        .iter_mut()
        .filter_map(|arg| match arg {
            FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => Some(e),
            _ => None,
        })
        .collect()
}

/// Whether `statement` has an expression this module may change.
pub(super) fn sites<T: Visit>(node: &T) -> bool {
    struct Find;
    impl Visitor for Find {
        type Break = ();
        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
            match expr {
                Expr::Case { .. } => ControlFlow::Break(()),
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

/// The projections of a set operation's branches.
fn projections(body: &SetExpr) -> Vec<&Vec<SelectItem>> {
    match body {
        SetExpr::Select(select) => vec![&select.projection],
        SetExpr::SetOperation { left, right, .. } => {
            let mut all = projections(left);
            all.extend(projections(right));
            all
        }
        SetExpr::Query(query) => projections(&query.body),
        _ => Vec::new(),
    }
}

fn item_expr(item: &SelectItem) -> Option<&Expr> {
    match item {
        SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => Some(expr),
        _ => None,
    }
}

/// Pin the carrier columns at `positions` of every branch, keeping the
/// output name of a bare column.
fn pin_branches(catalog: &Catalog, body: &mut SetExpr, positions: &[usize]) {
    match body {
        SetExpr::Select(select) => {
            for &position in positions {
                let Some(item) = select.projection.get_mut(position) else {
                    continue;
                };
                let Some(data_type) = item_expr(item)
                    .and_then(|e| catalog.carrier(e))
                    .and_then(|c| c.declared.clone())
                else {
                    continue;
                };
                let (expr, alias) =
                    match std::mem::replace(item, SelectItem::Wildcard(Default::default())) {
                        SelectItem::UnnamedExpr(expr) => {
                            let alias = match &expr {
                                Expr::Identifier(ident) => Some(ident.clone()),
                                Expr::CompoundIdentifier(parts) => parts.last().cloned(),
                                _ => None,
                            };
                            (expr, alias)
                        }
                        SelectItem::ExprWithAlias { expr, alias } => (expr, Some(alias)),
                        _ => unreachable!("a pinned item is an expression"),
                    };
                let expr = Expr::Cast {
                    kind: CastKind::Cast,
                    expr: Box::new(expr),
                    data_type,
                    format: None,
                };
                *item = match alias {
                    Some(alias) => SelectItem::ExprWithAlias { expr, alias },
                    None => SelectItem::UnnamedExpr(expr),
                };
            }
        }
        SetExpr::SetOperation { left, right, .. } => {
            pin_branches(catalog, left, positions);
            pin_branches(catalog, right, positions);
        }
        SetExpr::Query(query) => pin_branches(catalog, &mut query.body, positions),
        _ => {}
    }
}

pub(super) fn rewrite<T: VisitMut>(catalog: &Catalog, node: &mut T) {
    struct Pin<'a>(&'a Catalog);
    impl VisitorMut for Pin<'_> {
        type Break = ();
        fn pre_visit_query(&mut self, query: &mut Query) -> ControlFlow<()> {
            if !matches!(query.body.as_ref(), SetExpr::SetOperation { .. }) {
                return ControlFlow::Continue(());
            }
            let projections = projections(&query.body);
            let width = projections.first().map_or(0, |p| p.len());
            if projections.iter().any(|p| p.len() != width) {
                return ControlFlow::Continue(());
            }
            let positions: Vec<usize> = (0..width)
                .filter(|&position| {
                    let values: Vec<Option<&Expr>> = projections
                        .iter()
                        .map(|p| item_expr(&p[position]))
                        .collect();
                    let carrier = |v: &Option<&Expr>| {
                        v.and_then(|e| self.0.carrier(e))
                            .is_some_and(|c| c.declared.is_some())
                    };
                    values.iter().any(carrier)
                        && values.iter().any(|v| !carrier(v) && !v.is_some_and(null))
                })
                .collect();
            if !positions.is_empty() {
                pin_branches(self.0, &mut query.body, &positions);
            }
            ControlFlow::Continue(())
        }
        fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
            match expr {
                Expr::Case {
                    conditions,
                    else_result,
                    ..
                } => {
                    let mut values: Vec<&mut Expr> =
                        conditions.iter_mut().map(|c| &mut c.result).collect();
                    if let Some(otherwise) = else_result {
                        values.push(otherwise);
                    }
                    pin(self.0, values);
                }
                Expr::Function(f) => match f.name.to_string().to_ascii_uppercase().as_str() {
                    "ISNULL" | "COALESCE" => pin(self.0, arguments(f)),
                    "IIF" => pin(self.0, arguments(f).into_iter().skip(1).collect()),
                    _ => {}
                },
                _ => {}
            }
            ControlFlow::Continue(())
        }
    }
    let _ = node.visit(&mut Pin(catalog));
}
