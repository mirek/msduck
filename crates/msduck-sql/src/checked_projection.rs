//! Checked integer SELECT projections. Catalog acquisition and outcome decoding
//! belong to the root adapter; the original public descriptor is not rewritten.
use crate::{binding_scope::Scope, checked_expression::projection_operand};
use sqlparser::{ast::*, dialect::DuckDbDialect, parser::Parser};

pub struct Plan {
    pub query: Box<Query>,
    pub width: usize,
    pub kinds: Vec<crate::checked_expression::Kind>,
    pub scalar_checks: Vec<Expr>,
}

/// Each original projection becomes one correlated, materialized scalar plan.
/// Public values precede four diagnostic columns per original expression.
/// Unsupported projection trees and row-shaping clauses are not partly lowered.
/// Errors in FROM/WHERE still require their own checked relational plans.
pub fn plan(query: &Query, scope: &Scope) -> Option<Plan> {
    let SetExpr::Select(select) = query.body.as_ref() else {
        return None;
    };
    if query.limit_clause.is_some()
        || query.fetch.is_some()
        || !query.locks.is_empty()
        || query.for_clause.is_some()
        || query.settings.is_some()
        || query.format_clause.is_some()
        || !query.pipe_operators.is_empty()
        || select.distinct.is_some()
        || select.top.is_some()
        || select.into.is_some()
        || !select.optimizer_hints.is_empty()
        || select.select_modifiers.is_some()
        || select.exclude.is_some()
        || !select.lateral_views.is_empty()
        || select.prewhere.is_some()
        || !select.connect_by.is_empty()
        || !select.cluster_by.is_empty()
        || !select.distribute_by.is_empty()
        || !select.sort_by.is_empty()
        || select.having.is_some()
        || !select.named_window.is_empty()
        || select.qualify.is_some()
        || select.value_table_mode.is_some()
        || select.flavor != SelectFlavor::Standard
        || !matches!(&select.group_by, GroupByExpr::Expressions(items, modifiers) if items.is_empty() && modifiers.is_empty())
        || select.projection.is_empty()
        || select.projection.len() > 32
    {
        return None;
    }
    let operands = select
        .projection
        .iter()
        .map(|item| match item {
            SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => {
                projection_operand(expr, scope)
            }
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    if !operands.iter().any(|operand| operand.checked) {
        return None;
    }
    if let Some(order) = &query.order_by {
        let OrderByKind::Expressions(items) = &order.kind else {
            return None;
        };
        if order.interpolate.is_some() || !items.iter().all(|item| {
            if let Expr::Identifier(id) = &item.expr {
                let matching=select.projection.iter().enumerate().filter(|(_,p)| matches!(p,SelectItem::ExprWithAlias{alias,..} if alias.value.eq_ignore_ascii_case(&id.value))).collect::<Vec<_>>();
                if !matching.is_empty() {return matching.len()==1 && !operands[matching[0].0].checked;}
            }
            if let Expr::Value(value)=&item.expr
                && let Value::Number(n,_)=&value.value {
                return n.parse::<usize>().ok().and_then(|i|i.checked_sub(1)).and_then(|i|operands.get(i)).is_some_and(|p|!p.checked);
            }
            matches!(&item.expr,Expr::Identifier(_) | Expr::CompoundIdentifier(_))
                && projection_operand(&item.expr,scope).is_some_and(|p|!p.checked)
        }) { return None; }
    }
    // Row-independent projections expose arithmetic errors even for an empty
    // source. Select this path from syntax and declarations, never parameter
    // values. The adapter evaluates bound scalars in projection order.
    let constants = select.projection.iter().all(|item| {
        visit_expressions(item, |expr| {
            if matches!(expr, Expr::Identifier(id) if !id.value.starts_with('@'))
                || matches!(expr, Expr::CompoundIdentifier(_))
            {
                std::ops::ControlFlow::Break(())
            } else {
                std::ops::ControlFlow::Continue(())
            }
        })
        .is_continue()
    });
    let scalar_checks = if constants && !select.from.is_empty() {
        select
            .projection
            .iter()
            .map(|item| match item {
                SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => {
                    expr.clone()
                }
                _ => unreachable!(),
            })
            .collect()
    } else {
        Vec::new()
    };
    // The text is inspected only to choose fresh internal names, never parsed
    // back or used to rewrite identifiers, literals or bound values.
    let original = query.to_string().to_lowercase();
    let prefix = (0..64)
        .map(|n| format!("__msduck_checked_projection_{n}_"))
        .find(|name| !original.contains(name))?;
    let mut result = query.clone();
    let SetExpr::Select(target) = result.body.as_mut() else {
        unreachable!()
    };
    target.projection.clear();
    let mut diagnostics = Vec::new();
    let kinds = operands.iter().map(|operand| operand.kind).collect();
    for (index, operand) in operands.into_iter().enumerate() {
        let alias = format!("{prefix}{index}");
        let field = |name: &str| {
            Expr::CompoundIdentifier(vec![
                Ident::new(&alias),
                Ident::new(format!("{alias}_{name}")),
            ])
        };
        target.projection.push(match &select.projection[index] {
            SelectItem::ExprWithAlias { alias, .. } => SelectItem::ExprWithAlias {
                expr: field("value"),
                alias: alias.clone(),
            },
            _ => SelectItem::UnnamedExpr(field("value")),
        });
        for name in [
            "error_number",
            "error_state",
            "error_severity",
            "error_message",
        ] {
            diagnostics.push(SelectItem::UnnamedExpr(field(name)));
        }
        // Fixed template builds only the FROM skeleton. Insert the already
        // bound operand query as AST, retaining quoted source identifiers.
        let names = [
            "value",
            "error_number",
            "error_state",
            "error_severity",
            "error_message",
        ]
        .map(|name| format!("{alias}_{name}"))
        .join(",");
        let mut template = Parser::parse_sql(
            &DuckDbDialect {},
            &format!("SELECT 0 FROM LATERAL (SELECT 0) {alias}({names})"),
        )
        .expect("checked projection skeleton");
        let Statement::Query(template) = template.remove(0) else {
            unreachable!()
        };
        let SetExpr::Select(mut template) = *template.body else {
            unreachable!()
        };
        let mut source = template.from.remove(0);
        let TableFactor::Derived { subquery, .. } = &mut source.relation else {
            unreachable!()
        };
        *subquery = operand.query;
        target.from.push(source);
    }
    target.projection.extend(diagnostics);
    Some(Plan {
        query: Box::new(result),
        width: select.projection.len(),
        kinds,
        scalar_checks,
    })
}
