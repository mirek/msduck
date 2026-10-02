use msduck_core::catalog::TypeMetadata;
use msduck_sql::{
    binding_scope::{Field, Scope, Source},
    checked_expression::{Kind, plan, plan_in_scope},
};
use sqlparser::{ast::*, parser::Parser};
use std::{collections::HashMap, ops::ControlFlow};

fn expr(sql: &str) -> Expr {
    Parser::new(&msduck_sql::dialect::ServerDialect)
        .try_with_sql(sql)
        .unwrap()
        .parse_expr()
        .unwrap()
}
fn query(sql: &str) -> Box<Query> {
    let Statement::Query(query) = Parser::parse_sql(&msduck_sql::dialect::ServerDialect, sql)
        .unwrap()
        .remove(0)
    else {
        panic!("query")
    };
    query
}
fn info(id: u8) -> TypeMetadata {
    TypeMetadata {
        system_type_id: Some(id),
        ..Default::default()
    }
}
fn source(qualifier: &str, declaration: Option<u8>) -> Source {
    Source {
        qualifiers: vec![qualifier.into()],
        fields: vec![Field {
            name: "@p".into(),
            info: declaration.map(info),
            properties: Default::default(),
            collation: None,
            json_fragment: false,
        }],
    }
}
fn scope() -> Scope {
    Scope {
        parameters: HashMap::from([("@p".into(), info(127))]),
        rows: vec![Some(vec![source("t", Some(56))])],
        ..Default::default()
    }
}

#[test]
fn retained_captures_distinguish_variable_values_from_column_identity() {
    let capture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../reference/quoted-session-identifiers.json"
    ))
    .unwrap();
    let runs = capture["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 2);
    for run in runs {
        let records = run.as_array().unwrap();
        assert_eq!(records.len(), 30);
        for name in ["bound parameter", "null parameter", "empty parameter"] {
            let record = records.iter().find(|r| r["name"] == name).unwrap();
            let set = &record["result"]["sets"][0];
            assert_eq!(set["columns"][0]["flags"], 33);
            assert_eq!(set["columns"][1]["type"], "IntN");
            assert_eq!(set["columns"][1]["length"], 4);
            assert_eq!(set["columns"][1]["flags"], 9);
            if name == "empty parameter" {
                assert!(set["rows"].as_array().unwrap().is_empty());
            } else {
                assert_eq!(set["rows"][0][1], 7);
                assert_eq!(set["rows"][0][0], record["parameter"]["value"]);
            }
        }
    }
}

#[test]
fn standalone_parameters_do_not_supply_quoted_column_declarations() {
    let declarations = HashMap::from([("@p".into(), Kind::BigInt)]);
    for sql in ["[@p]+1", "\"@p\"+1", "(([@p]))+1", "t.[@p]+1"] {
        let expression = expr(sql);
        let original = expression.clone();
        assert!(plan(&expression, &declarations).is_none(), "{sql}");
        assert_eq!(expression, original);
    }
    assert_eq!(
        plan(&expr("@p+1"), &declarations).unwrap().kind,
        Kind::BigInt
    );
    assert_eq!(declarations["@p"], Kind::BigInt);
}

#[test]
fn scoped_columns_keep_their_own_type_and_single_original_leaf() {
    let scope = scope();
    for sql in ["[@p]+1", "\"@p\"+1", "(([@p]))+1", "t.[@p]+1"] {
        let expression = expr(sql);
        let original = expression.clone();
        let planned = plan_in_scope(&expression, &scope).unwrap();
        assert_eq!(planned.kind, Kind::Int, "{sql}");
        let mut leaves = 0;
        let _ = visit_expressions(&planned.query, |expr| {
            match expr {
                Expr::Identifier(id) if id.value == "@p" => {
                    assert!(id.quote_style.is_some());
                    leaves += 1;
                }
                Expr::CompoundIdentifier(ids) if ids.last().unwrap().value == "@p" => {
                    assert!(ids.last().unwrap().quote_style.is_some());
                    leaves += 1;
                }
                _ => {}
            }
            ControlFlow::<()>::Continue(())
        });
        assert_eq!(leaves, 1, "{sql}");
        assert_eq!(expression, original);
    }
    assert_eq!(
        plan_in_scope(&expr("@p+1"), &scope).unwrap().kind,
        Kind::BigInt
    );
    assert_eq!(scope.parameters["@p"].system_type_id, Some(127));
    assert_eq!(
        scope.rows[0].as_ref().unwrap()[0].fields[0]
            .info
            .as_ref()
            .unwrap()
            .system_type_id,
        Some(56)
    );
}

#[test]
fn unknown_ambiguous_and_shadowing_rows_do_not_fall_back_to_parameters() {
    let mut scope = scope();
    for rows in [
        vec![],
        vec![None],
        vec![Some(vec![source("t", None)])],
        vec![Some(vec![source("a", Some(56)), source("b", Some(127))])],
        vec![
            Some(vec![source("outer", Some(56))]),
            Some(vec![source("inner", None)]),
        ],
    ] {
        scope.rows = rows;
        assert!(plan_in_scope(&expr("[@p]/1"), &scope).is_none());
        assert_eq!(
            plan_in_scope(&expr("@p/1"), &scope).unwrap().kind,
            Kind::BigInt
        );
    }
    assert_eq!(
        plan_in_scope(&expr("[outer].[@p]/1"), &scope).unwrap().kind,
        Kind::Int
    );
    scope.rows.push(None);
    assert!(plan_in_scope(&expr("[outer].[@p]/1"), &scope).is_none());
}

#[test]
fn quoted_row_operands_never_become_empty_source_scalar_checks() {
    for with_parameter in [false, true] {
        let mut scope = scope();
        if !with_parameter {
            scope.parameters.clear();
        }
        for operand in ["[@p]", "\"@p\"", "(([@p]))", "t.[@p]"] {
            let query = query(&format!("SELECT {operand}/0 FROM t WHERE 1=0"));
            let original = query.clone();
            let planned = msduck_sql::checked_projection::plan(&query, &scope).unwrap();
            assert_eq!(planned.kinds, [Kind::Int]);
            assert!(planned.scalar_checks.is_empty(), "{operand}");
            assert_eq!(query, original);
        }
    }
    let planned =
        msduck_sql::checked_projection::plan(&query("SELECT @p/0 FROM t WHERE 1=0"), &scope())
            .unwrap();
    assert_eq!(planned.scalar_checks.len(), 1);
    assert_eq!(planned.kinds, [Kind::BigInt]);
}
