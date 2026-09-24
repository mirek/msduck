use super::{Annotate, Scope};
use sqlparser::ast::*;
use std::ops::ControlFlow;

fn group_key(value: Expr) -> Expr {
    if crate::variant_compare::known(&value) {
        crate::variant_compare::key(value)
    } else {
        crate::datetimeoffset_compare::key(value)
    }
}
fn group_null(value: &Expr) -> Expr {
    let null = Expr::Value(Value::Null.into());
    if let Some(scale) = crate::datetimeoffset_compare::scale(value, &Default::default()) {
        crate::datetimeoffset_cast::convert(null, scale)
    } else {
        crate::variant_pack::convert(null)
    }
}

// Keep a representative payload while grouping by numeric or UTC equality.
pub(super) fn lower(select: &mut Select, order: Option<&mut OrderBy>, scope: &Scope) {
    fn keys(expr: &mut Expr, scope: &Scope, found: &mut Vec<(Expr, Expr)>) {
        if let Expr::GroupingSets(sets) | Expr::Cube(sets) | Expr::Rollup(sets) = expr {
            for value in sets.iter_mut().flatten() {
                keys(value, scope, found);
            }
            return;
        }
        let mut typed = expr.clone();
        let _ = VisitMut::visit(
            &mut typed,
            &mut Annotate {
                scope,
                queries: 0,
                datetime_only: true,
                syntax: Default::default(),
            },
        );
        if crate::variant_compare::known(&typed)
            || crate::datetimeoffset_compare::scale(&typed, &Default::default()).is_some()
        {
            found.push((expr.clone(), typed.clone()));
            *expr = group_key(typed);
        }
    }
    let GroupByExpr::Expressions(groups, _) = &mut select.group_by else {
        return;
    };
    let mut found = vec![];
    for group in groups {
        keys(group, scope, &mut found);
    }
    if found.is_empty() {
        return;
    }
    struct Replace<'a> {
        found: &'a [(Expr, Expr)],
        scope: &'a Scope,
        queries: usize,
        skip: usize,
    }
    impl Replace<'_> {
        fn matched(&self, value: &Expr) -> Option<Expr> {
            self.found
                .iter()
                .find(|(original, _)| {
                    original == value
                        || self
                            .scope
                            .source_column(original)
                            .is_some_and(|id| Some(id) == self.scope.source_column(value))
                })
                .map(|(_, typed)| typed.clone())
        }
    }
    impl VisitorMut for Replace<'_> {
        type Break = ();
        fn pre_visit_query(&mut self, _: &mut Query) -> ControlFlow<()> {
            self.queries += 1;
            ControlFlow::Continue(())
        }
        fn post_visit_query(&mut self, _: &mut Query) -> ControlFlow<()> {
            self.queries -= 1;
            ControlFlow::Continue(())
        }
        fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
            if self.skip > 0 {
                self.skip += 1;
                return ControlFlow::Continue(());
            }
            if self.queries > 0 {
                return ControlFlow::Continue(());
            }
            if let Expr::Function(f) = expr {
                let name = f.name.to_string().to_ascii_lowercase();
                if matches!(name.as_str(), "grouping" | "grouping_id") {
                    if let FunctionArguments::List(args) = &mut f.args {
                        for arg in &mut args.args {
                            if let FunctionArg::Unnamed(FunctionArgExpr::Expr(value)) = arg
                                && let Some(typed) = self.matched(value)
                            {
                                *value = group_key(typed);
                            }
                        }
                    }
                    self.skip = 1;
                    return ControlFlow::Continue(());
                }
                if f.over.is_none()
                    && matches!(
                        name.as_str(),
                        "sum"
                            | "avg"
                            | "min"
                            | "max"
                            | "count"
                            | "count_big"
                            | "stdev"
                            | "stdevp"
                            | "var"
                            | "varp"
                            | "string_agg"
                            | "checksum_agg"
                            | "approx_count_distinct"
                            | "any_value"
                    )
                {
                    self.skip = 1;
                    return ControlFlow::Continue(());
                }
            }
            if let Some(typed) = self.matched(expr) {
                // GROUPING distinguishes a subtotal from an actual NULL-valued group.
                *expr = Expr::Case {
                    case_token: helpers::attached_token::AttachedToken::empty(),
                    end_token: helpers::attached_token::AttachedToken::empty(),
                    operand: None,
                    conditions: vec![CaseWhen {
                        condition: Expr::BinaryOp {
                            left: Box::new(crate::expr::unary_function(
                                "grouping",
                                group_key(typed.clone()),
                            )),
                            op: BinaryOperator::Eq,
                            right: Box::new(Expr::Value(Value::Number("1".into(), false).into())),
                        },
                        result: group_null(&typed),
                    }],
                    else_result: Some(Box::new(crate::expr::unary_function("min", typed))),
                };
                self.skip = 1;
            }
            ControlFlow::Continue(())
        }
        fn post_visit_expr(&mut self, _: &mut Expr) -> ControlFlow<()> {
            if self.skip > 0 {
                self.skip -= 1;
            }
            ControlFlow::Continue(())
        }
    }
    let mut projection = vec![];
    for item in std::mem::take(&mut select.projection) {
        if matches!(
            item,
            SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(_, _)
        ) {
            let mut expansion = select.clone();
            expansion.projection = vec![item.clone()];
            if let Some(values) = scope.projection_references(&expansion) {
                projection.extend(values.into_iter().map(SelectItem::UnnamedExpr));
                continue;
            }
        }
        projection.push(item);
    }
    select.projection = projection;
    let aliases = select
        .projection
        .iter()
        .filter_map(|item| match item {
            SelectItem::ExprWithAlias { alias, .. } => Some(alias.value.to_lowercase()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut visitor = Replace {
        found: &found,
        scope,
        queries: 0,
        skip: 0,
    };
    for item in &mut select.projection {
        let name = match item {
            SelectItem::UnnamedExpr(Expr::Identifier(id)) => Some(id.clone()),
            SelectItem::UnnamedExpr(Expr::CompoundIdentifier(ids)) => ids.last().cloned(),
            _ => None,
        };
        let before = item.clone();
        let _ = item.visit(&mut visitor);
        if *item != before
            && let Some(alias) = name
            && let SelectItem::UnnamedExpr(expr) = item
        {
            *item = SelectItem::ExprWithAlias {
                expr: expr.clone(),
                alias,
            };
        }
    }
    let _ = VisitMut::visit(&mut select.having, &mut visitor);
    if let Some(OrderBy {
        kind: OrderByKind::Expressions(orders),
        ..
    }) = order
    {
        for item in orders {
            if matches!(&item.expr, Expr::Identifier(id) if aliases.contains(&id.value.to_lowercase()))
            {
                continue;
            }
            let _ = VisitMut::visit(&mut item.expr, &mut visitor);
        }
    }
}
