//! AST construction with explicit arguments; no execution or session access.
use sqlparser::{ast::*, parser::Parser};

pub fn unary_function(name: &str, argument: Expr) -> Expr {
    let mut function = binary_function(name, argument, number(0));
    if let Expr::Function(f) = &mut function
        && let FunctionArguments::List(args) = &mut f.args
    {
        args.args.pop();
    }
    function
}
pub fn binary_function(name: &str, left: Expr, right: Expr) -> Expr {
    let mut parser = Parser::new(&sqlparser::dialect::GenericDialect {})
        .try_with_sql("f(NULL, NULL)")
        .expect("static function syntax");
    let Expr::Function(mut function) = parser.parse_expr().expect("static function syntax") else {
        unreachable!()
    };
    function.name = ObjectName::from(vec![Ident::new(name)]);
    if let FunctionArguments::List(args) = &mut function.args {
        args.args = vec![
            FunctionArg::Unnamed(FunctionArgExpr::Expr(left)),
            FunctionArg::Unnamed(FunctionArgExpr::Expr(right)),
        ];
    }
    Expr::Function(function)
}
pub fn number(n: impl ToString) -> Expr {
    Expr::Value(sqlparser::ast::Value::Number(n.to_string(), false).into())
}

/// SQL Server unary plus is an identity operation, including nonnumeric inputs.
/// Move the operand rather than duplicating it, preserving single evaluation.
pub fn lower_unary_plus(expr: &mut Expr) {
    while matches!(
        expr,
        Expr::UnaryOp {
            op: UnaryOperator::Plus,
            ..
        }
    ) {
        let Expr::UnaryOp { expr: operand, .. } =
            std::mem::replace(expr, Expr::Value(Value::Null.into()))
        else {
            unreachable!()
        };
        *expr = *operand;
    }
}

#[cfg(test)]
mod unary_plus_tests {
    use super::*;
    #[test]
    fn identity_lowering_moves_one_operand_and_is_idempotent() {
        for sql in [
            "NEWID()",
            "N'abc'",
            "CAST(1 AS BIT)",
            "CAST('2024-01-01' AS DATE)",
        ] {
            let original = Parser::new(&crate::dialect::ServerDialect)
                .try_with_sql(sql)
                .unwrap()
                .parse_expr()
                .unwrap();
            let mut expr = Expr::UnaryOp {
                op: UnaryOperator::Plus,
                expr: Box::new(Expr::UnaryOp {
                    op: UnaryOperator::Plus,
                    expr: Box::new(original.clone()),
                }),
            };
            lower_unary_plus(&mut expr);
            assert_eq!(expr, original);
            lower_unary_plus(&mut expr);
            assert_eq!(expr, original);
        }
    }
}
