//! Apply positional DATETIME2 result types before set comparison/deduplication.
use sqlparser::{ast::*, dialect::GenericDialect, parser::Parser};

pub fn wrap(body: &mut Box<SetExpr>, names: &[String], scales: &[Option<u8>]) {
    wrap_temporal(body, names, scales, false);
}

pub fn wrap_temporal(
    body: &mut Box<SetExpr>,
    names: &[String],
    scales: &[Option<u8>],
    offset: bool,
) {
    let aliases = (0..names.len())
        .map(|i| format!("__dt2_set_{i}"))
        .collect::<Vec<_>>();
    let sql = format!(
        "SELECT * FROM (SELECT NULL) AS __dt2_set_source({})",
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
        .zip(scales)
        .map(|((alias, name), scale)| {
            let value = Expr::Identifier(Ident::new(alias));
            let expr = scale.map_or_else(
                || value.clone(),
                |scale| {
                    if offset {
                        crate::datetimeoffset_cast::convert(value.clone(), scale)
                    } else {
                        crate::datetime2_cast::convert(value.clone(), scale)
                    }
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
