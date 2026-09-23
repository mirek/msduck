//! Resolve assignment references against explicit lexical/catalog snapshots.
//! Only references bound to the captured UPDATE inputs become private slots.
use crate::{binding_scope::Scope, catalog_snapshot::CatalogSnapshot, output_join::Plan};
use anyhow::{Result, ensure};
use msduck_core::diagnostic::SqlError;
use sqlparser::ast::*;
use std::ops::ControlFlow;

pub struct Context<'a> {
    pub candidates: &'a Plan,
    pub catalog: &'a CatalogSnapshot,
    pub scope: &'a Scope,
}

#[derive(Clone)]
struct Source {
    qualifiers: Vec<Vec<Ident>>,
    columns: Vec<Ident>,
    slots: Option<Vec<Ident>>,
}
#[derive(Clone)]
struct Environment {
    metadata: Scope,
    frames: Vec<Vec<Source>>,
}
fn same(left: &[Ident], right: &[Ident]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(a, b)| a.value.eq_ignore_ascii_case(&b.value))
}
fn query(body: SetExpr) -> Query {
    Query {
        with: None,
        body: Box::new(body),
        order_by: None,
        limit_clause: None,
        fetch: None,
        locks: vec![],
        for_clause: None,
        settings: None,
        format_clause: None,
        pipe_operators: vec![],
    }
}
fn frame(select: &Select, metadata: &Scope, capture: Option<&Plan>) -> Result<Vec<Source>> {
    let fields = metadata
        .rows
        .last()
        .and_then(Option::as_ref)
        .ok_or_else(|| anyhow::anyhow!("assignment binding requires resolved subquery sources"))?;
    let factors = select
        .from
        .iter()
        .flat_map(|tree| {
            std::iter::once(&tree.relation).chain(tree.joins.iter().map(|join| &join.relation))
        })
        .collect::<Vec<_>>();
    ensure!(
        factors.len() == fields.len(),
        "assignment source scope mismatch"
    );
    factors
        .into_iter()
        .zip(fields)
        .map(|(factor, source)| {
            let qualifiers = crate::output_target::qualifiers(factor)?;
            let columns = source
                .fields
                .iter()
                .map(|field| Ident::with_quote('"', &field.name))
                .collect::<Vec<_>>();
            let slots = capture
                .map(|capture| {
                    columns
                        .iter()
                        .map(|column| {
                            let mut reference = qualifiers[0].clone();
                            reference.push(column.clone());
                            capture.slot(&reference).cloned().ok_or_else(|| {
                                anyhow::anyhow!("missing captured assignment column")
                            })
                        })
                        .collect::<Result<Vec<_>>>()
                })
                .transpose()?;
            Ok(Source {
                qualifiers,
                columns,
                slots,
            })
        })
        .collect()
}

struct Binder<'a> {
    catalog: &'a CatalogSnapshot,
    alias: Ident,
}
impl Binder<'_> {
    fn stars(&self, items: &mut Vec<SelectItem>, env: &Environment) -> Result<()> {
        let mut result = vec![];
        for item in items.iter() {
            if let SelectItem::QualifiedWildcard(
                SelectItemQualifiedWildcardKind::ObjectName(name),
                options,
            ) = item
            {
                let prefix = name
                    .0
                    .iter()
                    .map(|part| part.as_ident().cloned())
                    .collect::<Option<Vec<_>>>()
                    .ok_or_else(|| anyhow::anyhow!("unsupported subquery wildcard prefix"))?;
                let source = env.frames.iter().rev().find_map(|frame| {
                    let sources = frame.iter().filter(|source| source.qualifiers.iter().any(|q| same(q,&prefix))).collect::<Vec<_>>();
                    (!sources.is_empty()).then_some(sources)
                }).ok_or_else(|| SqlError::syntax(107,1,format!("The column prefix '{name}' does not match with a table name or alias name used in the query.")))?;
                ensure!(source.len() == 1, "ambiguous assignment wildcard source");
                if let Some(slots) = &source[0].slots {
                    ensure!(
                        *options == Default::default(),
                        "unsupported captured wildcard modifiers"
                    );
                    result.extend(source[0].columns.iter().zip(slots).map(|(column, slot)| {
                        SelectItem::ExprWithAlias {
                            expr: Expr::CompoundIdentifier(vec![self.alias.clone(), slot.clone()]),
                            alias: column.clone(),
                        }
                    }));
                    continue;
                }
            }
            result.push(item.clone());
        }
        *items = result;
        Ok(())
    }
    fn reference(&self, ids: &[Ident], env: &Environment) -> Result<Option<Expr>> {
        let (name, prefix) = ids
            .split_last()
            .ok_or_else(|| anyhow::anyhow!("empty column reference"))?;
        for frame in env.frames.iter().rev() {
            let sources = frame
                .iter()
                .filter(|source| {
                    prefix.is_empty() || source.qualifiers.iter().any(|q| same(q, prefix))
                })
                .collect::<Vec<_>>();
            let matching = sources
                .iter()
                .flat_map(|source| {
                    source
                        .columns
                        .iter()
                        .enumerate()
                        .filter(|(_, column)| column.value.eq_ignore_ascii_case(&name.value))
                        .map(|(index, _)| (*source, index))
                })
                .collect::<Vec<_>>();
            ensure!(
                matching.len() <= 1,
                SqlError::new(209, 1, format!("Ambiguous column name '{}'.", name.value))
            );
            if let Some((source, index)) = matching.first() {
                return Ok(source.slots.as_ref().map(|slots| {
                    Expr::CompoundIdentifier(vec![self.alias.clone(), slots[*index].clone()])
                }));
            }
            if !prefix.is_empty() && !sources.is_empty() {
                return Err(SqlError::new(
                    207,
                    1,
                    format!("Invalid column name '{}'.", name.value),
                )
                .into());
            }
        }
        Err(if prefix.is_empty() {
            SqlError::new(207, 1, format!("Invalid column name '{}'.", name.value))
        } else {
            SqlError::new(
                4104,
                1,
                format!(
                    "The multi-part identifier \"{}\" could not be bound.",
                    ids.iter()
                        .map(|id| id.value.as_str())
                        .collect::<Vec<_>>()
                        .join(".")
                ),
            )
        }
        .into())
    }

    fn walk<T: VisitMut>(&self, node: &mut T, env: &Environment) -> Result<()> {
        struct Walk<'a, 'b> {
            binder: &'a Binder<'b>,
            env: &'a Environment,
            queries: usize,
            parents: Vec<(bool, usize)>,
        }
        impl VisitorMut for Walk<'_, '_> {
            type Break = anyhow::Error;
            fn pre_visit_query(&mut self, query: &mut Query) -> ControlFlow<Self::Break> {
                if self.queries == 0
                    && let Err(error) = self.binder.query(query, self.env)
                {
                    return ControlFlow::Break(error);
                }
                self.queries += 1;
                ControlFlow::Continue(())
            }
            fn post_visit_query(&mut self, _: &mut Query) -> ControlFlow<Self::Break> {
                self.queries -= 1;
                ControlFlow::Continue(())
            }
            fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<Self::Break> {
                let keyword = self.parents.last_mut().is_some_and(|(calendar, children)| {
                    let first = *calendar && *children == 0;
                    *children += 1;
                    first
                });
                let calendar = matches!(expr,Expr::Function(f) if matches!(f.name.to_string().to_ascii_uppercase().as_str(),
                    "DATEPART"|"DATENAME"|"DATEADD"|"DATEDIFF"|"DATEDIFF_BIG"));
                self.parents.push((calendar, 0));
                if self.queries > 0 {
                    return ControlFlow::Continue(());
                }
                let ids = match expr {
                    Expr::Identifier(id)
                        if !(keyword || id.quote_style.is_none() && id.value.starts_with('@')) =>
                    {
                        std::slice::from_ref(id)
                    }
                    Expr::CompoundIdentifier(ids) => ids.as_slice(),
                    _ => return ControlFlow::Continue(()),
                };
                match self.binder.reference(ids, self.env) {
                    Ok(Some(rebound)) => *expr = rebound,
                    Ok(None) => {}
                    Err(error) => return ControlFlow::Break(error),
                }
                ControlFlow::Continue(())
            }
            fn post_visit_expr(&mut self, _: &mut Expr) -> ControlFlow<Self::Break> {
                self.parents.pop();
                ControlFlow::Continue(())
            }
        }
        match node.visit(&mut Walk {
            binder: self,
            env,
            queries: 0,
            parents: vec![],
        }) {
            ControlFlow::Continue(()) => Ok(()),
            ControlFlow::Break(error) => Err(error),
        }
    }

    fn body(&self, body: &mut SetExpr, env: &Environment) -> Result<Environment> {
        match body {
            SetExpr::Select(_) => {
                let metadata =
                    crate::projection::scopes(self.catalog, &query(body.clone()), &env.metadata)
                        .body;
                // Reborrow after reading the original body to infer source fields.
                let SetExpr::Select(select) = body else {
                    unreachable!()
                };
                let sources = frame(select, &metadata, None)?;
                ensure!(
                    !sources.iter().any(|source| source
                        .qualifiers
                        .iter()
                        .any(|q| same(q, std::slice::from_ref(&self.alias)))),
                    "private image alias collides with an assignment subquery source"
                );
                let mut local = env.clone();
                local.metadata = metadata;
                local.frames.push(sources.clone());
                let mut from = std::mem::take(&mut select.from);
                let labels = select
                    .projection
                    .iter()
                    .map(|item| match item {
                        SelectItem::UnnamedExpr(expr) => crate::projection::expression_name(expr),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                self.walk(select, &local)?;
                for (item, label) in select.projection.iter_mut().zip(labels) {
                    if let (SelectItem::UnnamedExpr(expr), Some(label)) = (&*item, label)
                        && crate::projection::expression_name(expr).as_ref() != Some(&label)
                    {
                        *item = SelectItem::ExprWithAlias {
                            expr: expr.clone(),
                            alias: Ident::with_quote('"', label),
                        };
                    }
                }
                self.stars(&mut select.projection, &local)?;
                let mut count = 0;
                for tree in &mut from {
                    self.factor(&mut tree.relation, env, &local, &sources[..count], false)?;
                    count += 1;
                    for join in &mut tree.joins {
                        let mut preceding = local.clone();
                        *preceding.frames.last_mut().unwrap() = sources[..count].to_vec();
                        *preceding.metadata.rows.last_mut().unwrap() = local
                            .metadata
                            .rows
                            .last()
                            .unwrap()
                            .as_ref()
                            .map(|sources| sources[..count].to_vec());
                        let apply = matches!(
                            join.join_operator,
                            JoinOperator::CrossApply | JoinOperator::OuterApply
                        );
                        self.factor(
                            &mut join.relation,
                            env,
                            &preceding,
                            &sources[..count],
                            apply,
                        )?;
                        count += 1;
                        let mut on = local.clone();
                        *on.frames.last_mut().unwrap() = sources[..count].to_vec();
                        *on.metadata.rows.last_mut().unwrap() = local
                            .metadata
                            .rows
                            .last()
                            .unwrap()
                            .as_ref()
                            .map(|sources| sources[..count].to_vec());
                        self.walk(&mut join.join_operator, &on)?;
                    }
                }
                select.from = from;
                Ok(local)
            }
            SetExpr::SetOperation { left, right, .. } => {
                self.body(left, env)?;
                self.body(right, env)?;
                Ok(env.clone())
            }
            SetExpr::Query(query) => {
                self.query(query, env)?;
                Ok(env.clone())
            }
            SetExpr::Values(values) => {
                self.walk(values, env)?;
                Ok(env.clone())
            }
            _ => anyhow::bail!("unsupported assignment subquery body"),
        }
    }

    fn factor(
        &self,
        factor: &mut TableFactor,
        outer: &Environment,
        local: &Environment,
        prefix: &[Source],
        apply: bool,
    ) -> Result<()> {
        let mut preceding = local.clone();
        *preceding.frames.last_mut().unwrap() = prefix.to_vec();
        *preceding.metadata.rows.last_mut().unwrap() = local
            .metadata
            .rows
            .last()
            .unwrap()
            .as_ref()
            .map(|sources| sources[..prefix.len()].to_vec());
        match factor {
            TableFactor::Derived {
                subquery, lateral, ..
            } => self.query(subquery, if *lateral || apply { &preceding } else { outer }),
            _ => self.walk(factor, &preceding),
        }
    }

    fn query(&self, query: &mut Query, env: &Environment) -> Result<()> {
        let scopes = crate::projection::scopes(self.catalog, query, &env.metadata);
        let labels = crate::projection::query_fields(self.catalog, query, &env.metadata)
            .map(|fields| {
                fields
                    .into_iter()
                    .map(|field| field.name)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if let Some(with) = &mut query.with {
            for (cte, metadata) in with.cte_tables.iter_mut().zip(scopes.definitions) {
                self.query(
                    &mut cte.query,
                    &Environment {
                        metadata,
                        frames: vec![],
                    },
                )?;
            }
        }
        let mut inherited = env.clone();
        inherited.metadata = scopes.inherited;
        inherited.metadata.ctes = scopes.body.ctes;
        let local = self.body(&mut query.body, &inherited)?;
        if let Some(order) = &mut query.order_by
            && let OrderByKind::Expressions(expressions) = &mut order.kind
        {
            for expression in expressions {
                if matches!(&expression.expr,Expr::Identifier(id) if labels.iter().any(|label| label.eq_ignore_ascii_case(&id.value)))
                {
                    continue;
                }
                self.walk(&mut expression.expr, &local)?;
            }
        }
        self.walk(&mut query.limit_clause, &local)?;
        self.walk(&mut query.fetch, &local)?;
        ensure!(
            query.pipe_operators.is_empty(),
            "assignment subquery pipes require binding"
        );
        Ok(())
    }
}

/// Resolve names to their nearest lexical scope and rebind only captured inputs.
/// Return a clone, preserving the caller's AST on both success and failure.
pub fn rebind(
    candidates: &Plan,
    expression: &Expr,
    catalog: &CatalogSnapshot,
    scope: &Scope,
    alias: Ident,
) -> Result<Expr> {
    let SetExpr::Select(select) = candidates.capture.body.as_ref() else {
        anyhow::bail!("missing candidate SELECT")
    };
    let env = Environment {
        metadata: scope.clone(),
        frames: vec![frame(select, scope, Some(candidates))?],
    };
    let mut result = expression.clone();
    Binder { catalog, alias }.walk(&mut result, &env)?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parse(sql: &str) -> Box<Query> {
        let Statement::Query(query) =
            sqlparser::parser::Parser::parse_sql(&crate::dialect::ServerDialect, sql)
                .unwrap()
                .remove(0)
        else {
            unreachable!()
        };
        query
    }
    fn expression(sql: &str) -> Expr {
        let query = parse(&format!("SELECT {sql}"));
        let SetExpr::Select(select) = *query.body else {
            unreachable!()
        };
        let SelectItem::UnnamedExpr(expr) = select.projection.into_iter().next().unwrap() else {
            unreachable!()
        };
        expr
    }
    fn inputs(from: &str) -> (Plan, CatalogSnapshot, Scope) {
        let mut catalog = CatalogSnapshot::default();
        for (table, names) in [
            ("dbo.t", vec!["id", "n"]),
            ("dbo.s", vec!["id", "extra"]),
            ("inner_table", vec!["id", "n"]),
        ] {
            catalog.tables.insert(
                table.into(),
                names
                    .into_iter()
                    .map(|name| crate::binding_scope::Field {
                        collation: None,
                        name: name.into(),
                        info: None,
                        json_fragment: false,
                        properties: Default::default(),
                    })
                    .collect(),
            );
        }
        let query = parse(&format!("SELECT * FROM {from}"));
        let scope = crate::projection::scopes(&catalog, &query, &Scope::default()).body;
        let SetExpr::Select(select) = *query.body else {
            unreachable!()
        };
        let sources = frame(&select, &scope, None).unwrap();
        let mut identity = sources[0].qualifiers[0].clone();
        identity.push(Ident::new("rowid"));
        let columns = sources
            .iter()
            .flat_map(|source| {
                source.columns.iter().map(|column| {
                    let mut reference = source.qualifiers[0].clone();
                    reference.push(column.clone());
                    reference
                })
            })
            .collect();
        let plan = crate::output_join::plan(None, select.from, None, identity, columns).unwrap();
        (plan, catalog, scope)
    }
    #[test]
    fn names_are_resolved_structurally_and_fail_without_mutating_inputs() {
        let (plan, catalog, scope) = inputs("dbo.t t CROSS JOIN dbo.s s");
        let bind = |sql| {
            rebind(
                &plan,
                &expression(sql),
                &catalog,
                &scope,
                Ident::new("captured"),
            )
        };
        assert_eq!(
            bind("n+s.extra+@p").unwrap().to_string(),
            "captured.\"__msduck_source_1\" + captured.\"__msduck_source_3\" + @p"
        );
        for (sql, number) in [
            ("id", 209),
            ("t.missing", 207),
            ("missing", 207),
            ("dbo.t.n", 4104),
            ("[s.extra]", 207),
        ] {
            let expr = expression(sql);
            let before = expr.clone();
            let error = rebind(&plan, &expr, &catalog, &scope, Ident::new("captured"))
                .err()
                .unwrap();
            assert_eq!(
                error.downcast_ref::<SqlError>().unwrap().number,
                number,
                "{sql}"
            );
            assert_eq!(expr, before);
        }
        let (plan, catalog, scope) = inputs("dbo.t CROSS JOIN dbo.s [dbo.t]");
        let actual = rebind(
            &plan,
            &expression("dbo.t.n+[dbo.t].extra+t.id"),
            &catalog,
            &scope,
            Ident::new("captured"),
        )
        .unwrap()
        .to_string();
        assert!(
            actual.contains("__msduck_source_1")
                && actual.contains("__msduck_source_3")
                && actual.contains("__msduck_source_0")
        );
    }
    #[test]
    fn scalar_subqueries_keep_nearest_names_and_capture_only_outer_columns() {
        let (plan, catalog, scope) = inputs("dbo.t t CROSS JOIN dbo.s s");
        let bind = |sql| {
            rebind(
                &plan,
                &expression(sql),
                &catalog,
                &scope,
                Ident::new("captured"),
            )
        };
        for (sql, expected) in [
            (
                "(SELECT q.n+t.n FROM inner_table q WHERE q.id=t.id)",
                "q.n + captured.\"__msduck_source_1\"",
            ),
            (
                "(SELECT t.n FROM inner_table t)",
                "SELECT t.n FROM inner_table t",
            ),
            (
                "(SELECT n FROM inner_table q)",
                "SELECT n FROM inner_table q",
            ),
            (
                "(SELECT extra FROM inner_table s)",
                "SELECT captured.\"__msduck_source_3\"",
            ),
            (
                "(SELECT d.n FROM (SELECT t.n AS n) d)",
                "captured.\"__msduck_source_1\" AS n",
            ),
            (
                "(SELECT d.n FROM inner_table q CROSS APPLY (SELECT q.n+t.n AS n) d)",
                "q.n + captured.\"__msduck_source_1\" AS n",
            ),
            (
                "(SELECT q.n FROM inner_table q UNION ALL SELECT t.n)",
                "UNION ALL SELECT captured.\"__msduck_source_1\"",
            ),
            (
                "(SELECT q.n AS id FROM inner_table q ORDER BY id)",
                "ORDER BY id",
            ),
            (
                "DATEPART(year,(SELECT t.n))",
                "DATEPART(year, (SELECT captured.\"__msduck_source_1\" AS \"n\"))",
            ),
            ("(SELECT t.*)", "captured.\"__msduck_source_0\" AS \"id\""),
        ] {
            let actual = bind(sql).unwrap().to_string();
            assert!(actual.contains(expected), "{sql}: {actual}");
        }
        for (sql, number) in [
            ("(SELECT s.extra FROM inner_table s)", 207),
            (
                "(SELECT n FROM inner_table q CROSS JOIN inner_table r)",
                209,
            ),
            (
                "(SELECT d.n FROM inner_table q CROSS JOIN (SELECT q.n AS n) d)",
                4104,
            ),
        ] {
            let error = bind(sql).err().unwrap();
            assert_eq!(
                error.downcast_ref::<SqlError>().unwrap().number,
                number,
                "{sql}"
            );
        }
        assert!(bind("(SELECT t.n FROM inner_table captured)").is_err());
        let actual = bind("(SELECT t.n FROM inner_table q JOIN inner_table j ON j.id=t.id JOIN inner_table t ON t.id=q.id)").unwrap().to_string();
        assert!(actual.contains("j.id = captured.\"__msduck_source_0\""));
        assert!(actual.contains("t.id = q.id"));
        let actual = bind("(SELECT d.n FROM (SELECT t.n) d)")
            .unwrap()
            .to_string();
        assert!(actual.contains("captured.\"__msduck_source_1\" AS \"n\""));
        assert!(bind("(WITH c AS (SELECT t.n AS n) SELECT n FROM c)").is_err());
        let actual = bind("(WITH c AS (SELECT q.n FROM inner_table q) SELECT c.n+t.n FROM c)")
            .unwrap()
            .to_string();
        assert!(actual.contains("q.n FROM inner_table q"));
        assert!(actual.contains("c.n + captured.\"__msduck_source_1\""));
    }
}
