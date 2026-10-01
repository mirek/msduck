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

/// Whether `values` mix a carrier column with other (non-NULL) values.
fn mixed(catalog: &Catalog, values: &[&Expr]) -> bool {
    let carriers: Vec<bool> = values
        .iter()
        .map(|v| catalog.carrier(v).is_some())
        .collect();
    carriers.iter().any(|c| *c) && values.iter().zip(&carriers).any(|(v, c)| !c && !null(v))
}

/// Pin the carrier columns among `values` when they mix with other values;
/// a dry run only reports whether any would be.
fn pin(catalog: &Catalog, values: Vec<&mut Expr>, apply: bool) -> bool {
    if !mixed(catalog, &values.iter().map(|v| &**v).collect::<Vec<_>>()) {
        return false;
    }
    if !apply {
        return true;
    }
    for value in values {
        if let Some(data_type) = catalog.carrier(value).and_then(|c| c.declared.clone()) {
            let column = std::mem::replace(value, Expr::Value(Value::Null.into()));
            *value = Expr::Cast {
                kind: CastKind::Cast,
                expr: Box::new(column),
                data_type,
                format: None,
            };
        }
    }
    true
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

/// Pin the mixed carrier columns of `node`, or with `apply` false only
/// report whether there are any (before their declarations are loaded).
pub(super) fn rewrite<T: VisitMut>(catalog: &Catalog, node: &mut T, apply: bool) -> bool {
    struct Pin<'a> {
        catalog: &'a Catalog,
        apply: bool,
        found: bool,
    }
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
                    let values: Option<Vec<&Expr>> = projections
                        .iter()
                        .map(|p| item_expr(&p[position]))
                        .collect();
                    values.is_some_and(|values| mixed(self.catalog, &values))
                })
                .collect();
            if !positions.is_empty() {
                self.found = true;
                if self.apply {
                    pin_branches(self.catalog, &mut query.body, &positions);
                }
            }
            ControlFlow::Continue(())
        }
        fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
            let found = match expr {
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
                    pin(self.catalog, values, self.apply)
                }
                Expr::Function(f) => match f.name.to_string().to_ascii_uppercase().as_str() {
                    "ISNULL" | "COALESCE" => pin(self.catalog, arguments(f), self.apply),
                    "IIF" => pin(
                        self.catalog,
                        arguments(f).into_iter().skip(1).collect(),
                        self.apply,
                    ),
                    _ => false,
                },
                _ => false,
            };
            self.found |= found;
            ControlFlow::Continue(())
        }
    }
    let mut pin = Pin {
        catalog,
        apply,
        found: false,
    };
    let _ = node.visit(&mut pin);
    pin.found
}
