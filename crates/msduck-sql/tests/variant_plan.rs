use msduck_sql::{
    aggregate, dialect::ServerDialect, variant_compare, variant_order, variant_pack,
    variant_results, variant_sets::Equality,
};
use sqlparser::{ast::*, parser::Parser};

fn query(sql: &str) -> Box<Query> {
    let Statement::Query(query) = Parser::parse_sql(&ServerDialect, sql).unwrap().remove(0) else {
        panic!("query")
    };
    query
}
fn expression(sql: &str) -> Expr {
    let SetExpr::Select(select) = *query(&format!("SELECT {sql}")).body else {
        panic!("select")
    };
    let SelectItem::UnnamedExpr(value) = select.projection.into_iter().next().unwrap() else {
        panic!("expression")
    };
    value
}

#[test]
fn storage_identity_and_equality_keys_remain_explicit() {
    let storage = variant_pack::storage_type();
    assert_eq!(
        variant_pack::storage_kind(&storage.to_string()),
        Some(storage.clone())
    );
    assert_eq!(
        variant_pack::storage_kind("STRUCT(tag UTINYINT,value BIGINT)"),
        None
    );
    let DataType::Struct(mut fields, bracket) = storage else {
        panic!("struct")
    };
    fields[1].field_type = DataType::Int(None);
    assert!(!variant_pack::is_storage(&DataType::Struct(
        fields, bracket
    )));
    let source = expression("producer()");
    assert!(!variant_compare::known(&source));
    let logical = variant_compare::kind();
    assert!(variant_pack::is_variant(&logical));
    assert!(Equality::for_type(Some(&logical)).special());
    assert!(!Equality::for_type(None).special());
    assert_eq!(
        Equality::for_type(Some(&DataType::Int(None))).key(source.clone()),
        source
    );
    let packed = variant_pack::convert(source.clone());
    assert_eq!(variant_pack::convert(packed.clone()), packed);
    let key = variant_compare::key(packed);
    assert_eq!(key.to_string().matches("producer()").count(), 1);
}

#[test]
fn conditional_and_order_plans_keep_producers_and_payload_projection() {
    for sql in [
        "CASE WHEN flag=1 THEN CAST(producer() AS SQL_VARIANT) ELSE fallback() END",
        "COALESCE(CAST(producer() AS SQL_VARIANT),fallback())",
        "IIF(flag=1,CAST(producer() AS SQL_VARIANT),fallback())",
        "CHOOSE(indexer(),CAST(producer() AS SQL_VARIANT),fallback())",
    ] {
        let mut value = expression(sql);
        assert!(variant_results::known(&value));
        let original = value.to_string();
        variant_results::lower(&mut value);
        for name in ["producer()", "fallback()", "indexer()"] {
            assert_eq!(
                value.to_string().matches(name).count(),
                original.matches(name).count(),
                "{sql}: {name}"
            );
        }
    }
    let mut source = query(
        "SELECT TOP (2) CAST(producer() AS SQL_VARIANT) AS payload FROM t ORDER BY payload DESC",
    );
    let original_projection = match source.body.as_ref() {
        SetExpr::Select(s) => s.projection.clone(),
        _ => panic!("select"),
    };
    variant_order::wrap(&mut source, &["payload".into()], &[(0, true)], vec![]);
    let SetExpr::Select(outer) = source.body.as_ref() else {
        panic!("outer select")
    };
    assert!(outer.top.is_some());
    let TableFactor::Derived { subquery, .. } = &outer.from[0].relation else {
        panic!("derived")
    };
    let SetExpr::Select(inner) = subquery.body.as_ref() else {
        panic!("inner select")
    };
    assert!(inner.top.is_none());
    assert_eq!(inner.projection, original_projection);
    assert_eq!(source.to_string().matches("producer()").count(), 1);
}

#[test]
fn aggregate_plans_keep_input_count_and_reject_unsupported_variant_arithmetic() {
    for sql in [
        "MIN(CAST(producer() AS SQL_VARIANT))",
        "MAX(CAST(producer() AS SQL_VARIANT))",
        "COUNT(DISTINCT CAST(producer() AS SQL_VARIANT))",
        "STDEV(DISTINCT CAST(producer() AS BIGINT))",
        "SUM(CAST(producer() AS SMALLINT))",
    ] {
        let mut value = expression(sql);
        aggregate::mark(&mut value, &Default::default()).unwrap();
        assert_eq!(value.to_string().matches("producer()").count(), 1, "{sql}");
    }
    for function in ["SUM", "AVG", "STDEV", "VAR"] {
        let mut value = expression(&format!("{function}(CAST(producer() AS SQL_VARIANT))"));
        let before = value.clone();
        assert_eq!(
            aggregate::mark(&mut value, &Default::default()),
            Err(format!(
                "Operand data type sql_variant is invalid for {} operator.",
                function.to_lowercase()
            ))
        );
        assert_eq!(value, before);
    }
}
