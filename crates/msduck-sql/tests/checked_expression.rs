use msduck_sql::checked_expression::{Kind, plan};
use sqlparser::{ast::*, parser::Parser};
use std::collections::HashMap;
fn expr(sql: &str) -> Expr {
    Parser::new(&msduck_sql::dialect::ServerDialect)
        .try_with_sql(sql)
        .unwrap()
        .parse_expr()
        .unwrap()
}
#[test]
fn nested_plans_preserve_types_and_materialize_each_operand() {
    let declarations = HashMap::from([("@n".into(), Kind::BigInt), ("@d".into(), Kind::Int)]);
    let p = plan(&expr("((@n+1)/@d)%2>=0"), &declarations).unwrap();
    assert_eq!(p.kind, Kind::Boolean);
    let text = p.query.to_string();
    assert_eq!(text.matches("@n").count(), 1);
    assert_eq!(text.matches("@d").count(), 1);
    assert!(text.contains("__msduck_checked_add"));
    assert!(text.contains("__msduck_checked_divide"));
    assert!(text.contains("__msduck_checked_modulo"));
    assert!(
        p.query
            .with
            .as_ref()
            .unwrap()
            .cte_tables
            .iter()
            .all(|cte| cte.materialized.is_some())
    );
    assert_eq!(
        plan(&expr("CAST(7+1 AS BIGINT)"), &declarations)
            .unwrap()
            .kind,
        Kind::BigInt
    );
}
#[test]
fn unsupported_or_unchecked_expressions_are_not_partially_rewritten() {
    let declarations = HashMap::from([("@n".into(), Kind::BigInt)]);
    for sql in [
        "1",
        "@n",
        "CASE WHEN 1=0 THEN 1/0 ELSE 7 END",
        "1/0=0 AND 1=0",
        "CAST(@n+1 AS INT)",
        "(SELECT 1/0)",
        "'1'+1",
        "1.5/0",
        "ABS(1/0)",
    ] {
        assert!(plan(&expr(sql), &declarations).is_none(), "{sql}");
    }
}
#[test]
fn null_predicates_and_unary_negation_keep_fault_outcomes() {
    let p = plan(&expr("NOT ((-(1/0)) IS NULL)"), &HashMap::new()).unwrap();
    assert_eq!(p.kind, Kind::Boolean);
    let sql = p.query.to_string();
    assert!(sql.contains("__msduck_checked_subtract"));
    assert!(sql.contains("error_message"));
}

#[test]
fn planning_limits_reject_large_or_deep_trees_without_partial_plans() {
    fn tree(depth: usize) -> Expr {
        if depth == 0 {
            return msduck_sql::expr::number(1);
        }
        Expr::BinaryOp {
            left: Box::new(tree(depth - 1)),
            op: BinaryOperator::Plus,
            right: Box::new(tree(depth - 1)),
        }
    }
    assert!(plan(&tree(8), &HashMap::new()).is_none());
    let mut deep = expr("1/0");
    for _ in 0..66 {
        deep = Expr::Nested(Box::new(deep));
    }
    assert!(plan(&deep, &HashMap::new()).is_none());
}
