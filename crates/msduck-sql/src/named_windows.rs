//! Resolve SQL Server named windows within each query specification.
use sqlparser::ast::*;
use std::{
    collections::{HashMap, HashSet},
    ops::ControlFlow,
};

type Definitions = HashMap<String, WindowSpec>;
fn empty() -> WindowSpec {
    WindowSpec {
        window_name: None,
        partition_by: vec![],
        order_by: vec![],
        window_frame: None,
    }
}
fn merge(mut base: WindowSpec, mut added: WindowSpec) -> Result<WindowSpec, String> {
    if (!base.partition_by.is_empty() && !added.partition_by.is_empty())
        || (!base.order_by.is_empty() && !added.order_by.is_empty())
        || (base.window_frame.is_some() && added.window_frame.is_some())
    {
        return Err(
            "Window element in OVER clause can not also be specified in WINDOW clause.".into(),
        );
    }
    if !added.partition_by.is_empty() {
        base.partition_by = std::mem::take(&mut added.partition_by);
    }
    if !added.order_by.is_empty() {
        base.order_by = std::mem::take(&mut added.order_by);
    }
    if added.window_frame.is_some() {
        base.window_frame = added.window_frame;
    }
    base.window_name = None;
    Ok(base)
}
fn resolve(
    name: &str,
    definitions: &Definitions,
    cache: &mut Definitions,
) -> Result<WindowSpec, String> {
    let mut current = name.to_string();
    let mut path = vec![];
    let mut seen = HashSet::new();
    let mut result = loop {
        if let Some(spec) = cache.get(&current) {
            break spec.clone();
        }
        if !seen.insert(current.clone()) {
            return Err(format!("Cyclic named window reference: {current}"));
        }
        let spec = definitions
            .get(&current)
            .ok_or_else(|| format!("Undefined named window: {current}"))?
            .clone();
        let parent = spec.window_name.as_ref().map(|id| id.value.to_lowercase());
        path.push((current, spec));
        if let Some(parent) = parent {
            current = parent;
        } else {
            break empty();
        }
    };
    for (name, spec) in path.into_iter().rev() {
        result = merge(result, spec)?;
        cache.insert(name, result.clone());
    }
    Ok(result)
}
struct Expand<'a> {
    definitions: &'a Definitions,
    queries: usize,
}
impl VisitorMut for Expand<'_> {
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
        if self.queries != 0 {
            return ControlFlow::Continue(());
        }
        if let Expr::Function(function) = expr
            && let Some(over) = &mut function.over
        {
            let (name, extra) = match over {
                WindowType::NamedWindow(name) => (Some(name.clone()), empty()),
                WindowType::WindowSpec(spec) => (spec.window_name.clone(), spec.clone()),
            };
            if let Some(name) = name {
                let Some(base) = self.definitions.get(&name.value.to_lowercase()) else {
                    return ControlFlow::Break(format!("Undefined named window: {}", name.value));
                };
                match merge(base.clone(), extra) {
                    Ok(spec) => *over = WindowType::WindowSpec(spec),
                    Err(error) => return ControlFlow::Break(error),
                }
            }
        }
        ControlFlow::Continue(())
    }
}
// Window functions in a definition would recursively expand and are illegal
// at this query level. Subqueries retain their own independent window scope.
fn check_definition(spec: &WindowSpec) -> Result<(), String> {
    struct Check {
        queries: usize,
    }
    impl Visitor for Check {
        type Break = ();
        fn pre_visit_query(&mut self, _: &Query) -> ControlFlow<()> {
            self.queries += 1;
            ControlFlow::Continue(())
        }
        fn post_visit_query(&mut self, _: &Query) -> ControlFlow<()> {
            self.queries -= 1;
            ControlFlow::Continue(())
        }
        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
            if self.queries == 0 && matches!(expr, Expr::Function(f) if f.over.is_some()) {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        }
    }
    if spec.visit(&mut Check { queries: 0 }).is_break() {
        Err("Windowed functions cannot be used in the context of another windowed function or aggregate.".into())
    } else {
        Ok(())
    }
}
fn expand(select: &mut Select, order: Option<&mut OrderBy>) -> Result<(), String> {
    let mut definitions = Definitions::new();
    for NamedWindowDefinition(name, definition) in &select.named_window {
        let spec = match definition {
            NamedWindowExpr::WindowSpec(spec) => spec.clone(),
            NamedWindowExpr::NamedWindow(_) => {
                return Err("unsupported unparenthesized named window definition".into());
            }
        };
        check_definition(&spec)?;
        if definitions
            .insert(name.value.to_lowercase(), spec)
            .is_some()
        {
            return Err(format!("Duplicate named window: {}", name.value));
        }
    }
    let mut resolved = Definitions::new();
    // Validate in declaration order, not randomized map iteration order.
    // Lookup still supports forward references through the complete definition map.
    for NamedWindowDefinition(name, _) in &select.named_window {
        resolve(&name.value.to_lowercase(), &definitions, &mut resolved)?;
    }
    select.named_window.clear();
    let mut visitor = Expand {
        definitions: &resolved,
        queries: 0,
    };
    if let ControlFlow::Break(error) = select.visit(&mut visitor) {
        return Err(error);
    }
    if let Some(order) = order
        && let ControlFlow::Break(error) = order.visit(&mut visitor)
    {
        return Err(error);
    }
    Ok(())
}
pub fn query(query: &mut Query) -> Result<(), String> {
    if let SetExpr::Select(select) = query.body.as_mut() {
        expand(select, query.order_by.as_mut())?;
    }
    Ok(())
}
pub fn select(select: &mut Select) -> Result<(), String> {
    expand(select, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlparser::parser::Parser;

    #[test]
    fn definition_errors_follow_source_order_and_leave_the_ast_intact() {
        for (sql, expected) in [
            (
                "SELECT SUM(1) OVER z WINDOW z AS (missing_z), a AS (missing_a)",
                "Undefined named window: missing_z",
            ),
            (
                "SELECT SUM(1) OVER z WINDOW a AS (missing_a), z AS (missing_z)",
                "Undefined named window: missing_a",
            ),
            (
                "SELECT SUM(1) OVER z WINDOW z AS (a), a AS (z)",
                "Cyclic named window reference: z",
            ),
        ] {
            let statement = Parser::parse_sql(&crate::dialect::ServerDialect, sql)
                .unwrap()
                .remove(0);
            // Each resolution allocates new maps. The result must not depend
            // on their randomized hash seeds, including for unreferenced errors.
            for _ in 0..64 {
                let mut copy = statement.clone();
                let Statement::Query(query) = &mut copy else {
                    panic!("expected SELECT")
                };
                assert_eq!(super::query(query).unwrap_err(), expected);
                assert_eq!(copy, statement);
            }
        }
        let mut valid = Parser::parse_sql(
            &crate::dialect::ServerDialect,
            "SELECT SUM(1) OVER z WINDOW z AS (a), a AS (ORDER BY 1)",
        )
        .unwrap()
        .remove(0);
        let Statement::Query(query) = &mut valid else {
            panic!("expected SELECT")
        };
        super::query(query).unwrap();
        assert!(!valid.to_string().contains(" WINDOW "));
        assert!(valid.to_string().contains("OVER (ORDER BY 1)"));
    }
}
