//! Pad positional fixed character results before set comparison/deduplication.
use sqlparser::{ast::*, dialect::GenericDialect, parser::Parser};

pub fn wrap(body: &mut Box<SetExpr>, names: &[String], widths: &[Option<(u16, &str)>]) {
    let aliases = (0..names.len())
        .map(|i| format!("__nchar_set_{i}"))
        .collect::<Vec<_>>();
    let sql = format!(
        "SELECT * FROM (SELECT NULL) AS __nchar_set_source({})",
        aliases.join(",")
    );
    let Statement::Query(mut query) = Parser::parse_sql(&GenericDialect {}, &sql)
        .expect("generated set wrapper")
        .remove(0)
    else {
        unreachable!()
    };
    let SetExpr::Select(select) = query.body.as_mut() else {
        unreachable!()
    };
    select.projection = aliases
        .into_iter()
        .zip(names)
        .zip(widths)
        .map(|((alias, name), width)| {
            let value = Expr::Identifier(Ident::new(alias));
            let expr = width.map_or_else(
                || value.clone(),
                |(width, function)| {
                    crate::expr::binary_function(
                        function,
                        value.clone(),
                        Expr::Value(Value::Number(width.to_string(), false).into()),
                    )
                },
            );
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
    let TableFactor::Derived { subquery, .. } = &mut select.from[0].relation else {
        unreachable!()
    };
    std::mem::swap(&mut subquery.body, body);
    *body = query.body;
}
