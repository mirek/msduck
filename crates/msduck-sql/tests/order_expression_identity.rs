use msduck_core::catalog::TypeMetadata;
use msduck_sql::{
    binding_scope::{Field, Scope, Source},
    projection::order::expression_identity,
};
use sqlparser::{
    ast::{Expr, SelectItem, SetExpr, Statement},
    parser::Parser,
};

fn expr(sql: &str) -> Expr {
    let Statement::Query(query) = Parser::parse_sql(
        &msduck_sql::dialect::ServerDialect,
        &format!("SELECT {sql}"),
    )
    .unwrap()
    .remove(0) else {
        panic!()
    };
    let SetExpr::Select(select) = *query.body else {
        panic!()
    };
    let SelectItem::UnnamedExpr(expr) = select.projection.into_iter().next().unwrap() else {
        panic!()
    };
    expr
}
fn info() -> TypeMetadata {
    TypeMetadata {
        system_type_id: Some(56),
        user_type_id: Some(56),
        ..Default::default()
    }
}
fn source(qualifier: &str) -> Source {
    Source {
        qualifiers: vec![qualifier.into()],
        fields: ["a", "b"]
            .map(|name| Field {
                name: name.into(),
                info: Some(info()),
                collation: None,
                json_fragment: false,
                properties: Default::default(),
            })
            .into(),
    }
}
fn scope() -> Scope {
    let mut scope = Scope::default();
    scope.parameters.insert("@p".into(), info());
    scope.parameters.insert("@q".into(), info());
    scope
}
#[test]
fn conditional_identity_uses_bound_names_without_mutating_inputs() {
    let sources = [source("h")];
    let scope = scope();
    for (a, b, expected) in [
        ("COALESCE(h.a,@P)", "coalesce((a),(@p))", true),
        (
            "CASE WHEN h.a>@P THEN h.a ELSE h.b END",
            "CASE WHEN (a)>@p THEN a ELSE b END",
            true,
        ),
        ("ISNULL(a,@p)", "isnull(h.a,@P)", true),
        ("COALESCE(a,@p)", "COALESCE(b,@p)", false),
        ("COALESCE(a,@p)", "COALESCE(a,@q)", false),
        ("a+01", "h.a+1", true),
        ("a+1", "1+a", false),
    ] {
        let a = expr(a);
        let b = expr(b);
        let before = (a.clone(), b.clone());
        assert_eq!(
            expression_identity(&a, &b, &sources, &scope),
            Some(expected)
        );
        assert_eq!((a, b), before);
    }
}
#[test]
fn unknown_ambiguous_opaque_and_nested_queries_do_not_prove_identity() {
    let sources = [source("h")];
    let scope = scope();
    for sql in [
        "missing",
        "@missing",
        "COALESCE(a,@missing)",
        "RAND()",
        "NEWID()",
        "dbo.opaque(a)",
        "(SELECT a)",
        "CAST(a AS dbo.alias_type)",
        "COALESCE(a) OVER()",
    ] {
        let e = expr(sql);
        assert_eq!(expression_identity(&e, &e, &sources, &scope), None, "{sql}");
    }
    let e = expr("a");
    assert_eq!(
        expression_identity(&e, &e, &[source("h"), source("j")], &scope),
        None
    );
    let mut unknown = source("h");
    unknown.fields[0].info = None;
    assert_eq!(expression_identity(&e, &e, &[unknown], &scope), None);
    let mut alias = source("h");
    alias.fields[0].info.as_mut().unwrap().user_type_id = Some(500);
    assert_eq!(expression_identity(&e, &e, &[alias], &scope), None);
}
#[test]
fn explicit_scope_shadowing_and_distinct_sources_are_preserved() {
    let sources = [source("h"), source("j")];
    let mut scope = scope();
    assert_eq!(
        expression_identity(&expr("h.a"), &expr("j.a"), &sources, &scope),
        Some(false)
    );
    scope.rows.push(Some(vec![source("outer")]));
    assert_eq!(
        expression_identity(&expr("h.a"), &expr("outer.a"), &sources, &scope),
        Some(false)
    );
    scope.rows.push(None);
    assert_eq!(
        expression_identity(&expr("outer.a"), &expr("outer.a"), &sources, &scope),
        None
    );
    assert_eq!(
        expression_identity(&expr("h.a"), &expr("h.a"), &sources, &scope),
        Some(true)
    );
}

#[test]
fn variant_conversion_identity_preserves_casts_and_unknown_barriers() {
    let sources = [source("h")];
    let scope = scope();
    for (a, b, expected) in [
        (
            "COALESCE(h.a,CAST(@P AS SQL_VARIANT))",
            "coalesce((a),CAST(@p AS sql_variant))",
            true,
        ),
        (
            "COALESCE(h.a,CAST(@p AS SQL_VARIANT))",
            "COALESCE(a,@p)",
            false,
        ),
        (
            "COALESCE(h.a,CAST(@p AS SQL_VARIANT))",
            "COALESCE(b,CAST(@p AS SQL_VARIANT))",
            false,
        ),
    ] {
        let a = expr(a);
        let b = expr(b);
        let original = (a.clone(), b.clone());
        assert_eq!(
            expression_identity(&a, &b, &sources, &scope),
            Some(expected)
        );
        assert_eq!((a, b), original);
    }
    for sql in [
        "CAST(RAND() AS SQL_VARIANT)",
        "CAST(unknown_function() AS SQL_VARIANT)",
        "CAST((SELECT 1) AS SQL_VARIANT)",
    ] {
        let e = expr(sql);
        assert_eq!(expression_identity(&e, &e, &sources, &scope), None, "{sql}");
    }
    let e = expr("CAST(h.a AS SQL_VARIANT)");
    let mut alias = source("h");
    alias.fields[0].info.as_mut().unwrap().user_type_id = Some(500);
    assert_eq!(expression_identity(&e, &e, &[alias], &scope), None);
}
