//! Positional set-member casts without re-evaluating projected expressions.
use sqlparser::{ast::*, dialect::GenericDialect, parser::Parser};
use std::{collections::HashSet, ops::ControlFlow};

pub fn wrap(body: &mut Box<SetExpr>, names: &[String], kinds: &[Option<DataType>]) {
    assert_eq!(names.len(), kinds.len());
    if names.is_empty() || kinds.iter().all(Option::is_none) {
        return;
    }
    struct Names(HashSet<String>);
    impl Visitor for Names {
        type Break = ();
        fn pre_visit_ident(&mut self, id: &Ident) -> ControlFlow<()> {
            self.0.insert(id.value.to_lowercase());
            ControlFlow::Continue(())
        }
    }
    let mut used = Names(names.iter().map(|n| n.to_lowercase()).collect());
    let _ = Visit::visit(body, &mut used);
    let mut prefix = "__msduck_numeric_set".to_string();
    while used.0.contains(&format!("{prefix}_source"))
        || (0..names.len()).any(|i| used.0.contains(&format!("{prefix}_{i}")))
    {
        prefix.push('_');
    }
    let source = Ident::new(format!("{prefix}_source"));
    let columns = (0..names.len())
        .map(|i| Ident::new(format!("{prefix}_{i}")))
        .collect::<Vec<_>>();
    let Statement::Query(mut query) = Parser::parse_sql(
        &GenericDialect {},
        "SELECT * FROM (SELECT NULL) AS source(col)",
    )
    .unwrap()
    .remove(0) else {
        unreachable!()
    };
    let SetExpr::Select(select) = query.body.as_mut() else {
        unreachable!()
    };
    select.projection = columns
        .iter()
        .zip(names)
        .zip(kinds)
        .map(|((column, name), kind)| {
            let value = Expr::CompoundIdentifier(vec![source.clone(), column.clone()]);
            let expr = match kind {
                Some(kind) => Expr::Cast {
                    kind: CastKind::Cast,
                    expr: Box::new(value),
                    data_type: kind.clone(),
                    format: None,
                },
                None => value,
            };
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
    let TableFactor::Derived {
        subquery,
        alias: Some(alias),
        ..
    } = &mut select.from[0].relation
    else {
        unreachable!()
    };
    alias.name = source;
    alias.columns = columns
        .into_iter()
        .map(|name| TableAliasColumnDef {
            name,
            data_type: None,
        })
        .collect();
    std::mem::swap(&mut subquery.body, body);
    *body = query.body;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wrapper_casts_positions_once_and_avoids_capturing_identifiers() {
        let Statement::Query(mut query) = Parser::parse_sql(
            &GenericDialect {},
            "SELECT nextval('calls') AS value, __msduck_numeric_set_source.n FROM t",
        )
        .unwrap()
        .remove(0) else {
            panic!()
        };
        let original = query.body.clone();
        wrap(
            &mut query.body,
            &["value".into(), "n".into()],
            &[Some(DataType::BigInt(None)), None],
        );
        assert_eq!(query.to_string().matches("nextval").count(), 1);
        let SetExpr::Select(select) = query.body.as_ref() else {
            panic!()
        };
        let TableFactor::Derived {
            subquery,
            alias: Some(alias),
            ..
        } = &select.from[0].relation
        else {
            panic!()
        };
        assert_eq!(subquery.body, original);
        assert_ne!(alias.name.value, "__msduck_numeric_set_source");
        assert_eq!(select.projection.len(), 2);
    }
}
