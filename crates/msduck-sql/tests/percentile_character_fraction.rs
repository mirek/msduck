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
fn retained_character_fraction_matrix_lowers_or_preserves_captured_error_text() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../reference/percentile-character-fraction.json"
    ))
    .unwrap();
    assert_eq!(fixture["runs"][0], fixture["runs"][1]);
    let mut checked = 0;
    for record in fixture["runs"][0].as_array().unwrap() {
        let name = record["name"].as_str().unwrap();
        if record["mode"] != "batch" || !(name.starts_with("CONT") || name.starts_with("DISC")) {
            continue;
        }
        // Existing numeric-literal exact-range policy is unchanged in this
        // character task. Both captured numeric endpoint discrepancies remain
        // in the raw client report and docs, rather than being called matches.
        if name.ends_with("expression 1.00000000000000000001") {
            continue;
        }
        let sql = record["sql"].as_str().unwrap();
        let mut expr = expression(sql);
        let original = expr.clone();
        let lowered = percentile::lower(&mut expr);
        if let Some(error) = record["result"]["errors"].as_array().unwrap().first() {
            assert_eq!(
                lowered,
                Err(error["message"].as_str().unwrap().into()),
                "{name}"
            );
            assert_eq!(expr, original, "failed lowering mutated {name}");
        } else {
            lowered.unwrap_or_else(|error| panic!("{name}: {error}"));
            let Expr::Function(function) = expr else {
                panic!("lowered function")
            };
            assert!(matches!(
                function.name.to_string().as_str(),
                "quantile_cont" | "quantile_disc" | "max"
            ));
            assert!(function.within_group.is_empty());
            assert!(function.over.is_some());
        }
        checked += 1;
    }
    assert_eq!(checked, 100);
}

#[test]
fn captured_whitespace_exponents_and_nul_termination_are_source_sensitive() {
    for (fraction, expected) in [
        ("N'\u{180e}0.5'", "0.5"),
        ("N'1D-1'", "0.1"),
        ("N'0.5\0junk'", "0.5"),
        ("N'\0junk'", "0"),
        ("'1.00000000000000000001'", "1"),
        ("'1e-400'", "0"),
    ] {
        let mut expr = expression(&format!(
            "SELECT PERCENTILE_CONT({fraction}) WITHIN GROUP (ORDER BY n) OVER ()"
        ));
        percentile::lower(&mut expr).unwrap();
        assert!(
            expr.to_string()
                .ends_with(&format!(", {expected}) OVER ()")),
            "{expr}"
        );
    }
    for fraction in ["N'0.5\t'", "N'\u{202f}'", "N'1d'", "N'0.5\u{180e}'"] {
        let mut expr = expression(&format!(
            "SELECT PERCENTILE_CONT({fraction}) WITHIN GROUP (ORDER BY n) OVER ()"
        ));
        assert_eq!(
            percentile::lower(&mut expr),
            Err("Error converting data type nvarchar to float.".into())
        );
    }
}

#[test]
fn constant_cast_width_and_descending_zero_keep_single_operand_evaluation() {
    for fraction in [
        "CAST('0.55' AS VARCHAR(2))",
        "__msduck_cast_varchar('0.55', 2)",
    ] {
        let mut expr = expression(&format!(
            "SELECT PERCENTILE_DISC({fraction}) WITHIN GROUP (ORDER BY nextval('percentile_calls') DESC) OVER (PARTITION BY g)"
        ));
        percentile::lower(&mut expr).unwrap();
        assert!(expr.to_string().starts_with("max("));
        assert_eq!(
            expr.to_string()
                .matches("nextval('percentile_calls')")
                .count(),
            1
        );
        assert!(expr.to_string().ends_with("OVER (PARTITION BY g)"));
    }
    for fraction in [
        "@p",
        "CAST(@p AS VARCHAR(8))",
        "__msduck_cast_varchar(nextval('calls'), 8)",
    ] {
        let mut expr = expression(&format!(
            "SELECT PERCENTILE_CONT({fraction}) WITHIN GROUP (ORDER BY n) OVER ()"
        ));
        let original = expr.clone();
        assert!(percentile::lower(&mut expr).is_err());
        assert_eq!(expr, original);
    }
}
