//! Preserve variant base types and numeric equality across set operations.
use sqlparser::{ast::*, dialect::GenericDialect, parser::Parser};

#[derive(Clone, Copy)]
pub enum Equality {
    Native,
    Variant,
    DateTimeOffset,
    Unicode,
}
impl Equality {
    pub fn key(self, value: Expr) -> Expr {
        match self {
            Self::Native => value,
            Self::Variant => crate::variant_compare::key(value),
            Self::DateTimeOffset => crate::datetimeoffset_compare::key(value),
            Self::Unicode => crate::expr::unary_function(
                "__msduck_unicode_order_key",
                crate::expr::unary_function("__msduck_carrier_input", value),
            ),
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

fn member_names(body: &SetExpr) -> (String, String) {
    struct Names(std::collections::HashSet<String>);
    impl Visitor for Names {
        type Break = ();
        fn pre_visit_relation(&mut self, name: &ObjectName) -> std::ops::ControlFlow<()> {
            self.0.extend(
                name.0
                    .iter()
                    .filter_map(|part| part.as_ident())
                    .map(|id| id.value.to_lowercase()),
            );
            std::ops::ControlFlow::Continue(())
        }
    }
    let mut used = Names(std::collections::HashSet::new());
    let _ = body.visit(&mut used);
    let mut fresh = |base: &str| {
        let mut candidate = base.to_string();
        let mut suffix = 0;
        while !used.0.insert(candidate.clone()) {
            suffix += 1;
            candidate = format!("{base}_{suffix}");
        }
        candidate
    };
    (
        fresh("__variant_left_input"),
        fresh("__variant_right_input"),
    )
}

pub fn membership(body: &mut SetExpr, names: &[String], variants: &[Equality]) {
    let (left_input, right_input) = member_names(body);
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
        "WITH {left_input}({columns}) AS (SELECT NULL), {right_input}({columns}) AS (SELECT NULL) SELECT * FROM {left_input} AS l({columns}) WHERE {}EXISTS(SELECT 1 FROM {right_input} AS r({columns}) WHERE true)",
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
    if variants
        .iter()
        .any(|value| matches!(value, Equality::Unicode))
    {
        // Preserve one original payload for equivalent keys, preferring the
        // shorter binary prefix when trailing spaces are the only difference.
        // Sort references to materialized branch results, never source operands.
        let Statement::Query(mut query) =
            Parser::parse_sql(&GenericDialect {}, "SELECT NULL ORDER BY NULL")
                .expect("generated character distinct ordering")
                .remove(0)
        else {
            unreachable!()
        };
        let template = match &query.order_by.as_ref().unwrap().kind {
            OrderByKind::Expressions(exprs) => exprs[0].clone(),
            _ => unreachable!(),
        };
        let expressions = variants
            .iter()
            .enumerate()
            .flat_map(|(i, variant)| {
                let value = Expr::CompoundIdentifier(vec![
                    Ident::new("__variant_set_source"),
                    Ident::new(format!("__variant_set_{i}")),
                ]);
                let mut key = template.clone();
                key.expr = variant.key(value.clone());
                let mut ordering = vec![key];
                if matches!(variant, Equality::Unicode) {
                    let mut raw = template.clone();
                    raw.expr = crate::expr::binary_function(
                        "struct_extract",
                        crate::expr::unary_function("__msduck_carrier_input", value),
                        Expr::Value(Value::SingleQuotedString("__msduck_utf16le".into()).into()),
                    );
                    ordering.push(raw);
                }
                ordering
            })
            .collect();
        query.order_by.as_mut().unwrap().kind = OrderByKind::Expressions(expressions);
        query.body = inner;
        *body = SetExpr::Query(query);
    } else {
        *body = *inner;
    }
}
