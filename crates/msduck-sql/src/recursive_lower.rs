//! Bounded recursive CTE execution lowering over explicit catalog snapshots.
use crate::{
    binding_scope::{Field, QueryScopes, Scope},
    catalog_snapshot::CatalogSnapshot,
    cte_recursion,
    expr::{number, unary_function},
    projection,
};
use sqlparser::{ast::*, parser::Parser};
use std::ops::ControlFlow;

pub fn exhaustion_message(limit: u16) -> String {
    format!(
        "The statement terminated. The maximum recursion {limit} has been exhausted before statement completion."
    )
}
pub fn diagnostic(message: &str) -> Option<msduck_core::diagnostic::SqlError> {
    let message = message
        .strip_prefix("Invalid Input Error: ")
        .unwrap_or(message);
    let limit = message
        .strip_prefix("The statement terminated. The maximum recursion ")?
        .strip_suffix(" has been exhausted before statement completion.")?
        .parse::<u16>()
        .ok()?;
    (limit > 0 && limit <= 32767).then(|| msduck_core::diagnostic::SqlError::new(530, 1, message))
}

pub fn required<T: Visit>(node: &T) -> bool {
    struct Find;
    impl Visitor for Find {
        type Break = ();
        fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<()> {
            if crate::query_options::has(query)
                || query.with.as_ref().is_some_and(|with| {
                    with.cte_tables
                        .iter()
                        .any(|cte| cte_recursion::references(&cte.query, &cte.alias.name.value) > 0)
                })
            {
                return ControlFlow::Break(());
            }
            ControlFlow::Continue(())
        }
    }
    node.visit(&mut Find).is_break()
}

pub fn lower<T: Visit + VisitMut + Clone>(
    node: &mut T,
    catalog: &CatalogSnapshot,
) -> Result<(), String> {
    if !required(node) {
        return Ok(());
    }
    struct Lower<'a> {
        catalog: &'a CatalogSnapshot,
        scopes: Vec<QueryScopes>,
        limits: Vec<u16>,
    }
    impl VisitorMut for Lower<'_> {
        type Break = String;
        fn pre_visit_query(&mut self, query: &mut Query) -> ControlFlow<String> {
            let limit = crate::query_options::take(query)
                .unwrap_or_else(|| self.limits.last().copied().unwrap_or(100));
            self.limits.push(limit);
            let inherited = self
                .scopes
                .last_mut()
                .map(|parent| {
                    parent
                        .definitions
                        .pop_front()
                        .unwrap_or_else(|| parent.body.clone())
                })
                .unwrap_or_default();
            self.scopes
                .push(projection::scopes(self.catalog, query, &inherited));
            ControlFlow::Continue(())
        }
        fn post_visit_query(&mut self, query: &mut Query) -> ControlFlow<String> {
            let limit = self.limits.pop().unwrap();
            let inherited = self.scopes.pop().unwrap().inherited;
            // Children have now been lowered. Newly generated recursive queries
            // are not revisited, so this pass cannot wrap its own output again.
            let scopes = projection::scopes(self.catalog, query, &inherited);
            if let Some(with) = &mut query.with {
                for (cte, scope) in with.cte_tables.iter_mut().zip(scopes.definitions) {
                    if cte_recursion::references(&cte.query, &cte.alias.name.value) > 0
                        && let Err(error) = lower_cte(cte, self.catalog, &scope, limit)
                    {
                        return ControlFlow::Break(error);
                    }
                }
            }
            ControlFlow::Continue(())
        }
    }
    let mut staged = node.clone();
    match VisitMut::visit(
        &mut staged,
        &mut Lower {
            catalog,
            scopes: Vec::new(),
            limits: Vec::new(),
        },
    ) {
        ControlFlow::Break(error) => Err(error),
        ControlFlow::Continue(()) => {
            *node = staged;
            Ok(())
        }
    }
}
fn query(body: SetExpr) -> Box<Query> {
    let Statement::Query(mut q) =
        Parser::parse_sql(&sqlparser::dialect::GenericDialect {}, "SELECT 1")
            .unwrap()
            .remove(0)
    else {
        unreachable!()
    };
    *q.body = body;
    q
}
fn combine(mut members: Vec<SetExpr>) -> SetExpr {
    let first = members.remove(0);
    members
        .into_iter()
        .fold(first, |left, right| SetExpr::SetOperation {
            op: SetOperator::Union,
            set_quantifier: SetQuantifier::All,
            left: Box::new(left),
            right: Box::new(right),
        })
}
fn split(body: &SetExpr, members: &mut Vec<SetExpr>) {
    if let SetExpr::SetOperation {
        op: SetOperator::Union,
        set_quantifier: SetQuantifier::All,
        left,
        right,
    } = body
    {
        split(left, members);
        split(right, members);
    } else {
        members.push(body.clone());
    }
}
fn lower_cte(
    cte: &mut Cte,
    catalog: &CatalogSnapshot,
    outer: &Scope,
    limit: u16,
) -> Result<(), String> {
    cte_recursion::validate(cte).map_err(|e| e.message)?;
    let name = cte.alias.name.value.clone();
    if cte.query.with.is_some()
        || cte.query.order_by.is_some()
        || cte.query.limit_clause.is_some()
        || cte.query.for_clause.is_some()
    {
        return Err("unsupported clauses in recursive CTE definition".into());
    }
    let mut members = Vec::new();
    split(&cte.query.body, &mut members);
    let first = members
        .iter()
        .position(|m| cte_recursion::references(m, &name) > 0)
        .unwrap();
    let recursive = members.split_off(first);
    let anchor = combine(members);
    let mut fields = member_fields(catalog, &anchor, outer).unwrap_or_default();
    if !cte.alias.columns.is_empty() {
        if fields.len() != cte.alias.columns.len() {
            fields = cte
                .alias
                .columns
                .iter()
                .map(|c| Field {
                    collation: None,
                    name: c.name.value.clone(),
                    info: None,
                    properties: Default::default(),
                    json_fragment: false,
                })
                .collect();
        } else {
            for (f, c) in fields.iter_mut().zip(&cte.alias.columns) {
                f.name = c.name.value.clone();
            }
        }
    }
    if fields.is_empty() {
        return Err("recursive CTE requires known output columns".into());
    }
    struct Names(std::collections::HashSet<String>);
    impl Visitor for Names {
        type Break = ();
        fn pre_visit_ident(&mut self, id: &Ident) -> ControlFlow<()> {
            self.0.insert(id.value.to_lowercase());
            ControlFlow::Continue(())
        }
    }
    let mut names = Names(Default::default());
    let _ = Visit::visit(cte, &mut names);
    let mut depth = "__msduck_recursion_depth".to_string();
    while names.0.contains(&depth) {
        depth.push('_');
    }
    let mut scope = outer.clone();
    scope.insert(name.to_lowercase(), fields.clone());
    for member in &recursive {
        if let Some(actual) = member_fields(catalog, member, &scope) {
            for (expected, actual) in fields.iter().zip(actual) {
                if let (Some(a), Some(b)) = (&expected.info, &actual.info)
                    && (a.system_type_id, a.max_length, a.precision, a.scale)
                        != (b.system_type_id, b.max_length, b.precision, b.scale)
                {
                    return Err(format!(
                        "Types don't match between the anchor and the recursive part in column \"{}\" of recursive query \"{name}\".",
                        expected.name
                    ));
                }
            }
        }
    }
    let anchor = add_depth(anchor, catalog, outer, &name, &depth, false, limit)?;
    let recursive = recursive
        .into_iter()
        .map(|member| add_depth(member, catalog, &scope, &name, &depth, true, limit))
        .collect::<Result<Vec<_>, _>>()?;
    let mut inner = cte.clone();
    inner.alias.columns = fields
        .iter()
        .map(|f| TableAliasColumnDef {
            name: Ident::with_quote('"', &f.name),
            data_type: None,
        })
        .collect();
    inner.alias.columns.push(TableAliasColumnDef {
        name: Ident::with_quote('"', &depth),
        data_type: None,
    });
    inner.query = query(SetExpr::SetOperation {
        op: SetOperator::Union,
        set_quantifier: SetQuantifier::All,
        left: Box::new(anchor),
        right: Box::new(SetExpr::Query(query(combine(recursive)))),
    });
    let Statement::Query(mut wrapper) = Parser::parse_sql(
        &sqlparser::dialect::GenericDialect {},
        "WITH RECURSIVE placeholder AS (SELECT 1) SELECT 1 FROM placeholder WHERE 1<=100",
    )
    .unwrap()
    .remove(0) else {
        unreachable!()
    };
    wrapper.with.as_mut().unwrap().cte_tables = vec![inner];
    let SetExpr::Select(select) = wrapper.body.as_mut() else {
        unreachable!()
    };
    select.projection = fields
        .iter()
        .map(|f| SelectItem::UnnamedExpr(Expr::Identifier(Ident::with_quote('"', &f.name))))
        .collect();
    let TableFactor::Table { name: table, .. } = &mut select.from[0].relation else {
        unreachable!()
    };
    *table = ObjectName::from(vec![cte.alias.name.clone()]);
    select.selection = (limit > 0).then(|| Expr::BinaryOp {
        left: Box::new(Expr::Identifier(Ident::with_quote('"', depth))),
        op: BinaryOperator::LtEq,
        right: Box::new(number(limit)),
    });
    cte.query = wrapper;
    Ok(())
}
fn member_fields(catalog: &CatalogSnapshot, body: &SetExpr, scope: &Scope) -> Option<Vec<Field>> {
    projection::member_fields(catalog, &query(body.clone()), scope)
}

fn add_depth(
    body: SetExpr,
    catalog: &CatalogSnapshot,
    scope: &Scope,
    name: &str,
    depth: &str,
    recursive: bool,
    limit: u16,
) -> Result<SetExpr, String> {
    if let SetExpr::SetOperation {
        op,
        set_quantifier,
        left,
        right,
    } = body
    {
        return Ok(SetExpr::SetOperation {
            op,
            set_quantifier,
            left: Box::new(add_depth(
                *left, catalog, scope, name, depth, recursive, limit,
            )?),
            right: Box::new(add_depth(
                *right, catalog, scope, name, depth, recursive, limit,
            )?),
        });
    }
    let mut q = query(body);
    let SetExpr::Select(select) = q.body.as_ref() else {
        return Err("unsupported recursive CTE member query shape".into());
    };
    if select.projection.iter().any(|item| {
        matches!(
            item,
            SelectItem::Wildcard(..) | SelectItem::QualifiedWildcard(..)
        )
    }) {
        projection::expand_stars(catalog, &mut q, scope)
            .ok_or("recursive CTE star requires known source columns")?;
    }
    let SetExpr::Select(select) = q.body.as_mut() else {
        unreachable!()
    };
    let value = if recursive {
        let source = select
            .from
            .iter()
            .flat_map(|t| std::iter::once(&t.relation).chain(t.joins.iter().map(|j| &j.relation)))
            .find_map(|f| match f {
                TableFactor::Table {
                    name: table,
                    alias,
                    args: None,
                    ..
                } if table.0.len() == 1
                    && table.0[0]
                        .as_ident()
                        .is_some_and(|i| i.value.eq_ignore_ascii_case(name)) =>
                {
                    Some(
                        alias
                            .as_ref()
                            .map(|a| a.name.clone())
                            .unwrap_or_else(|| table.0[0].as_ident().unwrap().clone()),
                    )
                }
                _ => None,
            })
            .ok_or("recursive self reference must be a direct FROM/JOIN source")?;
        let reference = Expr::CompoundIdentifier(vec![source, Ident::with_quote('"', depth)]);
        let mut parser = Parser::new(&sqlparser::dialect::GenericDialect {})
            .try_with_sql("CASE WHEN 1>=100 THEN error('x') ELSE 1+1 END")
            .unwrap();
        let mut guard = parser.parse_expr().unwrap();
        if let Expr::Case {
            conditions,
            else_result,
            ..
        } = &mut guard
        {
            conditions[0].condition = Expr::BinaryOp {
                left: Box::new(reference.clone()),
                op: BinaryOperator::GtEq,
                right: Box::new(number(limit)),
            };
            conditions[0].result = unary_function(
                "error",
                Expr::Value(Value::SingleQuotedString(exhaustion_message(limit)).into()),
            );
            *else_result = Some(Box::new(Expr::BinaryOp {
                left: Box::new(reference),
                op: BinaryOperator::Plus,
                right: Box::new(number(1)),
            }));
        }
        if limit == 0 { number(0) } else { guard }
    } else {
        number(0)
    };
    select.projection.push(SelectItem::ExprWithAlias {
        expr: value,
        alias: Ident::with_quote('"', depth),
    });
    Ok(*q.body)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn statement(sql: &str) -> Statement {
        Parser::parse_sql(&crate::dialect::ServerDialect, sql)
            .unwrap()
            .remove(0)
    }
    #[test]
    fn query_limits_are_consumed_and_zero_omits_the_guard() {
        let source =
            "WITH r(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM r WHERE n<150) SELECT n FROM r";
        let sql =
            format!("{source} OPTION(MAXRECURSION 0); {source} OPTION(MAXRECURSION 2); {source}");
        let mut statements = Parser::parse_sql(&crate::dialect::ServerDialect, &sql).unwrap();
        lower(&mut statements, &CatalogSnapshot::default()).unwrap();
        assert!(!statements[0].to_string().contains("error("));
        assert!(statements[1].to_string().contains(&exhaustion_message(2)));
        assert!(statements[2].to_string().contains(&exhaustion_message(100)));
        assert!(
            statements
                .iter()
                .all(|s| !s.to_string().contains("SETTINGS"))
        );
        assert_eq!(
            diagnostic(&format!("Invalid Input Error: {}", exhaustion_message(2)))
                .unwrap()
                .message,
            exhaustion_message(2)
        );
        assert!(diagnostic("The statement terminated. The maximum recursion 0 has been exhausted before statement completion.").is_none());
    }

    #[test]
    fn private_depth_is_hygienic_and_lowering_failure_is_atomic() {
        let mut node = statement(
            "WITH r(__msduck_recursion_depth) AS (SELECT nextval('calls') UNION ALL SELECT r.* FROM r WHERE __msduck_recursion_depth<0) SELECT * FROM r",
        );
        lower(&mut node, &CatalogSnapshot::default()).unwrap();
        let sql = node.to_string();
        assert!(sql.contains("WITH RECURSIVE"));
        assert!(sql.contains("\"__msduck_recursion_depth_\""));
        assert_eq!(sql.matches("nextval").count(), 1);
        let Statement::Query(outer) = node else {
            panic!()
        };
        let cte = &outer.with.as_ref().unwrap().cte_tables[0];
        let SetExpr::Select(wrapper) = cte.query.body.as_ref() else {
            panic!()
        };
        assert_eq!(wrapper.projection.len(), 1);
        let mut invalid =
            statement("WITH r(n) AS (SELECT 1 UNION ALL SELECT DISTINCT n FROM r) SELECT * FROM r");
        let before = invalid.clone();
        assert!(lower(&mut invalid, &CatalogSnapshot::default()).is_err());
        assert_eq!(invalid, before);
    }
}
