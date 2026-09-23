use super::Scope;
use sqlparser::{ast::*, dialect::GenericDialect, parser::Parser};
use std::ops::ControlFlow;

pub(super) fn lower(
    select: &mut Select,
    order: Option<&mut OrderBy>,
    scope: &Scope,
) -> Result<bool, String> {
    if !crate::group_all::marked(select) {
        return Ok(false);
    }
    let GroupByExpr::Expressions(groups, modifiers) = &mut select.group_by else {
        unreachable!()
    };
    groups.remove(0);
    if !modifiers.is_empty()
        || groups.is_empty()
        || groups
            .iter()
            .any(|g| matches!(g, Expr::GroupingSets(_) | Expr::Cube(_) | Expr::Rollup(_)))
    {
        return Err("GROUP BY ALL requires ordinary grouping expressions without modifiers".into());
    }
    let Some(predicate) = select.selection.take() else {
        return Ok(true);
    };
    let mut source_select = select.clone();
    source_select.projection = vec![SelectItem::Wildcard(Default::default())];
    let columns = scope
        .projection_references(&source_select)
        .ok_or("cannot resolve GROUP BY ALL source columns")?;
    let aliases = (0..=columns.len())
        .map(|i| format!("__group_all_{i}"))
        .collect::<Vec<_>>();
    let source_sql = format!("{select} {predicate}").to_lowercase();
    let mut input_name = "__group_all_input".to_string();
    while source_sql.contains(&input_name) {
        input_name.push('_');
    }
    let sql = format!(
        "SELECT * FROM (WITH {input_name} AS (SELECT NULL) SELECT * FROM {input_name}) AS __group_all_source({})",
        aliases.join(",")
    );
    let Statement::Query(mut wrapper) = Parser::parse_sql(&GenericDialect {}, &sql)
        .expect("static group all wrapper")
        .remove(0)
    else {
        unreachable!()
    };
    let SetExpr::Select(outer) = wrapper.body.as_mut() else {
        unreachable!()
    };
    let TableFactor::Derived { subquery, .. } = &mut outer.from[0].relation else {
        unreachable!()
    };
    let cte = &mut subquery.with.as_mut().unwrap().cte_tables[0];
    cte.materialized = Some(CteAsMaterialized::Materialized);
    let SetExpr::Select(input) = cte.query.body.as_mut() else {
        unreachable!()
    };
    input.from = std::mem::take(&mut select.from);
    input.projection = columns
        .into_iter()
        .zip(&aliases)
        .map(|(expr, name)| SelectItem::ExprWithAlias {
            expr,
            alias: Ident::new(name),
        })
        .collect();
    input.projection.push(SelectItem::ExprWithAlias {
        expr: condition(
            predicate,
            Expr::Value(Value::Number("1".into(), false).into()),
            Expr::Value(Value::Number("0".into(), false).into()),
        ),
        alias: Ident::new(aliases.last().unwrap()),
    });
    // Preserve wildcard output names before replacing source references.
    let mut projection = vec![];
    for item in std::mem::take(&mut select.projection) {
        if matches!(
            item,
            SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(_, _)
        ) {
            let mut expansion = source_select.clone();
            expansion.projection = vec![item.clone()];
            let values = scope
                .projection_references(&expansion)
                .ok_or("cannot expand GROUP BY ALL projection")?;
            projection.extend(values.into_iter().map(SelectItem::UnnamedExpr));
        } else {
            projection.push(item);
        }
    }
    select.projection = projection;
    struct Rewrite<'a> {
        scope: &'a Scope,
        aliases: &'a [String],
        queries: usize,
    }
    impl VisitorMut for Rewrite<'_> {
        type Break = String;
        fn pre_visit_query(&mut self, _: &mut Query) -> ControlFlow<String> {
            self.queries += 1;
            ControlFlow::Continue(())
        }
        fn post_visit_query(&mut self, _: &mut Query) -> ControlFlow<String> {
            self.queries -= 1;
            ControlFlow::Continue(())
        }
        fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<String> {
            if self.queries > 0 {
                return ControlFlow::Continue(());
            }
            if let Some((source, index)) = self.scope.source_column(expr) {
                let offset: usize = self.scope.sources[..source]
                    .iter()
                    .map(|(_, c)| c.len())
                    .sum();
                *expr = column(&self.aliases[offset + index]);
            }
            if let Expr::Function(f) = expr
                && f.over.is_none()
                && matches!(
                    f.name.to_string().to_ascii_lowercase().as_str(),
                    "count"
                        | "count_big"
                        | "sum"
                        | "avg"
                        | "min"
                        | "max"
                        | "stdev"
                        | "stdevp"
                        | "var"
                        | "varp"
                        | "string_agg"
                        | "approx_count_distinct"
                )
            {
                if let Err(error) = crate::aggregate::validate(f) {
                    return ControlFlow::Break(error);
                }
                if let FunctionArguments::List(args) = &mut f.args
                    && let Some(FunctionArg::Unnamed(value)) = args.args.first_mut()
                {
                    let original = match value {
                        FunctionArgExpr::Expr(value) => value.clone(),
                        FunctionArgExpr::Wildcard => {
                            Expr::Value(Value::Number("1".into(), false).into())
                        }
                        _ => {
                            return ControlFlow::Break(
                                "unsupported GROUP BY ALL aggregate argument".into(),
                            );
                        }
                    };
                    let keep = Expr::BinaryOp {
                        left: Box::new(column(self.aliases.last().unwrap())),
                        op: BinaryOperator::Eq,
                        right: Box::new(Expr::Value(Value::Number("1".into(), false).into())),
                    };
                    *value = FunctionArgExpr::Expr(condition(
                        keep,
                        original,
                        Expr::Value(Value::Null.into()),
                    ));
                }
            }
            ControlFlow::Continue(())
        }
    }
    let output_aliases = select
        .projection
        .iter()
        .filter_map(|item| match item {
            SelectItem::ExprWithAlias { alias, .. } => Some(alias.value.to_lowercase()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut rewrite = Rewrite {
        scope,
        aliases: &aliases,
        queries: 0,
    };
    for item in &mut select.projection {
        let name = match item {
            SelectItem::UnnamedExpr(Expr::Identifier(id)) => Some(id.clone()),
            SelectItem::UnnamedExpr(Expr::CompoundIdentifier(ids)) => ids.last().cloned(),
            _ => None,
        };
        if let ControlFlow::Break(error) = item.visit(&mut rewrite) {
            return Err(error);
        }
        if let Some(alias) = name
            && let SelectItem::UnnamedExpr(expr) = item
        {
            *item = SelectItem::ExprWithAlias {
                expr: expr.clone(),
                alias,
            };
        }
    }
    if let ControlFlow::Break(error) = VisitMut::visit(&mut select.group_by, &mut rewrite) {
        return Err(error);
    }
    if let ControlFlow::Break(error) = VisitMut::visit(&mut select.having, &mut rewrite) {
        return Err(error);
    }
    if let Some(OrderBy {
        kind: OrderByKind::Expressions(items),
        ..
    }) = order
    {
        for item in items {
            if matches!(&item.expr,Expr::Identifier(id) if output_aliases.contains(&id.value.to_lowercase()))
            {
                continue;
            }
            if let ControlFlow::Break(error) = VisitMut::visit(&mut item.expr, &mut rewrite) {
                return Err(error);
            }
        }
    }
    select.from = std::mem::take(&mut outer.from);
    Ok(true)
}
fn column(name: &str) -> Expr {
    Expr::CompoundIdentifier(vec![Ident::new("__group_all_source"), Ident::new(name)])
}
fn condition(predicate: Expr, yes: Expr, no: Expr) -> Expr {
    Expr::Case {
        case_token: helpers::attached_token::AttachedToken::empty(),
        end_token: helpers::attached_token::AttachedToken::empty(),
        operand: None,
        conditions: vec![CaseWhen {
            condition: predicate,
            result: yes,
        }],
        else_result: Some(Box::new(no)),
    }
}
