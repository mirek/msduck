use msduck_core::catalog::TypeMetadata;
use msduck_sql::{
    binding_scope::{Field, Scope, Source},
    checked_projection::plan,
};
use sqlparser::{ast::*, parser::Parser};
fn query(sql: &str) -> Box<Query> {
    let Statement::Query(query) = Parser::parse_sql(&msduck_sql::dialect::ServerDialect, sql)
        .unwrap()
        .remove(0)
    else {
        panic!("query")
    };
    query
}
fn scope() -> Scope {
    Scope {
        rows: vec![Some(vec![Source {
            qualifiers: vec!["s".into()],
            fields: vec![Field {
                name: "n".into(),
                info: Some(TypeMetadata {
                    system_type_id: Some(127),
                    ..Default::default()
                }),
                properties: Default::default(),
                collation: None,
                json_fragment: false,
            }],
        }])],
        ..Default::default()
    }
}
#[test]
fn projects_values_then_diagnostics_with_quoted_aliases_and_original_source() {
    let q = query("SELECT s.n/2 AS [a b],s.n+1 AS [a b],7 FROM t AS s WHERE s.n>0");
    let p = plan(&q, &scope()).unwrap();
    assert_eq!(p.width, 3);
    let SetExpr::Select(select) = p.query.body.as_ref() else {
        panic!()
    };
    assert_eq!(select.projection.len(), 15);
    assert_eq!(select.from.len(), 4);
    assert!(p.query.to_string().contains("AS [a b]"));
    assert!(p.query.to_string().contains("WHERE s.n > 0"));
    assert_eq!(
        q.to_string(),
        "SELECT s.n / 2 AS [a b], s.n + 1 AS [a b], 7 FROM t AS s WHERE s.n > 0"
    );
}
#[test]
fn generated_aliases_do_not_capture_user_names() {
    let q = query("SELECT 1/0 AS __msduck_checked_projection_0_x");
    let p = plan(&q, &Scope::default()).unwrap();
    assert!(
        p.query
            .to_string()
            .contains("__msduck_checked_projection_1_0.__msduck_checked_projection_1_0_value")
    );
}

#[test]
fn ordering_uses_safe_columns_and_scalar_prechecks_bind_parameters_without_value_dependent_plans() {
    let q = query("SELECT s.n,s.n/0 AS fault FROM t s ORDER BY s.n DESC");
    assert!(plan(&q, &scope()).is_some());
    assert!(
        plan(
            &query("SELECT s.n/0 AS fault FROM t s ORDER BY fault"),
            &scope()
        )
        .is_none()
    );
    let p = plan(
        &query("SELECT 1/0,CAST(NULL AS BIGINT)+1 FROM t s WHERE s.n<0"),
        &scope(),
    )
    .unwrap();
    assert_eq!(p.scalar_checks.len(), 2);
    let mut scope = scope();
    scope.parameters.insert(
        "@d".into(),
        TypeMetadata {
            system_type_id: Some(56),
            ..Default::default()
        },
    );
    let p = plan(
        &query("SELECT 1/@d,2147483647+1 FROM t s WHERE s.n<0"),
        &scope,
    )
    .unwrap();
    assert_eq!(p.scalar_checks.len(), 2);
}
#[test]
fn unsupported_row_shapes_and_operand_trees_fall_back_whole() {
    for sql in [
        "SELECT 1",
        "SELECT 1/0,ABS(2)",
        "SELECT DISTINCT 1/0",
        "SELECT TOP(1) 1/0",
        "SELECT 1/0 ORDER BY 1",
        "SELECT 1/0 UNION SELECT 1",
        "SELECT n/2 FROM t",
        "SELECT 1/0, * FROM t",
        "SELECT SUM(n)/2 FROM t",
        "SELECT 1/0 FROM t GROUP BY n",
    ] {
        assert!(plan(&query(sql), &Scope::default()).is_none(), "{sql}");
    }
}
