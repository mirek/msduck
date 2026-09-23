//! Validate sensitive operations at their own expression boundaries.
use super::*;
use msduck_core::{
    collation::{Conflict, Operation},
    diagnostic::SqlError,
};
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

#[derive(Clone, Copy)]
enum Bin2Input {
    Unicode,
    Windows1252,
}

fn comparison_plan(
    catalog: &CatalogSnapshot,
    query: &Query,
    outer: &Scope,
) -> Result<Vec<(usize, Bin2Input)>, SqlError> {
    struct Check<'a> {
        catalog: &'a CatalogSnapshot,
        outer: &'a Scope,
        queries: Vec<QueryScopes>,
        derived: Option<bool>,
        selects: Vec<SelectBindings>,
        saved_rows: Vec<(*const Expr, Rows)>,
        position: usize,
        bin2: Vec<(usize, Bin2Input)>,
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
            let scope = &self.queries.last().unwrap().body;
            let label = match expr {
                Expr::BinaryOp { left, op, right } if comparison_operation(op).is_some() => {
                    let label = sensitive_collation(
                        [left.as_ref(), right.as_ref()].into_iter().map(|value| {
                            expression_collation(self.catalog, value, &[], scope).or_else(|| {
                                conditional::literal_null(value)
                                    .then(|| {
                                        self.catalog.default_collation.clone().map(|name| {
                                            Ok(msduck_core::collation::Label::CoercibleDefault(
                                                name,
                                            ))
                                        })
                                    })
                                    .flatten()
                            })
                        }),
                        comparison_operation(op).unwrap(),
                    );
                    let types = [left.as_ref(), right.as_ref()].map(|value| {
                        member_expression(self.catalog, value, &[], scope)
                            .and_then(|info| info.system_type_id)
                    });
                    let supported = [left.as_ref(), right.as_ref()].into_iter().zip(types).all(
                        |(value, kind)| {
                            conditional::literal_null(value)
                                || matches!(kind, Some(167 | 175 | 231 | 239))
                        },
                    );
                    let mode = if types.iter().any(|kind| matches!(kind, Some(231 | 239))) {
                        Some(Bin2Input::Unicode)
                    } else if types.iter().any(|kind| matches!(kind, Some(167 | 175))) {
                        Some(Bin2Input::Windows1252)
                    } else {
                        None
                    };
                    if supported
                        && label
                            .as_ref()
                            .and_then(|label| label.as_ref().ok())
                            .and_then(|label| label.name())
                            .is_some_and(|name| {
                                name.eq_ignore_ascii_case("Latin1_General_100_BIN2")
                            })
                        && let Some(mode) = mode
                    {
                        self.bin2.push((self.position, mode));
                    }
                    label
                }
                _ => expression_collation(self.catalog, expr, &[], scope),
            };
            self.position += 1;
            if let Some(Err(Conflict::Operation(error))) = label {
                return ControlFlow::Break(error);
            }
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
        bin2: vec![],
    };
    match query.visit(&mut check) {
        ControlFlow::Break(error) => Err(error),
        ControlFlow::Continue(()) => Ok(check.bin2),
    }
}

fn comparison_operation(op: &BinaryOperator) -> Option<Operation> {
    Some(match op {
        BinaryOperator::Eq => Operation::Equal,
        BinaryOperator::NotEq => Operation::NotEqual,
        BinaryOperator::Lt => Operation::Less,
        BinaryOperator::Gt => Operation::Greater,
        BinaryOperator::LtEq => Operation::LessEqual,
        BinaryOperator::GtEq => Operation::GreaterEqual,
        _ => return None,
    })
}

pub fn validate_query_operations(
    catalog: &CatalogSnapshot,
    query: &Query,
    outer: &Scope,
) -> Result<(), SqlError> {
    comparison_plan(catalog, query, outer).map(|_| ())
}

/// Validate before changing the AST. Each planned comparison consumes each
/// operand once; postorder positions refer only to the unchanged input tree.
/// Operand declarations select Unicode units or Windows-1252 byte ordering.
pub fn lower_bin2_comparisons(
    catalog: &CatalogSnapshot,
    query: &mut Query,
    outer: &Scope,
) -> Result<(), SqlError> {
    let positions = comparison_plan(catalog, query, outer)?;
    struct Lower {
        position: usize,
        pending: std::iter::Peekable<std::vec::IntoIter<(usize, Bin2Input)>>,
    }
    fn operand(value: Expr) -> Expr {
        let value = match value {
            Expr::Nested(value) | Expr::Collate { expr: value, .. } => return operand(*value),
            value => value,
        };
        crate::expr::unary_function("__msduck_carrier_input", value)
    }
    impl VisitorMut for Lower {
        type Break = ();
        fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
            if self
                .pending
                .peek()
                .is_some_and(|(position, _)| *position == self.position)
            {
                let (_, mode) = self.pending.next().unwrap();
                let Expr::BinaryOp { left, op, right } =
                    std::mem::replace(expr, crate::expr::number(0))
                else {
                    unreachable!("planned comparison")
                };
                *expr = Expr::BinaryOp {
                    left: Box::new(crate::expr::binary_function(
                        match mode {
                            Bin2Input::Unicode => "__msduck_bin2_compare",
                            Bin2Input::Windows1252 => "__msduck_bin2_ansi_compare",
                        },
                        operand(*left),
                        operand(*right),
                    )),
                    op,
                    right: Box::new(crate::expr::number(0)),
                };
            }
            self.position += 1;
            ControlFlow::Continue(())
        }
    }
    let mut lower = Lower {
        position: 0,
        pending: positions.into_iter().peekable(),
    };
    let _ = VisitMut::visit(query, &mut lower);
    debug_assert!(lower.pending.next().is_none());
    Ok(())
}
