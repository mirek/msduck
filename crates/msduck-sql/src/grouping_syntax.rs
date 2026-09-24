//! Reject aggregate calls and subqueries before grouping expressions are lowered.
use sqlparser::ast::*;
use std::ops::ControlFlow;
pub const ERROR: &str = "Cannot use an aggregate or a subquery in an expression used for the group by list of a GROUP BY clause.";

pub fn validate(statement: &Statement) -> Result<(), String> {
    struct Expression;
    impl Visitor for Expression {
        type Break = String;
        fn pre_visit_query(&mut self, _: &Query) -> ControlFlow<String> {
            ControlFlow::Break(ERROR.into())
        }
        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<String> {
            if let Expr::Function(f) = expr
                && f.over.is_none()
                && let [ObjectNamePart::Identifier(name)] = f.name.0.as_slice()
                && matches!(
                    name.value.to_ascii_lowercase().as_str(),
                    "sum"
                        | "avg"
                        | "min"
                        | "max"
                        | "count"
                        | "count_big"
                        | "stdev"
                        | "stdevp"
                        | "var"
                        | "varp"
                        | "string_agg"
                        | "checksum_agg"
                        | "approx_count_distinct"
                )
            {
                return ControlFlow::Break(ERROR.into());
            }
            ControlFlow::Continue(())
        }
    }
    struct Selects;
    impl Visitor for Selects {
        type Break = String;
        fn pre_visit_select(&mut self, select: &Select) -> ControlFlow<String> {
            select.group_by.visit(&mut Expression)
        }
    }
    match statement.visit(&mut Selects) {
        ControlFlow::Continue(()) => Ok(()),
        ControlFlow::Break(error) => Err(error),
    }
}

pub const CONSTANT_ERROR: &str =
    "Each GROUP BY expression must contain at least one column that is not an outer reference.";

// Run after grouping constructs have been normalized. The scope resolver
// excludes proven outer references; unresolved identifiers remain for binding.
pub fn columns(select: &Select, possible_local: &dyn Fn(&Expr) -> bool) -> Result<(), String> {
    struct References<'a>(&'a dyn Fn(&Expr) -> bool);
    impl VisitorMut for References<'_> {
        type Break = ();
        fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
            if let Expr::Function(f) = expr
                && let [ObjectNamePart::Identifier(name)] = f.name.0.as_slice()
                && matches!(
                    name.value.to_ascii_lowercase().as_str(),
                    "datepart" | "datename" | "dateadd" | "datediff" | "datediff_big"
                )
                && let FunctionArguments::List(args) = &mut f.args
                && let Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(value))) =
                    args.args.first_mut()
                && matches!(value, Expr::Identifier(_))
            {
                *value = Expr::Value(Value::Null.into());
            }
            if (matches!(expr, Expr::Identifier(id) if !id.value.starts_with('@'))
                || matches!(expr, Expr::CompoundIdentifier(_)))
                && (self.0)(expr)
            {
                return ControlFlow::Break(());
            }
            ControlFlow::Continue(())
        }
    }
    fn check(expr: &Expr, possible_local: &dyn Fn(&Expr) -> bool) -> Result<(), String> {
        match expr {
            Expr::GroupingSets(groups) | Expr::Cube(groups) | Expr::Rollup(groups) => {
                for expr in groups.iter().flatten() {
                    check(expr, possible_local)?;
                }
            }
            Expr::Tuple(values) => {
                for expr in values {
                    check(expr, possible_local)?;
                }
            }
            Expr::Nested(value) => return check(value, possible_local),
            _ => {
                if VisitMut::visit(&mut expr.clone(), &mut References(possible_local)).is_continue()
                {
                    return Err(CONSTANT_ERROR.into());
                }
            }
        }
        Ok(())
    }
    if let GroupByExpr::Expressions(values, _) = &select.group_by {
        for value in values {
            check(value, possible_local)?;
        }
    }
    Ok(())
}
