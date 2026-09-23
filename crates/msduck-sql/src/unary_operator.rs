//! Unary-operator validation over explicit declarations, without evaluation.
use crate::{expression_metadata::storage, parameter::Parameter};
use msduck_core::diagnostic::SqlError;
use sqlparser::ast::*;
use std::{collections::HashMap, ops::ControlFlow};

pub const BIT_MINUS: &str = "Operand data type bit is invalid for minus operator.";

pub fn check(
    expr: &Expr,
    parameters: &HashMap<String, Parameter>,
    column: &impl Fn(&Expr) -> Option<DataType>,
) -> Result<(), SqlError> {
    let Expr::UnaryOp {
        op: UnaryOperator::Minus,
        expr: operand,
    } = expr
    else {
        return Ok(());
    };
    let mut operand = operand.as_ref();
    while let Expr::Nested(inner)
    | Expr::UnaryOp {
        op: UnaryOperator::Plus,
        expr: inner,
    } = operand
    {
        operand = inner;
    }
    if matches!(
        storage::kind(operand, parameters, column),
        Some(DataType::Bit(_) | DataType::Boolean)
    ) || crate::case_types::is_bit(operand, parameters)
    {
        return Err(SqlError::new(8117, 1, BIT_MINUS));
    }
    Ok(())
}

pub fn validate<T: Visit>(
    node: &T,
    parameters: &HashMap<String, Parameter>,
) -> Result<(), SqlError> {
    struct Check<'a>(&'a HashMap<String, Parameter>);
    impl Visitor for Check<'_> {
        type Break = SqlError;
        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<SqlError> {
            match check(expr, self.0, &|_| None) {
                Ok(()) => ControlFlow::Continue(()),
                Err(error) => ControlFlow::Break(error),
            }
        }
    }
    match node.visit(&mut Check(parameters)) {
        ControlFlow::Continue(()) => Ok(()),
        ControlFlow::Break(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_known_bit_operands_without_evaluating_unreachable_branches() {
        for sql in [
            "IF 1=0 SELECT -CAST(1 AS BIT)",
            "BEGIN TRY SELECT -CAST(1 AS BIT) END TRY BEGIN CATCH SELECT 1 END CATCH",
            "DECLARE @b BIT=1; SELECT -@b",
            "SELECT -CONVERT(BIT,1)",
            "SELECT -(+CAST(1 AS BIT))",
            "SELECT -COALESCE(CAST(NULL AS BIT),CAST(1 AS BIT))",
            "SELECT -ISNULL(CAST(NULL AS BIT),1)",
        ] {
            let statements = crate::batch::parse(sql).unwrap();
            let before = statements.clone();
            let error = crate::preflight::variables(&statements, &HashMap::new()).unwrap_err();
            let error = error.downcast_ref::<SqlError>().unwrap();
            assert_eq!(
                (
                    error.number,
                    error.state,
                    error.severity,
                    error.message.as_str()
                ),
                (8117, 1, 16, BIT_MINUS),
                "{sql}"
            );
            assert_eq!(statements, before);
        }
        let statements = crate::batch::parse("SELECT +CAST(1 AS BIT),-CAST(1 AS INT)").unwrap();
        crate::preflight::variables(&statements, &HashMap::new()).unwrap();
    }
}
