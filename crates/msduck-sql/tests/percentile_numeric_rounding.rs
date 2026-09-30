use msduck_sql::{dialect::ServerDialect, percentile};
use sqlparser::{ast::*, parser::Parser};

fn expression(sql: &str) -> Expr {
    let Statement::Query(query) = Parser::parse_sql(&ServerDialect, sql).unwrap().remove(0) else {
        panic!("query")
    };
    let SetExpr::Select(select) = *query.body else {
        panic!("select")
    };
    match select.projection.into_iter().last().unwrap() {
        SelectItem::ExprWithAlias { expr, .. } | SelectItem::UnnamedExpr(expr) => expr,
        _ => panic!("expression"),
    }
}
#[test]
fn retained_numeric_shapes_lower_or_preserve_captured_diagnostics_without_mutation() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../reference/percentile-numeric-rounding.json"
    ))
    .unwrap();
    assert_eq!(fixture["runs"][0], fixture["runs"][1]);
    let mut checked = 0;
    for record in fixture["runs"][0].as_array().unwrap() {
        let name = record["name"].as_str().unwrap();
        if !(name.starts_with("CONT ") || name.starts_with("DISC ")) {
            continue;
        }
        let mut expr = expression(record["sql"].as_str().unwrap());
        let original = expr.clone();
        let result = percentile::lower(&mut expr);
        if let Some(error) = record["result"]["errors"].as_array().unwrap().first() {
            assert_eq!(
                result,
                Err(error["message"].as_str().unwrap().to_owned()),
                "{name}"
            );
            assert_eq!(expr, original, "failed lower mutated {name}");
            if [1007, 168].contains(&error["number"].as_i64().unwrap()) {
                let actual =
                    percentile::literal_diagnostic(error["message"].as_str().unwrap()).unwrap();
                assert_eq!(
                    actual.number,
                    i32::try_from(error["number"].as_i64().unwrap()).unwrap()
                );
                assert_eq!((actual.state, actual.severity), (1, 15));
            }
        } else {
            result.unwrap_or_else(|e| panic!("{name}: {e}"));
            let Expr::Function(f) = expr else {
                panic!("function")
            };
            assert!(matches!(
                f.name.to_string().as_str(),
                "quantile_cont" | "quantile_disc" | "max"
            ));
            assert!(f.within_group.is_empty());
            assert!(f.over.is_some());
        }
        checked += 1;
    }
    assert_eq!(checked, 144);
}
#[test]
fn canonical_literal_diagnostics_reject_wrappers_altered_text_and_wrong_source_shapes() {
    for message in [
        "The number '0.000000000000000000000000000000000000001' is out of the range for numeric representation (maximum precision 38).",
        "The floating point value '1e309' is out of the range of computer representation (8 bytes).",
    ] {
        assert!(percentile::literal_diagnostic(message).is_some());
        for invalid in [
            format!("Invalid Input Error: {message}"),
            format!("{message} trailing"),
            message.replace("value", "expression"),
            message.replace("1e309", "0e309"),
        ] {
            if invalid != message {
                assert!(
                    percentile::literal_diagnostic(&invalid).is_none(),
                    "{invalid}"
                );
            }
        }
    }
    assert!(percentile::literal_diagnostic("The number '0.5' is out of the range for numeric representation (maximum precision 38).").is_none());
}
