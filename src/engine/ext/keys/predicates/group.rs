//! GROUP BY and SELECT DISTINCT over character columns under the
//! case-insensitive default collation.
//!
//! DuckDB groups Unicode carriers by their STRUCT bytes, and VARCHAR values
//! by the session's `nocase` collation, which still tells trailing spaces
//! apart. A grouping column of a relation in the statement (a carrier or
//! VARCHAR column without a collation of its own) therefore groups by its
//! sort key instead ([`GROUP`], lowered to the key in [`super::lower`]),
//! like SQL Server's equality. Its other references after grouping (the
//! select list, HAVING, ORDER BY and window functions) become `MIN` of the
//! column, which picks one member of the group, as SQL Server shows one of
//! the equal values; ORDER BY sorts by the key.
//!
//! `SELECT DISTINCT` over column references becomes the same grouping.
//! Other shapes (grouping sets, expressions, wildcards, aggregates with
//! DISTINCT projections) are left unchanged.
use super::catalog::Catalog;
use super::lower::GROUP;
use sqlparser::ast::*;
use std::ops::ControlFlow;

/// Whether `expr` is a column reference that groups by its sort key.
/// Every column of that name in the statement's relations must be one, so
/// that a same-named column of another type or collation is not keyed.
fn keyed(catalog: &Catalog, expr: &Expr) -> bool {
    matches!(expr, Expr::Identifier(_) | Expr::CompoundIdentifier(_)) && catalog.default_text(expr)
}

fn last_name(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Identifier(ident) => Some(ident.value.to_lowercase()),
        Expr::CompoundIdentifier(parts) => parts.last().map(|p| p.value.to_lowercase()),
        _ => None,
    }
}

/// Whether two column references name the same column: the same last
/// name, and the same qualifier when both have one.
fn same(left: &Expr, right: &Expr) -> bool {
    let qualifier = |expr: &Expr| match expr {
        Expr::CompoundIdentifier(parts) if parts.len() >= 2 => {
            Some(parts[parts.len() - 2].value.to_lowercase())
        }
        _ => None,
    };
    last_name(left).is_some()
        && last_name(left) == last_name(right)
        && match (qualifier(left), qualifier(right)) {
            (Some(a), Some(b)) => a == b,
            _ => true,
        }
}

fn group_key(column: Expr) -> Expr {
    msduck_sql::expr::unary_function(GROUP, column)
}

fn minimum(column: Expr) -> Expr {
    msduck_sql::expr::unary_function("MIN", column)
}

const AGGREGATES: &[&str] = &[
    "COUNT",
    "COUNT_BIG",
    "SUM",
    "AVG",
    "MIN",
    "MAX",
    "STRING_AGG",
    "STDEV",
    "STDEVP",
    "VAR",
    "VARP",
    "CHECKSUM_AGG",
    "GROUPING",
    "GROUPING_ID",
    "APPROX_COUNT_DISTINCT",
    "PERCENTILE_CONT",
    "PERCENTILE_DISC",
];

fn aggregate(function: &Function) -> bool {
    function.over.is_none()
        && AGGREGATES.contains(&function.name.to_string().to_ascii_uppercase().as_str())
}

/// Replace references to `columns` outside aggregate arguments and nested
/// queries with `replace(column)`.
fn substitute(expr: &mut Expr, columns: &[Expr], replace: &dyn Fn(Expr) -> Expr) {
    struct Substitute<'a> {
        columns: &'a [Expr],
        replace: &'a dyn Fn(Expr) -> Expr,
        /// Depth inside aggregates or nested queries.
        skip: usize,
    }
    impl VisitorMut for Substitute<'_> {
        type Break = ();
        fn pre_visit_query(&mut self, _: &mut Query) -> ControlFlow<()> {
            self.skip += 1;
            ControlFlow::Continue(())
        }
        fn post_visit_query(&mut self, _: &mut Query) -> ControlFlow<()> {
            self.skip -= 1;
            ControlFlow::Continue(())
        }
        fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
            if matches!(expr, Expr::Function(f) if aggregate(f)) {
                self.skip += 1;
            }
            ControlFlow::Continue(())
        }
        fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
            if matches!(expr, Expr::Function(f) if aggregate(f)) {
                self.skip -= 1;
                return ControlFlow::Continue(());
            }
            if self.skip == 0 && self.columns.iter().any(|c| same(c, expr)) {
                let column = std::mem::replace(expr, Expr::Value(Value::Null.into()));
                *expr = (self.replace)(column);
            }
            ControlFlow::Continue(())
        }
    }
    let _ = VisitMut::visit(
        expr,
        &mut Substitute {
            columns,
            replace,
            skip: 0,
        },
    );
}

/// Whether `expr` refers to one of `columns`; with `nested`, only inside
/// nested queries (possible outer references).
fn references(expr: &Expr, columns: &[Expr], nested: bool) -> bool {
    struct Find<'a>(&'a [Expr], bool, usize);
    impl Visitor for Find<'_> {
        type Break = ();
        fn pre_visit_query(&mut self, _: &Query) -> ControlFlow<()> {
            self.2 += 1;
            ControlFlow::Continue(())
        }
        fn post_visit_query(&mut self, _: &Query) -> ControlFlow<()> {
            self.2 -= 1;
            ControlFlow::Continue(())
        }
        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
            if (!self.1 || self.2 > 0) && self.0.iter().any(|c| same(c, expr)) {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        }
    }
    expr.visit(&mut Find(columns, nested, 0)).is_break()
}

/// The select-list expression an ORDER BY item names by alias or ordinal.
fn projected<'a>(item: &Expr, projection: &'a [SelectItem]) -> Option<&'a Expr> {
    match item {
        Expr::Identifier(ident) => projection.iter().find_map(|p| match p {
            SelectItem::ExprWithAlias { expr, alias }
                if alias.value.eq_ignore_ascii_case(&ident.value) =>
            {
                Some(expr)
            }
            _ => None,
        }),
        Expr::Value(value) => {
            let Value::Number(ordinal, _) = &value.value else {
                return None;
            };
            let index = ordinal.parse::<usize>().ok()?.checked_sub(1)?;
            match projection.get(index)? {
                SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => {
                    Some(expr)
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// Rewrite one query whose body is a SELECT; see the module documentation.
fn query(catalog: &Catalog, query: &mut Query) {
    let SetExpr::Select(select) = query.body.as_mut() else {
        return;
    };
    let distinct = matches!(select.distinct, Some(Distinct::Distinct));
    let GroupByExpr::Expressions(existing, modifiers) = &select.group_by else {
        return;
    };
    if !modifiers.is_empty() {
        return;
    }
    let groups: Vec<Expr> = if distinct {
        // DISTINCT over plain column references only.
        if !existing.is_empty() || select.having.is_some() {
            return;
        }
        let Some(projected) = select
            .projection
            .iter()
            .map(|item| match item {
                SelectItem::UnnamedExpr(
                    expr @ (Expr::Identifier(_) | Expr::CompoundIdentifier(_)),
                )
                | SelectItem::ExprWithAlias {
                    expr: expr @ (Expr::Identifier(_) | Expr::CompoundIdentifier(_)),
                    ..
                } => Some(expr.clone()),
                _ => None,
            })
            .collect::<Option<Vec<Expr>>>()
        else {
            return;
        };
        projected
    } else {
        existing.clone()
    };
    let columns: Vec<Expr> = groups
        .iter()
        .filter(|e| keyed(catalog, e))
        .cloned()
        .collect();
    if columns.is_empty() {
        return;
    }
    // Grouping by the column and by another expression of it keeps
    // DuckDB's grouping.
    if groups
        .iter()
        .any(|g| !columns.iter().any(|c| same(c, g)) && references(g, &columns, false))
    {
        return;
    }
    // Nested queries may refer to the grouped value; leave them be.
    let outer = select
        .projection
        .iter()
        .filter_map(|item| match item {
            SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => Some(expr),
            _ => None,
        })
        .chain(select.having.as_ref())
        .any(|expr| references(expr, &columns, true));
    if outer {
        return;
    }
    let groups = groups
        .into_iter()
        .map(|group| {
            if columns.iter().any(|c| same(c, &group)) {
                group_key(group)
            } else {
                group
            }
        })
        .collect();
    select.group_by = GroupByExpr::Expressions(groups, vec![]);
    if distinct {
        select.distinct = None;
    }
    // ORDER BY items naming a grouped column, directly or through the
    // select list, sort by its key.
    if let Some(order) = &mut query.order_by
        && let OrderByKind::Expressions(items) = &mut order.kind
    {
        for item in items.iter_mut() {
            match projected(&item.expr, &select.projection).cloned() {
                // A select-list alias or ordinal: sort by the grouped
                // column's key, or leave other items to the select list.
                Some(target) => {
                    if columns.iter().any(|c| same(c, &target)) {
                        item.expr = group_key(target);
                    }
                }
                None if columns.iter().any(|c| same(c, &item.expr)) => {
                    item.expr = group_key(item.expr.clone());
                }
                None => substitute(&mut item.expr, &columns, &minimum),
            }
        }
    }
    for item in &mut select.projection {
        match item {
            SelectItem::UnnamedExpr(expr) => {
                let name = columns
                    .iter()
                    .any(|c| same(c, expr))
                    .then(|| match &*expr {
                        Expr::Identifier(ident) => Some(ident.clone()),
                        Expr::CompoundIdentifier(parts) => parts.last().cloned(),
                        _ => None,
                    })
                    .flatten();
                substitute(expr, &columns, &minimum);
                if let Some(alias) = name {
                    *item = SelectItem::ExprWithAlias {
                        expr: expr.clone(),
                        alias,
                    };
                }
            }
            SelectItem::ExprWithAlias { expr, .. } => substitute(expr, &columns, &minimum),
            _ => {}
        }
    }
    if let Some(having) = &mut select.having {
        substitute(having, &columns, &minimum);
    }
}

/// Whether `statement` groups or selects DISTINCT anywhere.
pub(super) fn sites(statement: &Statement) -> bool {
    struct Find;
    impl Visitor for Find {
        type Break = ();
        fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<()> {
            if let SetExpr::Select(select) = query.body.as_ref()
                && (select.distinct.is_some()
                    || !matches!(&select.group_by, GroupByExpr::Expressions(g, _) if g.is_empty()))
            {
                return ControlFlow::Break(());
            }
            ControlFlow::Continue(())
        }
    }
    Visit::visit(statement, &mut Find).is_break()
}

pub(super) fn rewrite(catalog: &Catalog, statement: &mut Statement) {
    struct Rewrite<'a>(&'a Catalog, Vec<Catalog>);
    impl Rewrite<'_> {
        fn catalog(&self) -> &Catalog {
            self.1.last().unwrap_or(self.0)
        }
    }
    impl VisitorMut for Rewrite<'_> {
        type Break = ();
        fn pre_visit_query(&mut self, q: &mut Query) -> ControlFlow<()> {
            let mut scope = self.catalog().query_scope(q);
            if let SetExpr::Select(select) = q.body.as_ref() {
                scope = scope.select_scope(select);
            }
            self.1.push(scope);
            query(self.catalog(), q);
            ControlFlow::Continue(())
        }
        fn post_visit_query(&mut self, _: &mut Query) -> ControlFlow<()> {
            self.1.pop();
            ControlFlow::Continue(())
        }
    }
    let _ = VisitMut::visit(statement, &mut Rewrite(catalog, Vec::new()));
}
