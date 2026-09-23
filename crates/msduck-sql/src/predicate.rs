//! SQL Server search conditions are predicates, not scalar BIT values.
use anyhow::Result;
use sqlparser::ast::*;
use std::ops::ControlFlow;

const ERROR: &str =
    "An expression of non-boolean type specified in a context where a condition is expected";

pub use crate::expression_metadata::conditional::iif_args;

pub use crate::expression_metadata::conditional::null_constant;

fn comparison(op: &BinaryOperator) -> bool {
    matches!(
        op,
        BinaryOperator::Eq
            | BinaryOperator::NotEq
            | BinaryOperator::Lt
            | BinaryOperator::LtEq
            | BinaryOperator::Gt
            | BinaryOperator::GtEq
    )
}

fn condition(expr: &Expr) -> ControlFlow<String> {
    let valid = match expr {
        Expr::Nested(inner)
        | Expr::UnaryOp {
            op: UnaryOperator::Not,
            expr: inner,
        } => return condition(inner),
        Expr::BinaryOp {
            left,
            op: BinaryOperator::And | BinaryOperator::Or,
            right,
        } => {
            condition(left)?;
            return condition(right);
        }
        Expr::BinaryOp { op, .. }
        | Expr::AnyOp { compare_op: op, .. }
        | Expr::AllOp { compare_op: op, .. } => comparison(op),
        Expr::IsNull(_)
        | Expr::IsNotNull(_)
        | Expr::IsDistinctFrom(..)
        | Expr::IsNotDistinctFrom(..)
        | Expr::InList { .. }
        | Expr::InSubquery { .. }
        | Expr::Between { .. }
        | Expr::Like { .. }
        | Expr::Exists { .. } => true,
        _ => false,
    };
    if valid {
        ControlFlow::Continue(())
    } else {
        ControlFlow::Break(ERROR.into())
    }
}

fn optional(expr: &Option<Expr>) -> ControlFlow<String> {
    if let Some(expr) = expr {
        condition(expr)?;
    }
    ControlFlow::Continue(())
}

fn joins(table: &TableWithJoins) -> ControlFlow<String> {
    for join in &table.joins {
        let constraint = match &join.join_operator {
            JoinOperator::Join(c)
            | JoinOperator::Inner(c)
            | JoinOperator::Left(c)
            | JoinOperator::LeftOuter(c)
            | JoinOperator::Right(c)
            | JoinOperator::RightOuter(c)
            | JoinOperator::FullOuter(c)
            | JoinOperator::CrossJoin(c) => Some(c),
            _ => None,
        };
        if let Some(JoinConstraint::On(expr)) = constraint {
            condition(expr)?;
        }
    }
    ControlFlow::Continue(())
}

pub fn validate(statement: &Statement) -> Result<()> {
    struct Predicates {
        case_depth: usize,
    }
    impl Visitor for Predicates {
        type Break = String;
        fn pre_visit_statement(&mut self, statement: &Statement) -> ControlFlow<String> {
            match statement {
                Statement::CreateTable(table) => {
                    for column in &table.columns {
                        for option in &column.options {
                            if let ColumnOption::Check(check) = &option.option {
                                condition(&check.expr)?;
                            }
                        }
                    }
                    for constraint in &table.constraints {
                        if let TableConstraint::Check(check) = constraint {
                            condition(&check.expr)?;
                        }
                    }
                }
                Statement::If(s) => optional(&s.if_block.condition)?,
                Statement::While(s) => optional(&s.while_block.condition)?,
                Statement::Update(s) => {
                    optional(&s.selection)?;
                    joins(&s.table)?;
                    if let Some(
                        UpdateTableFromKind::AfterSet(sources)
                        | UpdateTableFromKind::BeforeSet(sources),
                    ) = &s.from
                    {
                        for source in sources {
                            joins(source)?;
                        }
                    }
                }
                Statement::Delete(s) => {
                    optional(&s.selection)?;
                    let (FromTable::WithFromKeyword(sources) | FromTable::WithoutKeyword(sources)) =
                        &s.from;
                    for source in sources {
                        joins(source)?;
                    }
                    if let Some(sources) = &s.using {
                        for source in sources {
                            joins(source)?;
                        }
                    }
                }
                _ => {}
            }
            ControlFlow::Continue(())
        }
        fn pre_visit_select(&mut self, select: &Select) -> ControlFlow<String> {
            optional(&select.selection)?;
            optional(&select.having)?;
            for table in &select.from {
                joins(table)?;
            }
            ControlFlow::Continue(())
        }
        fn pre_visit_table_factor(&mut self, table: &TableFactor) -> ControlFlow<String> {
            if let TableFactor::NestedJoin {
                table_with_joins, ..
            } = table
            {
                joins(table_with_joins)?;
            }
            ControlFlow::Continue(())
        }
        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<String> {
            if let Expr::Function(function) = expr {
                if let Err(error) = crate::nullif::args(function) {
                    return ControlFlow::Break(error);
                }
                if let Err(error) = crate::expression_metadata::conditional::isnull_args(function) {
                    return ControlFlow::Break(error);
                }
                match crate::expression_metadata::conditional::coalesce_args(function) {
                    Ok(Some(args)) if args.iter().all(|expr| null_constant(expr)) => return ControlFlow::Break("At least one of the arguments to COALESCE must be an expression that is not the NULL constant.".into()),
                    Err(error) => return ControlFlow::Break(error),
                    _ => {}
                }
            }
            if let Expr::Function(function) = expr
                && let Err(error) = crate::choose::args(function)
            {
                return ControlFlow::Break(error);
            }
            let iif = if let Expr::Function(function) = expr {
                match iif_args(function) {
                    Ok(args) => args,
                    Err(error) => return ControlFlow::Break(error),
                }
            } else {
                None
            };
            if matches!(expr, Expr::Case { .. }) || iif.is_some() {
                self.case_depth += 1;
                if self.case_depth > 10 {
                    return ControlFlow::Break(
                        "Case expressions may only be nested to level 10.".into(),
                    );
                }
            }
            if let Some([test, yes, no]) = iif {
                condition(test)?;
                if null_constant(yes) && null_constant(no) {
                    return ControlFlow::Break("At least one of the result expressions in a CASE specification must be an expression other than the NULL constant.".into());
                }
            }
            if let Expr::Case {
                conditions,
                else_result,
                ..
            } = expr
                && conditions
                    .iter()
                    .all(|branch| null_constant(&branch.result))
                && else_result
                    .as_ref()
                    .is_none_or(|value| null_constant(value))
            {
                return ControlFlow::Break("At least one of the result expressions in a CASE specification must be an expression other than the NULL constant.".into());
            }
            if let Expr::Case {
                operand: None,
                conditions,
                ..
            } = expr
            {
                for branch in conditions {
                    condition(&branch.condition)?;
                }
            }
            ControlFlow::Continue(())
        }
        fn post_visit_expr(&mut self, expr: &Expr) -> ControlFlow<String> {
            if matches!(expr, Expr::Case { .. })
                || matches!(expr, Expr::Function(f) if f.name.to_string().eq_ignore_ascii_case("IIF"))
            {
                self.case_depth -= 1;
            }
            ControlFlow::Continue(())
        }
    }
    if let ControlFlow::Break(error) = statement.visit(&mut Predicates { case_depth: 0 }) {
        anyhow::bail!(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::ServerDialect;
    use sqlparser::parser::Parser;

    fn parse(sql: &str) -> Statement {
        Parser::parse_sql(&ServerDialect, sql).unwrap().remove(0)
    }

    #[test]
    fn validation_preserves_input_and_rejects_scalar_conditions() {
        for sql in [
            "SELECT 1 WHERE CAST(1 AS BIT)",
            "SELECT 1 WHERE 1=1 OR 0",
            "SELECT 1 FROM a JOIN b ON a.flag",
            "SELECT COUNT(*) FROM a HAVING 1",
            "UPDATE a SET n=1 WHERE flag",
            "DELETE FROM a WHERE flag",
            "SELECT CASE WHEN flag THEN 1 ELSE 2 END FROM a",
            "SELECT IIF(flag,1,2) FROM a",
            "CREATE TABLE a(n INT CHECK(n))",
        ] {
            let statement = parse(sql);
            let original = statement.clone();
            assert_eq!(
                validate(&statement).unwrap_err().to_string(),
                ERROR,
                "{sql}"
            );
            assert_eq!(statement, original);
        }
        for sql in [
            "SELECT 1 WHERE n=1 OR (n IS NULL AND NOT m<2)",
            "SELECT CASE flag WHEN 1 THEN 2 ELSE 3 END FROM a",
            "SELECT IIF(EXISTS(SELECT 1 FROM a WHERE n>0),1,2)",
            "SELECT CASE WHEN n>0 THEN CAST(NULL AS INT) ELSE NULL END FROM a",
            "SELECT COALESCE(CAST(NULL AS INT),NULL)",
        ] {
            let statement = parse(sql);
            let original = statement.clone();
            validate(&statement).unwrap();
            assert_eq!(statement, original);
        }
    }

    #[test]
    fn nested_case_depth_and_diagnostic_order_are_repeatable() {
        let nested = |depth: usize| {
            (0..depth).fold("1".to_string(), |value, _| format!("IIF(1=1,{value},0)"))
        };
        validate(&parse(&format!("SELECT {}", nested(10)))).unwrap();
        let too_deep = parse(&format!("SELECT {}", nested(11)));
        for _ in 0..3 {
            assert_eq!(
                validate(&too_deep).unwrap_err().to_string(),
                "Case expressions may only be nested to level 10."
            );
        }
        // The search condition is checked before invalid projection arguments.
        let statement = parse("SELECT NULLIF(1) WHERE 1");
        assert_eq!(validate(&statement).unwrap_err().to_string(), ERROR);
        let all_null = parse("SELECT COALESCE(NULL,NULL)");
        assert_eq!(
            validate(&all_null).unwrap_err().to_string(),
            "At least one of the arguments to COALESCE must be an expression that is not the NULL constant."
        );
    }
}
