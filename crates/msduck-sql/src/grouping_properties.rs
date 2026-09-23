//! Result-property context for grouping keys, without evaluating grouping rows.
use crate::binding_scope::{self, Field, Source};
use msduck_core::result::{Origin, Properties};
use sqlparser::ast::*;
use std::{collections::HashSet, ops::ControlFlow};

pub struct Plan {
    keys: HashSet<Expr>,
    advanced: bool,
}
impl Plan {
    pub fn new(select: &Select, sources: &[Source], outer: &[Option<Vec<Source>>]) -> Option<Self> {
        let mut select = select.clone();
        let key = |e: &Expr| canonical(e, sources, outer);
        crate::grouping::legacy(&mut select, key).ok()?;
        crate::grouping::sets(&mut select).ok()?;
        crate::grouping::columns(&select, key).ok()?;
        let mut plan = Self {
            keys: HashSet::new(),
            advanced: false,
        };
        fn collect(plan: &mut Plan, expr: &Expr, key: &impl Fn(&Expr) -> Expr) {
            match expr {
                Expr::Rollup(groups) | Expr::Cube(groups) | Expr::GroupingSets(groups) => {
                    plan.advanced = true;
                    for expr in groups.iter().flatten() {
                        collect(plan, expr, key);
                    }
                }
                Expr::Tuple(values) if values.is_empty() => {}
                _ => {
                    plan.keys.insert(key(expr));
                }
            }
        }
        if let GroupByExpr::Expressions(values, _) = &select.group_by {
            for value in values {
                collect(&mut plan, value, &key);
            }
        }
        Some(plan)
    }
    pub fn field_properties(&self, field: &Field, sources: &[Source]) -> Properties {
        let mut properties = field.properties;
        if self.advanced {
            for (source, row) in sources.iter().enumerate() {
                if let Some(column) = row.fields.iter().position(|f| std::ptr::eq(f, field))
                    && self.keys.contains(&column_key(source, column))
                {
                    properties.null_extend();
                }
            }
        }
        properties
    }
    pub fn properties(
        &self,
        expr: &Expr,
        sources: &[Source],
        outer: &[Option<Vec<Source>>],
    ) -> Option<Properties> {
        if self.keys.is_empty() || !self.keys.contains(&canonical(expr, sources, outer)) {
            return None;
        }
        let mut properties = crate::result_properties::expression(expr, sources, outer);
        let mut value = expr;
        while let Expr::Nested(inner) = value {
            value = inner;
        }
        if !matches!(value, Expr::Identifier(_) | Expr::CompoundIdentifier(_)) {
            properties.origin = Origin::Derived;
        }
        // SQL Server marks grouping keys nullable for advanced grouping even
        // when a particular key occurs in every generated grouping set.
        if self.advanced {
            properties.null_extend();
        }
        Some(properties)
    }
}
fn canonical(expr: &Expr, sources: &[Source], outer: &[Option<Vec<Source>>]) -> Expr {
    struct Normalize<'a> {
        sources: &'a [Source],
        outer: &'a [Option<Vec<Source>>],
    }
    impl VisitorMut for Normalize<'_> {
        type Break = ();
        fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
            let ids = match expr {
                Expr::Identifier(id) => vec![&*id],
                Expr::CompoundIdentifier(ids) => ids.iter().collect(),
                _ => Vec::new(),
            };
            if let Some(field) = binding_scope::resolve(&ids, self.sources, self.outer) {
                for (source, row) in self.sources.iter().enumerate() {
                    if let Some(column) = row.fields.iter().position(|f| std::ptr::eq(f, field)) {
                        *expr = column_key(source, column);
                        return ControlFlow::Continue(());
                    }
                }
            }
            if let Some(source) = crate::variant_cast::source(expr) {
                *expr = source.clone();
            } else {
                match expr {
                    Expr::Identifier(id) => id.value = id.value.to_lowercase(),
                    Expr::CompoundIdentifier(ids) => {
                        for id in ids {
                            id.value = id.value.to_lowercase();
                        }
                    }
                    Expr::Nested(inner) => *expr = *inner.clone(),
                    Expr::Function(f) => {
                        for part in &mut f.name.0 {
                            if let ObjectNamePart::Identifier(id) = part {
                                id.value = id.value.to_lowercase();
                            }
                        }
                    }
                    _ => {}
                }
            }
            ControlFlow::Continue(())
        }
    }
    let mut value = expr.clone();
    let _ = VisitMut::visit(&mut value, &mut Normalize { sources, outer });
    value
}

fn column_key(source: usize, column: usize) -> Expr {
    Expr::Value(Value::Placeholder(format!("group-column:{source}:{column}")).into())
}
