//! Type-bound trailing-space equality for ANSI operands. No backend effects.
use super::*;
use msduck_core::diagnostic::SqlError;
use std::ops::ControlFlow;

type Rows = Vec<Option<Vec<Source>>>;

struct SelectBindings {
    outer: Rows,
    // Node identities are used only during this immutable AST traversal. They
    // never escape into a plan or affect name resolution through iteration order.
    on: HashMap<*const Expr, Option<Vec<Source>>>,
    apply: HashMap<*const Query, Option<Vec<Source>>>,
}

fn select_bindings(
    catalog: &CatalogSnapshot,
    select: &Select,
    scope: &Scope,
    outer: Rows,
) -> SelectBindings {
    let mut result = SelectBindings {
        outer,
        on: HashMap::new(),
        apply: HashMap::new(),
    };
    let mut base = scope.clone();
    base.rows = result.outer.clone();
    for table in &select.from {
        let mut prefix = source(catalog, &table.relation, &base).map(|value| vec![value]);
        for join in &table.joins {
            let apply = matches!(
                join.join_operator,
                JoinOperator::CrossApply | JoinOperator::OuterApply
            );
            let mut input = base.clone();
            if apply {
                input.rows.push(prefix.clone());
                if let TableFactor::Derived { subquery, .. } = &join.relation {
                    result.apply.insert(subquery.as_ref(), prefix.clone());
                }
            }
            prefix = prefix
                .zip(source_with_correlation(
                    catalog,
                    &join.relation,
                    &input,
                    apply,
                ))
                .map(|(mut left, right)| {
                    left.push(right);
                    left
                });
            if let JoinOperator::Join(JoinConstraint::On(expr))
            | JoinOperator::Inner(JoinConstraint::On(expr))
            | JoinOperator::Left(JoinConstraint::On(expr))
            | JoinOperator::LeftOuter(JoinConstraint::On(expr))
            | JoinOperator::Right(JoinConstraint::On(expr))
            | JoinOperator::RightOuter(JoinConstraint::On(expr))
            | JoinOperator::FullOuter(JoinConstraint::On(expr)) = &join.join_operator
            {
                result.on.insert(expr, prefix.clone());
            }
        }
    }
    result
}

fn plan(catalog: &CatalogSnapshot, query: &Query, outer: &Scope) -> Result<Vec<usize>, SqlError> {
    struct Check<'a> {
        catalog: &'a CatalogSnapshot,
        outer: &'a Scope,
        queries: Vec<QueryScopes>,
        derived: Option<bool>,
        selects: Vec<SelectBindings>,
        saved_rows: Vec<(*const Expr, Rows)>,
        position: usize,
        positions: Vec<usize>,
    }
    impl Visitor for Check<'_> {
        type Break = SqlError;

        fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<SqlError> {
            if let TableFactor::Derived { lateral, .. } = factor {
                self.derived = Some(*lateral);
            }
            ControlFlow::Continue(())
        }

        fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<SqlError> {
            let mut inherited = self
                .queries
                .last_mut()
                .map(|parent| {
                    parent
                        .definitions
                        .pop_front()
                        .unwrap_or_else(|| parent.body.clone())
                })
                .unwrap_or_else(|| self.outer.clone());
            let derived = self.derived.take();
            if let Some((frame, rows)) = self.selects.last().and_then(|frame| {
                frame
                    .apply
                    .get(&(query as *const Query))
                    .map(|rows| (frame, rows))
            }) {
                inherited.rows = frame.outer.clone();
                inherited.rows.push(rows.clone());
            } else if derived == Some(false) {
                inherited.rows.clear();
            }
            self.queries.push(scopes(self.catalog, query, &inherited));
            ControlFlow::Continue(())
        }

        fn post_visit_query(&mut self, _: &Query) -> ControlFlow<SqlError> {
            self.queries.pop();
            ControlFlow::Continue(())
        }

        fn pre_visit_select(&mut self, select: &Select) -> ControlFlow<SqlError> {
            let query = self.queries.last_mut().unwrap();
            let scope = &mut query.body;
            self.selects.push(select_bindings(
                self.catalog,
                select,
                scope,
                query.inherited.rows.clone(),
            ));
            // Rebind this SELECT against its inherited rows, not the complete
            // local frame already retained for query-level ORDER BY. Otherwise
            // an APPLY input can accidentally find a later alias in that frame.
            let mut declarations = scope.clone();
            declarations.rows = query.inherited.rows.clone();
            scope
                .rows
                .push(sources(self.catalog, select, &declarations));
            ControlFlow::Continue(())
        }

        fn post_visit_select(&mut self, _: &Select) -> ControlFlow<SqlError> {
            self.queries.last_mut().unwrap().body.rows.pop();
            self.selects.pop();
            ControlFlow::Continue(())
        }

        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<SqlError> {
            if let Some(frame) = self.selects.last()
                && let Some(rows) = frame.on.get(&(expr as *const Expr))
            {
                let scope = &mut self.queries.last_mut().unwrap().body;
                let mut local = frame.outer.clone();
                local.push(rows.clone());
                self.saved_rows
                    .push((expr, std::mem::replace(&mut scope.rows, local)));
            }
            ControlFlow::Continue(())
        }

        fn post_visit_expr(&mut self, expr: &Expr) -> ControlFlow<SqlError> {
            if let Expr::BinaryOp {
                left,
                op: BinaryOperator::Eq | BinaryOperator::NotEq,
                right,
            } = expr
            {
                let scope = &self.queries.last().unwrap().body;
                let operands = [left.as_ref(), right.as_ref()];
                let types = operands.map(|value| {
                    member_expression(self.catalog, value, &[], scope)
                        .and_then(|info| info.system_type_id)
                });
                let ansi = operands.into_iter().zip(types).all(|(value, kind)| {
                    conditional::literal_null(value) || matches!(kind, Some(167 | 175))
                }) && types.iter().any(|kind| matches!(kind, Some(167 | 175)));
                let bin2 = operands.iter().any(|value| {
                    expression_collation(self.catalog, value, &[], scope)
                        .and_then(Result::ok)
                        .and_then(|label| label.name().map(str::to_owned))
                        .is_some_and(|name| name.to_ascii_uppercase().ends_with("_BIN2"))
                });
                if ansi && !bin2 {
                    self.positions.push(self.position);
                }
            }
            self.position += 1;
            if self
                .saved_rows
                .last()
                .is_some_and(|(node, _)| std::ptr::eq(*node, expr))
            {
                self.queries.last_mut().unwrap().body.rows = self.saved_rows.pop().unwrap().1;
            }
            ControlFlow::Continue(())
        }
    }
    let mut check = Check {
        catalog,
        outer,
        queries: vec![],
        derived: None,
        selects: vec![],
        saved_rows: vec![],
        position: 0,
        positions: vec![],
    };
    match query.visit(&mut check) {
        ControlFlow::Break(error) => Err(error),
        ControlFlow::Continue(()) => Ok(check.positions),
    }
}

/// SQL pads character equality operands with spaces. Trimming U+0020 gives
/// equivalent equality without inventing ordering or linguistic weights.
/// Call after collation validation; BIN2 retains its dedicated comparison path.
pub fn lower(catalog: &CatalogSnapshot, query: &mut Query, outer: &Scope) -> Result<(), SqlError> {
    let positions = plan(catalog, query, outer)?;
    struct Lower {
        position: usize,
        pending: std::iter::Peekable<std::vec::IntoIter<usize>>,
    }
    fn operand(value: &mut Box<Expr>) {
        if matches!(value.as_ref(), Expr::Function(f) if f.name.to_string() == "__msduck_rtrim") {
            return;
        }
        let input = std::mem::replace(value.as_mut(), crate::expr::number(0));
        **value = crate::expr::unary_function("__msduck_rtrim", input);
    }
    impl VisitorMut for Lower {
        type Break = ();
        fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
            if self.pending.peek() == Some(&self.position) {
                self.pending.next();
                let Expr::BinaryOp { left, right, .. } = expr else {
                    unreachable!("planned equality")
                };
                operand(left);
                operand(right);
            }
            self.position += 1;
            ControlFlow::Continue(())
        }
    }
    let _ = query.visit(&mut Lower {
        position: 0,
        pending: positions.into_iter().peekable(),
    });
    Ok(())
}
