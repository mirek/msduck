//! Sort projected variants by their numeric values without repeating projections.
use sqlparser::{ast::*, dialect::GenericDialect, parser::Parser};

pub fn column(index: usize) -> Expr {
    Expr::CompoundIdentifier(vec![
        Ident::new("__variant_order_source"),
        Ident::new(format!("__variant_order_{index}")),
    ])
}

pub fn wrap(query: &mut Query, names: &[String], keys: &[(usize, bool)], extra: Vec<Expr>) {
    let aliases = (0..names.len() + extra.len())
        .map(|i| format!("__variant_order_{i}"))
        .collect::<Vec<_>>();
    let sql = format!(
        "SELECT * FROM (SELECT NULL) AS __variant_order_source({})",
        aliases.join(",")
    );
    let Statement::Query(mut wrapper) = Parser::parse_sql(&GenericDialect {}, &sql)
        .expect("generated variant order wrapper")
        .remove(0)
    else {
        unreachable!()
    };
    let SetExpr::Select(outer) = wrapper.body.as_mut() else {
        unreachable!()
    };
    outer.projection = aliases
        .iter()
        .zip(names)
        .map(|(alias, name)| {
            let expr = Expr::Identifier(Ident::new(alias));
            if name.is_empty() {
                SelectItem::UnnamedExpr(expr)
            } else {
                SelectItem::ExprWithAlias {
                    expr,
                    alias: Ident::with_quote('"', name),
                }
            }
        })
        .collect();
    if let SetExpr::Select(inner) = query.body.as_mut() {
        outer.top = inner.top.take();
        outer.into = inner.into.take();
        inner
            .projection
            .extend(extra.into_iter().map(SelectItem::UnnamedExpr));
    }
    let TableFactor::Derived { subquery, .. } = &mut outer.from[0].relation else {
        unreachable!()
    };
    std::mem::swap(&mut subquery.body, &mut query.body);
    query.body = wrapper.body;
    if let Some(OrderBy {
        kind: OrderByKind::Expressions(orders),
        ..
    }) = &mut query.order_by
    {
        for (order, (index, variant)) in orders.iter_mut().zip(keys) {
            let expr = column(*index);
            order.expr = if *variant {
                crate::variant_compare::key(expr)
            } else {
                expr
            };
        }
    }
}
