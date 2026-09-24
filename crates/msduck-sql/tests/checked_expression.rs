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

#[test]
fn scoped_operands_respect_qualified_names_ambiguity_and_shadowing() {
    use msduck_core::catalog::TypeMetadata;
    use msduck_sql::binding_scope::{Field, Scope, Source};
    use msduck_sql::checked_expression::plan_in_scope;
    fn source(qualifier: &str, system_type_id: Option<u8>) -> Source {
        Source {
            qualifiers: vec![qualifier.into()],
            fields: vec![Field {
                name: "n".into(),
                info: system_type_id.map(|id| TypeMetadata {
                    system_type_id: Some(id),
                    ..Default::default()
                }),
                properties: Default::default(),
                collation: None,
                json_fragment: false,
            }],
        }
    }
    let mut scope = Scope {
        rows: vec![Some(vec![source("outer", Some(127))])],
        ..Default::default()
    };
    scope.parameters.insert(
        "@d".into(),
        TypeMetadata {
            system_type_id: Some(56),
            ..Default::default()
        },
    );
    let expression = expr("[outer].[n]/@d");
    let bound = plan_in_scope(&expression, &scope).unwrap();
    assert_eq!(bound.kind, Kind::BigInt);
    assert_eq!(bound.query.to_string().matches("[outer].[n]").count(), 1);
    assert!(
        plan(
            &expression,
            &HashMap::from([("outer.n".into(), Kind::BigInt), ("@d".into(), Kind::Int)])
        )
        .is_none()
    );
    scope.rows.push(Some(vec![source("local", None)]));
    assert!(plan_in_scope(&expr("n/1"), &scope).is_none());
    assert!(plan_in_scope(&expression, &scope).is_some());
    scope.rows.push(None);
    assert!(plan_in_scope(&expression, &scope).is_none());
    // Variables retain their explicit declarations across unresolved row scopes.
    assert_eq!(
        plan_in_scope(&expr("@d/1"), &scope).unwrap().kind,
        Kind::Int
    );
    scope.rows = vec![Some(vec![source("a", Some(56)), source("b", Some(127))])];
    assert!(plan_in_scope(&expr("n/1"), &scope).is_none());
    assert_eq!(
        plan_in_scope(&expr("a.n/1"), &scope).unwrap().kind,
        Kind::Int
    );
    assert_eq!(
        plan_in_scope(&expr("b.n/1"), &scope).unwrap().kind,
        Kind::BigInt
    );
    scope.rows = vec![Some(vec![source("a", Some(106))])];
    assert!(plan_in_scope(&expr("a.n/1"), &scope).is_none());
}

#[test]
fn parser_marked_integer_casts_keep_checked_widening_and_reject_narrowing() {
    let declarations = HashMap::from([("@n".into(), Kind::BigInt)]);
    for (sql, expected) in [
        ("SELECT CAST(NULL AS BIGINT)+1", Some(Kind::BigInt)),
        ("SELECT CAST(2147483647+1 AS BIGINT)", Some(Kind::BigInt)),
        ("SELECT CAST(@n+1 AS INT)", None),
    ] {
        let mut statement = Parser::parse_sql(&msduck_sql::dialect::ServerDialect, sql)
            .unwrap()
            .remove(0);
        msduck_sql::variant_cast::mark(&mut statement);
        let Statement::Query(query) = statement else {
            panic!()
        };
        let SetExpr::Select(select) = *query.body else {
            panic!()
        };
        let SelectItem::UnnamedExpr(expr) = &select.projection[0] else {
            panic!()
        };
        let checked = plan(expr, &declarations);
        assert_eq!(checked.as_ref().map(|p| p.kind), expected, "{sql}");
        if let Some(checked) = checked {
            assert!(
                !checked
                    .query
                    .to_string()
                    .contains("__msduck_explicit_integer_source")
            );
        }
    }
}
