use msduck_core::catalog::TypeMetadata;
use msduck_sql::{
    binding_scope::{Field, Scope},
    catalog_snapshot::CatalogSnapshot,
    projection::order::{Barrier, Plan, infer},
};
use serde_json::Value;
use sqlparser::{
    ast::{Query, Statement},
    parser::Parser,
};

fn catalog() -> CatalogSnapshot {
    let mut catalog = CatalogSnapshot::default();
    for (name, id, width, precision) in [
        ("int", 56, 4, 10),
        ("bigint", 127, 8, 19),
        ("nvarchar", 231, 16, 0),
    ] {
        catalog.types.insert(
            name.into(),
            TypeMetadata {
                system_type_id: Some(id),
                user_type_id: Some(i32::from(id)),
                max_length: Some(width),
                precision: Some(precision),
                scale: Some(0),
                collation_name: None,
            },
        );
    }
    let field = |name: &str, kind: &str| Field {
        name: name.into(),
        info: catalog.types.get(kind).cloned(),
        collation: None,
        json_fragment: false,
        properties: Default::default(),
    };
    catalog.tables.insert(
        "dbo.order_heap".into(),
        vec![
            field("a", "int"),
            field("b", "int"),
            field("label", "nvarchar"),
        ],
    );
    catalog.tables.insert(
        "dbo.order_clustered".into(),
        vec![field("a", "int"), field("b", "int")],
    );
    catalog
}
fn queries(sql: &str) -> Vec<Query> {
    Parser::parse_sql(&msduck_sql::dialect::ServerDialect, sql)
        .unwrap()
        .into_iter()
        .filter_map(|statement| {
            if let Statement::Query(query) = statement {
                Some(*query)
            } else {
                None
            }
        })
        .collect()
}
fn plan(sql: &str) -> Plan {
    infer(&catalog(), &queries(sql).remove(0), &Scope::default())
}

#[test]
fn retained_order_presence_and_ordinals_match_for_every_resolved_query() {
    let reference: Value =
        serde_json::from_str(include_str!("../../../reference/order-token.json")).unwrap();
    let catalog = catalog();
    let mut scope = Scope::default();
    scope
        .parameters
        .insert("@b".into(), catalog.types["int"].clone());
    let mut unknown = Vec::new();
    let mut resolved = 0;
    for record in reference["runs"][0].as_array().unwrap() {
        if record["name"] == "setup" {
            continue;
        };
        let ast = queries(record["sql"].as_str().unwrap());
        let expected_events = if record["mode"] == "prepared" {
            &record["preparation"]["events"]
        } else {
            &record["result"]["events"]
        };
        let expected: Vec<Vec<u16>> = expected_events
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["kind"] == "ORDER")
            .map(|e| {
                e["ordinals"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_u64().unwrap() as u16)
                    .collect()
            })
            .collect();
        let plans: Vec<_> = ast.iter().map(|q| infer(&catalog, q, &scope)).collect();
        if plans.iter().any(|p| matches!(p, Plan::Unknown(_))) {
            unknown.push((
                record["name"].as_str().unwrap().to_string(),
                record["mode"].as_str().unwrap().to_string(),
                plans,
            ));
        } else {
            let actual: Vec<_> = plans
                .clone()
                .into_iter()
                .filter_map(|p| {
                    if let Plan::Token(values) = p {
                        Some(values)
                    } else {
                        None
                    }
                })
                .collect();
            assert_eq!(actual, expected, "{} {}", record["name"], record["mode"]);
            if record["mode"] == "prepared" {
                for execution in record["executions"].as_array().unwrap() {
                    let actual_orders: Vec<Vec<u16>> = execution["result"]["events"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|event| event["kind"] == "ORDER")
                        .map(|event| {
                            event["ordinals"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .map(|value| value.as_u64().unwrap() as u16)
                                .collect()
                        })
                        .collect();
                    assert_eq!(
                        actual, actual_orders,
                        "prepared execution {}",
                        record["name"]
                    );
                }
                assert!(
                    !record["unpreparation"]["events"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|event| event["kind"] == "ORDER")
                );
            }
            resolved += 1;
        }
    }
    eprintln!("resolved {resolved}, unknown {unknown:?}");
    assert_eq!(resolved, 65);
    assert_eq!(
        unknown
            .iter()
            .map(|(name, mode, _)| (name.as_str(), mode.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("error before order", "batch"),
            ("error before order", "rpc")
        ]
    );
}

#[test]
fn catalog_alias_identity_is_explicit_and_empty_predicates_do_not_change_plans() {
    for suffix in ["", " WHERE 1=0", " WHERE a>42"] {
        assert_eq!(
            plan(&format!(
                "SELECT b,a AS k FROM dbo.order_heap{suffix} ORDER BY k DESC"
            )),
            Plan::Token(vec![2])
        );
    }
    assert_eq!(
        plan("SELECT h.a AS x FROM dbo.order_heap h ORDER BY h.a"),
        Plan::Token(vec![1])
    );
    assert_eq!(
        plan("SELECT h.a FROM dbo.order_heap h JOIN dbo.order_clustered c ON h.a=c.a ORDER BY c.a"),
        Plan::Token(vec![0])
    );
    assert_eq!(
        plan("SELECT a AS x,b AS x FROM dbo.order_heap ORDER BY x"),
        Plan::Unknown(Barrier::AmbiguousName)
    );
    assert!(matches!(
        plan("SELECT missing FROM dbo.order_heap ORDER BY a"),
        Plan::Unknown(_)
    ));
}

#[test]
fn unproven_folding_unknown_shapes_and_bound_sort_values_are_barriers() {
    for sql in [
        "SELECT * FROM dbo.order_heap ORDER BY a",
        "SELECT a FROM missing_source ORDER BY a",
        "SELECT 1 AS k FROM dbo.order_heap ORDER BY k",
        "SELECT a FROM dbo.order_heap ORDER BY 0",
        "SELECT a FROM dbo.order_heap ORDER BY @b",
        "SELECT a FROM dbo.order_heap ORDER BY a+2",
        "SELECT a FROM dbo.order_heap ORDER BY COALESCE(a,1)",
    ] {
        assert!(matches!(plan(sql), Plan::Unknown(_)), "{sql}");
    }
    assert_eq!(
        plan("SELECT CAST(NULL AS INT) AS k FROM dbo.order_heap ORDER BY k"),
        Plan::NoToken
    );
    assert_eq!(
        plan("SELECT a FROM dbo.order_heap ORDER BY (SELECT NULL)"),
        Plan::Token(vec![0])
    );
    assert_eq!(
        plan("SELECT ROW_NUMBER() OVER(ORDER BY a) AS k FROM dbo.order_heap"),
        Plan::NoToken
    );
}

#[test]
fn supplemental_fresh_sql_server_optimizer_cases() {
    for (sql, expected) in [
        ("SELECT 1 AS a UNION ALL SELECT 1 ORDER BY a", vec![1]),
        ("SELECT 1 AS a UNION SELECT 1 ORDER BY a", vec![1]),
        (
            "SELECT CAST(NULL AS INT) AS a UNION ALL SELECT CAST(NULL AS INT) ORDER BY a",
            vec![1],
        ),
        (
            "SELECT CAST(NULL AS INT) AS a UNION ALL SELECT 1 ORDER BY a",
            vec![1],
        ),
        (
            "SELECT h.a FROM dbo.order_heap h JOIN dbo.order_clustered c ON h.a=c.a ORDER BY c.a",
            vec![0],
        ),
        (
            "SELECT h.a FROM dbo.order_heap h JOIN dbo.order_clustered c ON h.a=c.a ORDER BY h.a",
            vec![1],
        ),
        (
            "SELECT h.a,c.a FROM dbo.order_heap h JOIN dbo.order_clustered c ON h.a=c.a ORDER BY c.a",
            vec![2],
        ),
        (
            "SELECT h.a FROM dbo.order_heap h JOIN dbo.order_clustered c ON h.a<c.a ORDER BY c.a",
            vec![0],
        ),
        (
            "SELECT h.a FROM dbo.order_heap h LEFT JOIN dbo.order_clustered c ON h.a=c.a ORDER BY c.a",
            vec![0],
        ),
        ("SELECT a FROM dbo.order_heap WHERE a=1 ORDER BY a", vec![1]),
        ("SELECT TOP(1) a FROM dbo.order_heap ORDER BY a", vec![1]),
        (
            "SELECT a,ROW_NUMBER() OVER(PARTITION BY b ORDER BY a) AS n FROM dbo.order_heap ORDER BY n",
            vec![2],
        ),
        (
            "SELECT a,ROW_NUMBER() OVER(PARTITION BY b ORDER BY b,a) AS n FROM dbo.order_heap WHERE 1=0 ORDER BY n",
            vec![2],
        ),
    ] {
        assert_eq!(plan(sql), Plan::Token(expected), "{sql}");
    }
    for sql in [
        "SELECT CAST(NULL AS INT) AS k FROM dbo.order_heap WHERE 1=0 ORDER BY k",
        "SELECT TOP(0) CAST(NULL AS INT) AS k FROM dbo.order_heap ORDER BY k",
        "SELECT CAST(NULL AS INT) AS k ORDER BY k",
        "SELECT a,CAST(NULL AS INT) AS k FROM dbo.order_heap ORDER BY k",
    ] {
        assert_eq!(plan(sql), Plan::NoToken, "{sql}");
        let Statement::Query(query) = msduck_sql::batch::parse(sql).unwrap().remove(0) else {
            panic!("query");
        };
        let original = query.clone();
        assert_eq!(
            infer(&catalog(), &query, &Scope::default()),
            Plan::NoToken,
            "normalized {sql}"
        );
        assert_eq!(query, original, "planner must not mutate the source AST");
    }
    assert_eq!(
        plan("SELECT a,a FROM dbo.order_heap ORDER BY a"),
        Plan::Unknown(Barrier::AmbiguousName)
    );
}
