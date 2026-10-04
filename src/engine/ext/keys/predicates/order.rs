//! ORDER BY over Unicode carrier columns.
//!
//! DuckDB orders the carrier STRUCT by its little-endian payload bytes, so
//! U+0100 sorts before `a` and trailing spaces break ties. Each ORDER BY
//! item naming a carrier column of a relation in the statement (directly,
//! through a select-list alias or by ordinal) becomes two items: the
//! carrier's sort key, then the value itself for other types. The items are
//! markers here; [`super::lower`] turns them into `typeof` dispatches, so a
//! same-named column of another type keeps its own order. DISTINCT and set
//! operations, whose ORDER BY must name output columns, are left unchanged.
//! A carrier column declared with a collation of its own (case-sensitive,
//! binary or accent-insensitive) orders by that collation through an
//! explicit COLLATE instead.
use super::catalog::Catalog;
use super::lower::{SORT, TIE};
use sqlparser::ast::*;
use std::collections::HashSet;
use std::ops::ControlFlow;

/// The column an ORDER BY item names, if it names one.
fn column<'a>(item: &'a Expr, select: &'a Select) -> Option<&'a Expr> {
    let named = |expr: &'a Expr| match expr {
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) => Some(expr),
        _ => None,
    };
    match item {
        Expr::Identifier(ident) => {
            for projected in &select.projection {
                if let SelectItem::ExprWithAlias { expr, alias } = projected
                    && alias.value.eq_ignore_ascii_case(&ident.value)
                {
                    return named(expr);
                }
            }
            Some(item)
        }
        Expr::CompoundIdentifier(_) => Some(item),
        Expr::Value(value) => {
            let Value::Number(ordinal, _) = &value.value else {
                return None;
            };
            let index = ordinal.parse::<usize>().ok()?.checked_sub(1)?;
            let prefix = select.projection.get(..=index)?;
            if prefix.iter().any(|p| {
                matches!(
                    p,
                    SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(..)
                )
            }) {
                return None;
            }
            match &prefix[index] {
                SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => {
                    named(expr)
                }
                _ => None,
            }
        }
        _ => None,
    }
}

fn last_name(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Identifier(ident) => Some(ident.value.to_lowercase()),
        Expr::CompoundIdentifier(parts) => parts.last().map(|p| p.value.to_lowercase()),
        _ => None,
    }
}

/// Queries whose ORDER BY may be rewritten, with the column names they order by.
fn candidates(statement: &Statement) -> HashSet<String> {
    struct Find(HashSet<String>);
    impl Visitor for Find {
        type Break = ();
        fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<()> {
            if let (Some(order), SetExpr::Select(select)) = (&query.order_by, query.body.as_ref())
                && select.distinct.is_none()
                && let OrderByKind::Expressions(items) = &order.kind
            {
                for item in items {
                    if let Some(name) = column(&item.expr, select).and_then(last_name) {
                        self.0.insert(name);
                    }
                }
            }
            ControlFlow::Continue(())
        }
    }
    let mut find = Find(HashSet::new());
    let _ = Visit::visit(statement, &mut find);
    find.0
}

fn marker(name: &str, value: Expr) -> Expr {
    let mut call = msduck_sql::expr::unary_function(name, value);
    if let Expr::Function(f) = &mut call {
        f.name = ObjectName::from(vec![Ident::new(name)]);
    }
    call
}

/// Whether `statement` has an ORDER BY this module may change.
pub(super) fn sites(statement: &Statement) -> bool {
    !candidates(statement).is_empty()
}

pub(super) fn rewrite(catalog: &Catalog, statement: &mut Statement) {
    let carriers: HashSet<String> = candidates(statement)
        .into_iter()
        .filter(|name| catalog.carrier_named(name))
        .collect();
    if carriers.is_empty() {
        return;
    }
    struct Rewrite<'a>(&'a Catalog, HashSet<String>, Vec<Catalog>);
    impl Rewrite<'_> {
        fn catalog(&self) -> &Catalog {
            self.2.last().unwrap_or(self.0)
        }
    }
    impl VisitorMut for Rewrite<'_> {
        type Break = ();
        fn pre_visit_query(&mut self, query: &mut Query) -> ControlFlow<()> {
            let mut scope = self.catalog().query_scope(query);
            if let SetExpr::Select(select) = query.body.as_ref() {
                scope = scope.select_scope(select);
            }
            self.2.push(scope);
            let (Some(order), SetExpr::Select(select)) = (&mut query.order_by, query.body.as_ref())
            else {
                return ControlFlow::Continue(());
            };
            if select.distinct.is_some() {
                return ControlFlow::Continue(());
            }
            let OrderByKind::Expressions(items) = &mut order.kind else {
                return ControlFlow::Continue(());
            };
            let mut rewritten = Vec::with_capacity(items.len());
            for item in items.drain(..) {
                let target = column(&item.expr, select)
                    .filter(|c| {
                        last_name(c).is_some_and(|n| self.1.contains(&n))
                            && self.catalog().carrier(c).is_some()
                    })
                    .cloned();
                // The collation of the carrier column the item names, when it
                // compares differently from the default.
                let collation = target
                    .as_ref()
                    .filter(|t| self.catalog().carrier(t).is_some())
                    .and_then(|t| self.catalog().collation(t))
                    .filter(|name| {
                        use msduck_sql::dialect::ext::keys::collation::{Sensitivity, sensitivity};
                        sensitivity(name) == Some(Sensitivity::Other)
                    })
                    .map(str::to_owned);
                match (target, collation) {
                    // A column collation of its own orders through the
                    // explicit COLLATE lowering.
                    (Some(target), Some(collation)) => rewritten.push(OrderByExpr {
                        expr: Expr::Collate {
                            expr: Box::new(target),
                            collation: ObjectName::from(vec![Ident::new(collation)]),
                        },
                        options: item.options,
                        with_fill: item.with_fill,
                    }),
                    (Some(target), None) => {
                        rewritten.push(OrderByExpr {
                            expr: marker(SORT, target.clone()),
                            options: item.options.clone(),
                            with_fill: None,
                        });
                        rewritten.push(OrderByExpr {
                            expr: marker(TIE, target),
                            options: item.options,
                            with_fill: item.with_fill,
                        });
                    }
                    (None, _) => rewritten.push(item),
                }
            }
            *items = rewritten;
            ControlFlow::Continue(())
        }
        fn post_visit_query(&mut self, _: &mut Query) -> ControlFlow<()> {
            self.2.pop();
            ControlFlow::Continue(())
        }
    }
    let _ = VisitMut::visit(statement, &mut Rewrite(catalog, carriers, Vec::new()));
}
