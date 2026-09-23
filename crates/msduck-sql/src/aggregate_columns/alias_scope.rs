//! SELECT aliases are unavailable to WHERE, GROUP BY and HAVING.
use super::Scope;
use sqlparser::ast::*;
use std::{collections::HashSet, ops::ControlFlow};

pub(super) fn validate(select: &Select, scope: &Scope) -> Result<(), String> {
    if scope.unknown_source {
        return Ok(());
    }
    let aliases = select
        .projection
        .iter()
        .filter_map(|item| match item {
            SelectItem::ExprWithAlias { alias, .. } => Some(alias.value.to_lowercase()),
            _ => None,
        })
        .collect::<HashSet<_>>();
    if aliases.is_empty() {
        return Ok(());
    }
    struct Check<'a> {
        scope: &'a Scope,
        aliases: &'a HashSet<String>,
        queries: usize,
    }
    impl VisitorMut for Check<'_> {
        type Break = String;
        fn pre_visit_query(&mut self, _: &mut Query) -> ControlFlow<String> {
            self.queries += 1;
            ControlFlow::Continue(())
        }
        fn post_visit_query(&mut self, _: &mut Query) -> ControlFlow<String> {
            self.queries -= 1;
            ControlFlow::Continue(())
        }
        fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<String> {
            if self.queries > 0 {
                return ControlFlow::Continue(());
            }
            // Datepart keywords are syntax, rather than column references.
            if let Expr::Function(f) = expr
                && matches!(
                    f.name.to_string().to_ascii_lowercase().as_str(),
                    "datepart" | "datename" | "dateadd" | "datediff" | "datediff_big"
                )
                && let FunctionArguments::List(args) = &mut f.args
                && let Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(value))) =
                    args.args.first_mut()
                && matches!(value, Expr::Identifier(_))
            {
                *value = Expr::Value(Value::Null.into());
            }
            if let Expr::Identifier(id) = expr {
                let name = id.value.to_lowercase();
                if !name.starts_with('@')
                    && self.aliases.contains(&name)
                    && !self.scope.columns.contains_key(&vec![name])
                {
                    return ControlFlow::Break(format!(
                        "Invalid column name '{}'.",
                        id.value.replace('\'', "''")
                    ));
                }
            }
            ControlFlow::Continue(())
        }
    }
    let mut check = Check {
        scope,
        aliases: &aliases,
        queries: 0,
    };
    // Check clones: replacing datepart syntax must not alter the executable AST.
    for mut clause in [select.selection.clone(), select.having.clone()]
        .into_iter()
        .flatten()
    {
        if let ControlFlow::Break(error) = VisitMut::visit(&mut clause, &mut check) {
            return Err(error);
        }
    }
    if let ControlFlow::Break(error) = VisitMut::visit(&mut select.group_by.clone(), &mut check) {
        return Err(error);
    }
    Ok(())
}
