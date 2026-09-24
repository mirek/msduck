//! Select one joined input per target before evaluating UPDATE assignments.
//! The adapter must materialize this query before constructing paired images.
use anyhow::{Result, ensure};
use sqlparser::{ast::*, dialect::DuckDbDialect, parser::Parser};
use std::ops::ControlFlow;

pub struct Column {
    pub reference: Vec<Ident>,
    pub slot: Ident,
}

pub struct Plan {
    pub capture: Box<Query>,
    pub identity: Ident,
    pub columns: Vec<Column>,
}

impl Plan {
    pub fn slot(&self, reference: &[Ident]) -> Option<&Ident> {
        self.columns
            .iter()
            .find(|column| {
                column.reference.len() == reference.len()
                    && column
                        .reference
                        .iter()
                        .zip(reference)
                        .all(|(a, b)| a.value.eq_ignore_ascii_case(&b.value))
            })
            .map(|column| &column.slot)
    }

    /// Rebind a scalar expression whose columns have already been resolved to
    /// the canonical qualified references in this plan. Local/global parameters
    /// remain explicit inputs. Query scopes must be bound separately: blindly
    /// rewriting a same-named column inside a subquery can capture the wrong row.
    /// Return a new AST, leaving the caller's input intact even on failure.
    pub fn rebind(&self, expression: &Expr, alias: Ident) -> Result<Expr> {
        use msduck_core::diagnostic::SqlError;
        struct Rebind<'a> {
            plan: &'a Plan,
            alias: Ident,
            parents: Vec<(bool, usize)>,
        }
        impl VisitorMut for Rebind<'_> {
            type Break = anyhow::Error;
            fn pre_visit_query(&mut self, _: &mut Query) -> ControlFlow<Self::Break> {
                ControlFlow::Break(anyhow::anyhow!(
                    "joined image rebinding requires separately bound subquery scopes"
                ))
            }
            fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<Self::Break> {
                let datepart = self.parents.last_mut().is_some_and(|(calendar, children)| {
                    let first = *calendar && *children == 0;
                    *children += 1;
                    first
                });
                let calendar = matches!(expr, Expr::Function(function) if matches!(function.name.to_string().to_ascii_uppercase().as_str(), "DATEPART" | "DATENAME" | "DATEADD" | "DATEDIFF" | "DATEDIFF_BIG"));
                self.parents.push((calendar, 0));
                match expr {
                    Expr::CompoundIdentifier(reference) => {
                        if let Some(slot) = self.plan.slot(reference) {
                            *expr =
                                Expr::CompoundIdentifier(vec![self.alias.clone(), slot.clone()]);
                        } else {
                            let name = reference.last().map_or("", |id| id.value.as_str());
                            let known = self.plan.columns.iter().any(|column| {
                                column.reference.len() == reference.len()
                                    && column.reference[..column.reference.len() - 1]
                                        .iter()
                                        .zip(&reference[..reference.len() - 1])
                                        .all(|(a, b)| a.value.eq_ignore_ascii_case(&b.value))
                            });
                            let error = if known {
                                SqlError::new(207, 1, format!("Invalid column name '{name}'."))
                            } else {
                                let reference = reference
                                    .iter()
                                    .map(|id| id.value.as_str())
                                    .collect::<Vec<_>>()
                                    .join(".");
                                SqlError::new(
                                    4104,
                                    1,
                                    format!(
                                        "The multi-part identifier \"{reference}\" could not be bound."
                                    ),
                                )
                            };
                            return ControlFlow::Break(error.into());
                        }
                    }
                    Expr::Identifier(id)
                        if !(datepart || id.quote_style.is_none() && id.value.starts_with('@')) =>
                    {
                        return ControlFlow::Break(
                            SqlError::new(207, 1, format!("Invalid column name '{}'.", id.value))
                                .into(),
                        );
                    }
                    _ => {}
                }
                ControlFlow::Continue(())
            }
            fn post_visit_expr(&mut self, _: &mut Expr) -> ControlFlow<Self::Break> {
                self.parents.pop();
                ControlFlow::Continue(())
            }
        }
        let mut result = expression.clone();
        match VisitMut::visit(
            &mut result,
            &mut Rebind {
                plan: self,
                alias,
                parents: vec![],
            },
        ) {
            ControlFlow::Continue(()) => Ok(result),
            ControlFlow::Break(error) => Err(error),
        }
    }
}

/// All references must be qualified columns already resolved in the supplied
/// FROM scope. Capturing columns rather than assignment expressions provides
/// the evaluation boundary: discarded matches cannot execute assignments.
/// Preserve the original join tree; flattening an outer join changes its rows.
pub fn plan(
    with: Option<With>,
    sources: Vec<TableWithJoins>,
    selection: Option<Expr>,
    target_identity: Vec<Ident>,
    columns: Vec<Vec<Ident>>,
) -> Result<Plan> {
    ensure!(!sources.is_empty(), "missing joined OUTPUT sources");
    ensure!(
        target_identity.len() >= 2,
        "target identity must be qualified"
    );
    let identity_expr = Expr::CompoundIdentifier(target_identity);
    let identity = Ident::with_quote('"', "__msduck_identity");
    let mut seen = std::collections::HashSet::new();
    let columns = columns
        .into_iter()
        .enumerate()
        .map(|(index, reference)| {
            ensure!(
                reference.len() >= 2,
                "captured source column must be qualified"
            );
            let key = reference
                .iter()
                .map(|id| id.value.to_lowercase())
                .collect::<Vec<_>>();
            ensure!(seen.insert(key), "duplicate captured source column");
            Ok(Column {
                reference,
                slot: Ident::with_quote('"', format!("__msduck_source_{index}")),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    // Only a fixed backend template is parsed; user inputs remain AST nodes.
    let Statement::Query(mut capture) = Parser::parse_sql(
        &DuckDbDialect {},
        "SELECT 1 FROM __source QUALIFY ROW_NUMBER() OVER(PARTITION BY __identity)=1",
    )?
    .remove(0) else {
        unreachable!()
    };
    capture.with = with;
    let SetExpr::Select(select) = capture.body.as_mut() else {
        unreachable!()
    };
    select.from = sources;
    let present = Expr::IsNotNull(Box::new(identity_expr.clone()));
    select.selection = Some(match selection {
        Some(selection) => Expr::BinaryOp {
            left: Box::new(Expr::Nested(Box::new(selection))),
            op: BinaryOperator::And,
            right: Box::new(present),
        },
        None => present,
    });
    select.projection = vec![SelectItem::ExprWithAlias {
        expr: identity_expr.clone(),
        alias: identity.clone(),
    }];
    select
        .projection
        .extend(columns.iter().map(|column| SelectItem::ExprWithAlias {
            expr: Expr::CompoundIdentifier(column.reference.clone()),
            alias: column.slot.clone(),
        }));
    let Some(Expr::BinaryOp { left, .. }) = &mut select.qualify else {
        unreachable!()
    };
    let Expr::Function(function) = left.as_mut() else {
        unreachable!()
    };
    let Some(WindowType::WindowSpec(window)) = &mut function.over else {
        unreachable!()
    };
    window.partition_by = vec![identity_expr];
    Ok(Plan {
        capture,
        identity,
        columns,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn reference(alias: &str, name: &str) -> Vec<Ident> {
        vec![Ident::with_quote('"', alias), Ident::with_quote('"', name)]
    }
    #[test]
    fn preserves_outer_join_cte_parameters_and_column_names_without_mutation() {
        let Statement::Query(query) = crate::batch::parse("WITH s AS (SELECT id,n FROM source) SELECT 1 FROM target t FULL JOIN s ON t.id=s.id WHERE t.n>@p OR s.n IS NULL").unwrap().remove(0) else { unreachable!() };
        let before = query.clone();
        let SetExpr::Select(select) = query.body.as_ref() else {
            unreachable!()
        };
        let plan = plan(
            query.with.clone(),
            select.from.clone(),
            select.selection.clone(),
            reference("t", "rowid"),
            vec![
                reference("t", "id"),
                reference("s", "id"),
                reference("s", "odd.name"),
            ],
        )
        .unwrap();
        let SetExpr::Select(capture) = plan.capture.body.as_ref() else {
            unreachable!()
        };
        assert_eq!(capture.from, select.from);
        assert_eq!(plan.capture.with, query.with);
        let sql = plan.capture.to_string();
        assert!(sql.contains("FULL JOIN"));
        assert!(sql.contains("@p"));
        assert!(sql.contains("AND \"t\".\"rowid\" IS NOT NULL"));
        assert!(sql.contains("PARTITION BY \"t\".\"rowid\""));
        assert!(!sql.contains("ORDER BY"));
        assert_eq!(
            plan.slot(&reference("S", "ODD.NAME")).unwrap().value,
            "__msduck_source_2"
        );
        assert!(plan.slot(&reference("s", "missing")).is_none());
        assert_eq!(query, before);
    }
    #[test]
    fn rejects_unbound_and_duplicate_source_references() {
        let Statement::Query(query) = crate::batch::parse("SELECT 1 FROM t").unwrap().remove(0)
        else {
            unreachable!()
        };
        let SetExpr::Select(select) = query.body.as_ref() else {
            unreachable!()
        };
        for columns in [
            vec![vec![Ident::new("n")]],
            vec![reference("t", "n"), reference("T", "N")],
        ] {
            assert!(
                plan(
                    None,
                    select.from.clone(),
                    None,
                    reference("t", "rowid"),
                    columns
                )
                .is_err()
            );
        }
    }

    #[test]
    fn expression_rebinding_keeps_parameters_and_rejects_unresolved_scopes_atomically() {
        let Statement::Query(query) = crate::batch::parse("SELECT 1 FROM t JOIN s ON t.id=s.id")
            .unwrap()
            .remove(0)
        else {
            unreachable!()
        };
        let SetExpr::Select(select) = query.body.as_ref() else {
            unreachable!()
        };
        let plan = plan(
            None,
            select.from.clone(),
            None,
            reference("t", "rowid"),
            vec![
                reference("t", "n"),
                reference("s", "extra"),
                reference("s", "stamp"),
            ],
        )
        .unwrap();
        let expression = |sql: &str| {
            let Statement::Query(query) =
                Parser::parse_sql(&crate::dialect::ServerDialect, &format!("SELECT {sql}"))
                    .unwrap()
                    .remove(0)
            else {
                unreachable!()
            };
            let SetExpr::Select(select) = *query.body else {
                unreachable!()
            };
            let SelectItem::UnnamedExpr(expr) = select.projection.into_iter().next().unwrap()
            else {
                unreachable!()
            };
            expr
        };
        let original = expression("t.n+COALESCE(s.extra,0)+@delta+@@ROWCOUNT");
        let before = original.clone();
        let rewritten = plan
            .rebind(&original, Ident::with_quote('"', "selected rows"))
            .unwrap()
            .to_string();
        assert!(rewritten.contains("\"selected rows\".\"__msduck_source_0\""));
        assert!(rewritten.contains("\"selected rows\".\"__msduck_source_1\""));
        assert!(rewritten.contains("@delta") && rewritten.contains("@@ROWCOUNT"));
        assert_eq!(original, before);
        assert!(
            plan.rebind(&expression("DATEPART(day,s.stamp)"), Ident::new("selected"))
                .unwrap()
                .to_string()
                .contains("day")
        );
        for (sql, number) in [
            ("t.n+s.missing", 207),
            ("other.n", 4104),
            ("n", 207),
            ("[@delta]", 207),
        ] {
            let original = expression(sql);
            let before = original.clone();
            let error = plan.rebind(&original, Ident::new("selected")).unwrap_err();
            assert_eq!(
                error
                    .downcast_ref::<msduck_core::diagnostic::SqlError>()
                    .unwrap()
                    .number,
                number
            );
            assert_eq!(original, before);
        }
        let original = expression("t.n+(SELECT t.n FROM other t)");
        let before = original.clone();
        assert!(
            plan.rebind(&original, Ident::new("selected"))
                .unwrap_err()
                .to_string()
                .contains("subquery scopes")
        );
        assert_eq!(original, before);
    }
}
