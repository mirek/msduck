//! Reject same-query window expressions outside SELECT and ORDER BY.
use sqlparser::ast::*;
use std::ops::ControlFlow;

pub const ERROR: &str = "Windowed functions can only appear in the SELECT or ORDER BY clauses.";

fn reject<T: Visit>(value: &T) -> ControlFlow<String> {
    struct Windows {
        queries: usize,
    }
    impl Visitor for Windows {
        type Break = String;
        fn pre_visit_query(&mut self, _: &Query) -> ControlFlow<String> {
            self.queries += 1;
            ControlFlow::Continue(())
        }
        fn post_visit_query(&mut self, _: &Query) -> ControlFlow<String> {
            self.queries -= 1;
            ControlFlow::Continue(())
        }
        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<String> {
            if self.queries == 0 && matches!(expr, Expr::Function(f) if f.over.is_some()) {
                ControlFlow::Break(ERROR.into())
            } else {
                ControlFlow::Continue(())
            }
        }
    }
    value.visit(&mut Windows { queries: 0 })
}

pub fn validate(statement: &Statement) -> Result<(), String> {
    struct Placement;
    impl Visitor for Placement {
        type Break = String;
        fn pre_visit_select(&mut self, select: &Select) -> ControlFlow<String> {
            reject(&select.selection)?;
            reject(&select.having)?;
            reject(&select.group_by)?;
            reject(&select.from)?;
            reject(&select.top)?;
            ControlFlow::Continue(())
        }
        fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<String> {
            reject(&query.limit_clause)?;
            reject(&query.fetch)?;
            if let SetExpr::Values(values) = query.body.as_ref() {
                reject(values)?;
            }
            ControlFlow::Continue(())
        }
        fn pre_visit_statement(&mut self, statement: &Statement) -> ControlFlow<String> {
            match statement {
                Statement::Update(update) => {
                    reject(&update.assignments)?;
                    reject(&update.selection)?;
                    reject(&update.table)?;
                    reject(&update.from)?;
                }
                Statement::Delete(delete) => {
                    reject(&delete.selection)?;
                    reject(&delete.from)?;
                    reject(&delete.using)?;
                }
                Statement::CreateTable(table) => {
                    reject(&table.columns)?;
                    reject(&table.constraints)?;
                }
                _ => {}
            }
            ControlFlow::Continue(())
        }
    }
    match statement.visit(&mut Placement) {
        ControlFlow::Continue(()) => Ok(()),
        ControlFlow::Break(error) => Err(error),
    }
}
