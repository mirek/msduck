//! Preserve variant base types and numeric equality across set operations.
use sqlparser::{ast::*, dialect::GenericDialect, parser::Parser};

#[derive(Clone, Copy)]
pub enum Equality {
    Native,
    Variant,
    DateTimeOffset,
}
impl Equality {
    pub fn key(self, value: Expr) -> Expr {
        match self {
            Self::Native => value,
            Self::Variant => crate::variant_compare::key(value),
            Self::DateTimeOffset => crate::datetimeoffset_compare::key(value),
        }
    }
    pub fn special(self) -> bool {
        !matches!(self, Self::Native)
    }
    pub fn for_type(kind: Option<&DataType>) -> Self {
        if kind.is_some_and(crate::variant_pack::is_variant) {
            Self::Variant
        } else if kind.is_some_and(|k| {
            crate::datetimeoffset_cast::scale(k)
                .ok()
                .flatten()
                .is_some()
        }) {
            Self::DateTimeOffset
        } else {
            Self::Native
        }
    }
}

pub fn supported(op: SetOperator, quantifier: SetQuantifier) -> bool {
    matches!(quantifier, SetQuantifier::None | SetQuantifier::Distinct)
        || (op == SetOperator::Union && quantifier == SetQuantifier::All)
}

pub fn membership(body: &mut SetExpr, names: &[String], variants: &[Equality]) {
    let SetExpr::SetOperation {
        left, right, op, ..
    } = body
    else {
        unreachable!()
    };
    let columns = (0..names.len())
        .map(|i| format!("__variant_member_{i}"))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "WITH __variant_left_input AS (SELECT NULL), __variant_right_input AS (SELECT NULL) SELECT * FROM __variant_left_input AS l({columns}) WHERE {}EXISTS(SELECT 1 FROM __variant_right_input AS r({columns}) WHERE true)",
        if *op == SetOperator::Except {
            "NOT "
        } else {
            ""
        }
    );
    let Statement::Query(mut query) = Parser::parse_sql(&GenericDialect {}, &sql)
        .expect("generated variant membership wrapper")
        .remove(0)
    else {
        unreachable!()
    };
    let with = query.with.as_mut().unwrap();
    for cte in &mut with.cte_tables {
        cte.materialized = Some(CteAsMaterialized::Materialized);
    }
    with.cte_tables[0].query.body = left.clone();
    with.cte_tables[1].query.body = right.clone();
    let SetExpr::Select(select) = query.body.as_mut() else {
        unreachable!()
    };
    let column = |side: &str, i: usize| {
        Expr::CompoundIdentifier(vec![
            Ident::new(side),
            Ident::new(format!("__variant_member_{i}")),
        ])
    };
    select.projection = names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let expr = column("l", i);
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
    let predicate = variants
        .iter()
        .enumerate()
        .map(|(i, variant)| {
            let value = |side| {
                let expr = column(side, i);
                variant.key(expr)
            };
            Expr::IsNotDistinctFrom(Box::new(value("l")), Box::new(value("r")))
        })
        .reduce(|left, right| Expr::BinaryOp {
            left: Box::new(left),
            op: BinaryOperator::And,
            right: Box::new(right),
        })
        .expect("nonempty variant projection");
    let Some(Expr::Exists { subquery, .. }) = &mut select.selection else {
        unreachable!()
    };
    let SetExpr::Select(right_select) = subquery.body.as_mut() else {
        unreachable!()
    };
    right_select.selection = Some(predicate);
    *body = SetExpr::Query(query);
    deduplicate(body, names, variants);
}

pub fn wrap(body: &mut Box<SetExpr>, names: &[String], variants: &[bool]) {
    let aliases = (0..names.len())
        .map(|i| format!("__variant_set_{i}"))
        .collect::<Vec<_>>();
    let sql = format!(
        "SELECT * FROM (SELECT NULL) AS __variant_set_source({})",
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
        .zip(variants)
        .map(|((alias, name), variant)| {
            let value = Expr::Identifier(Ident::new(alias));
            let expr = if *variant {
                crate::variant_pack::convert(value)
            } else {
                value
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
    let TableFactor::Derived { subquery, .. } = &mut select.from[0].relation else {
        unreachable!()
    };
    std::mem::swap(&mut subquery.body, body);
    *body = query.body;
}

pub fn deduplicate(body: &mut SetExpr, names: &[String], variants: &[Equality]) {
    let mut inner = Box::new(body.clone());
    wrap(&mut inner, names, &vec![false; names.len()]);
    let SetExpr::Select(select) = inner.as_mut() else {
        unreachable!()
    };
    select.distinct = Some(Distinct::On(
        variants
            .iter()
            .enumerate()
            .map(|(i, variant)| {
                let value = Expr::CompoundIdentifier(vec![
                    Ident::new("__variant_set_source"),
                    Ident::new(format!("__variant_set_{i}")),
                ]);
                variant.key(value)
            })
            .collect(),
    ));
    *body = *inner;
}
