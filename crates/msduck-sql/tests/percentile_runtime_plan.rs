use msduck_sql::{
    binding_scope::Scope,
    percentile::{self, PlanError},
};
use sqlparser::{ast::*, parser::Parser, tokenizer::Tokenizer};

fn expression(source: &str) -> Expr {
    let mut parser = Parser::new(&msduck_sql::dialect::ServerDialect)
        .try_with_sql(source)
        .unwrap();
    parser.parse_expr().unwrap()
}
fn declarations() -> Scope {
    let mut scope = Scope::default();
    for name in ["@p", "@b"] {
        scope.parameters.insert(name.into(), Default::default());
    }
    scope
}
#[test]
fn retained_runtime_requests_keep_syntax_and_defer_execution_diagnostics() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../reference/percentile-runtime-fraction.json"
    ))
    .unwrap();
    assert_eq!(fixture["runs"][0], fixture["runs"][1]);
    let mut requests = 0;
    let mut planned = 0;
    let mut rejected = 0;
    let mut sequence_requests = 0;
    for record in fixture["runs"][0].as_array().unwrap() {
        let name = record["name"].as_str().unwrap();
        let sql = record["sql"].as_str().unwrap();
        // This suite plans percentile requests, not fixture setup/control SQL.
        if !sql.contains("PERCENTILE_") {
            continue;
        }
        if name.contains("sequence side effect") {
            // ServerDialect lacks this node. Preserve that limitation rather
            // than rewriting the query to a backend sequence call.
            assert!(msduck_sql::batch::parse(sql).is_err());
            let tokens = Tokenizer::new(
                &msduck_sql::dialect::ServerDialect,
                "NEXT VALUE FOR dbo.percentile_fraction_sequence",
            )
            .tokenize()
            .unwrap();
            let error = percentile::validate_sequence_source(&tokens).unwrap_err();
            let reference = &record["result"]["errors"][0];
            assert_eq!(error.number, reference["number"].as_i64().unwrap() as i32);
            assert_eq!(error.message, reference["message"].as_str().unwrap());
            assert_eq!((error.state, error.severity), (1, 15));
            sequence_requests += 1;
            continue;
        }
        let statements = msduck_sql::batch::parse(sql).unwrap_or_else(|e| panic!("{name}: {e}"));
        let mut count = 0;
        let _ = visit_expressions(&statements, |expr| {
            let Expr::Function(function) = expr else {
                return std::ops::ControlFlow::<()>::Continue(());
            };
            if !["percentile_cont", "percentile_disc"]
                .contains(&function.name.to_string().to_ascii_lowercase().as_str())
            {
                return std::ops::ControlFlow::Continue(());
            }
            count += 1;
            let before = expr.clone();
            let outcome = percentile::runtime_plan(expr, &declarations());
            assert_eq!(expr, &before, "planning mutated {name}");
            let expected = record["preparation"]["errors"]
                .as_array()
                .and_then(|e| e.first())
                .or_else(|| {
                    record["result"]["errors"]
                        .as_array()
                        .and_then(|e| e.first())
                });
            let compile_error =
                expected.filter(|e| matches!(e["number"].as_i64(), Some(8726 | 5309)));
            if let Some(expected) = compile_error {
                let Err(PlanError::Diagnostic(error)) = outcome else {
                    panic!("{name}: {outcome:?}")
                };
                assert_eq!(
                    error.number,
                    expected["number"].as_i64().unwrap() as i32,
                    "{name}"
                );
                assert_eq!(
                    error.message,
                    expected["message"].as_str().unwrap(),
                    "{name}"
                );
                assert_eq!((error.state, error.severity), (1, 16));
                rejected += 1;
            } else {
                let plan = outcome.unwrap_or_else(|e| panic!("{name}: {e:?}")).unwrap();
                let FunctionArguments::List(args) = &function.args else {
                    panic!("args")
                };
                let FunctionArg::Unnamed(FunctionArgExpr::Expr(fraction)) = &args.args[0] else {
                    panic!("fraction")
                };
                assert_eq!(&plan.fraction, fraction);
                assert_eq!(plan.ordering, function.within_group[0]);
                assert_eq!(Some(plan.window), function.over);
                planned += 1;
            }
            std::ops::ControlFlow::Continue(())
        });
        requests += usize::from(count > 0);
    }
    assert_eq!(sequence_requests, 2);
    assert_eq!(requests, 128);
    assert_eq!(planned, 120);
    assert_eq!(rejected, 12);
}
#[test]
fn unknown_shapes_and_declarations_do_not_become_backend_guesses() {
    for source in [
        "missing(@p)",
        "RAND(42)",
        "(SELECT .5 WHERE 1=1)",
        "(SELECT *)",
        "(SELECT t.*)",
        "CAST(@p AS DATE)",
        "@undeclared",
        "[\u{40}p]",
    ] {
        let expr = expression(&format!(
            "PERCENTILE_CONT({source}) WITHIN GROUP(ORDER BY n) OVER()"
        ));
        let result = percentile::runtime_plan(&expr, &declarations());
        if source.starts_with('[') {
            assert!(matches!(result, Err(PlanError::Diagnostic(_))))
        } else {
            assert_eq!(result, Err(PlanError::Unknown), "{source}")
        }
    }
    let expr = expression(
        "PERCENTILE_CONT(CASE WHEN 1=1 THEN .5 ELSE RAND()*0 END) WITHIN GROUP(ORDER BY n DESC) OVER(PARTITION BY g)",
    );
    let plan = percentile::runtime_plan(&expr, &declarations())
        .unwrap()
        .unwrap();
    assert_eq!(plan.fraction.to_string().matches("RAND()").count(), 1);
    assert!(plan.fraction.to_string().contains("ELSE RAND() * 0"));
    assert_eq!(plan.ordering.options.sort, Some(OrderBySort::Desc));
    assert!(plan.window.to_string().contains("PARTITION BY g"));
    // Eligibility is independent of declaration metadata, including unknown type.
    let mut changed = declarations();
    changed.parameters.get_mut("@p").unwrap().system_type_id = Some(231);
    let expr = expression("PERCENTILE_DISC(@P) WITHIN GROUP(ORDER BY n) OVER()");
    assert_eq!(
        percentile::runtime_plan(&expr, &changed),
        percentile::runtime_plan(&expr, &declarations())
    );
}

#[test]
fn numeric_literal_syntax_stays_compile_time_but_range_and_text_stay_deferred() {
    for source in [
        "1e309",
        "0.000000000000000000000000000000000000001",
        "CASE WHEN 1=1 THEN .5 ELSE 1e309 END",
    ] {
        let expr = expression(&format!(
            "PERCENTILE_CONT({source}) WITHIN GROUP(ORDER BY n) OVER()"
        ));
        let Err(PlanError::Diagnostic(error)) = percentile::runtime_plan(&expr, &declarations())
        else {
            panic!("{source}")
        };
        assert!(matches!(error.number, 168 | 1007));
        assert_eq!((error.state, error.severity), (1, 15));
    }
    for source in ["2", "NULL", "'abc'", "'1e309'", "1e-400"] {
        let expr = expression(&format!(
            "PERCENTILE_CONT({source}) WITHIN GROUP(ORDER BY n) OVER()"
        ));
        assert!(
            percentile::runtime_plan(&expr, &declarations())
                .unwrap()
                .is_some(),
            "{source}"
        );
    }
}
#[test]
fn sequence_tokens_require_unquoted_keywords_and_fraction_scope() {
    for sql in [
        "'NEXT VALUE FOR s'",
        "[NEXT] [VALUE] [FOR]",
        ".5 + 'NEXT VALUE FOR'",
    ] {
        let tokens = Tokenizer::new(&msduck_sql::dialect::ServerDialect, sql)
            .tokenize()
            .unwrap();
        assert!(
            percentile::validate_sequence_source(&tokens).is_ok(),
            "{sql}"
        );
    }
    let tokens = Tokenizer::new(
        &msduck_sql::dialect::ServerDialect,
        "CASE WHEN 1=0 THEN NEXT /* bounded */ VALUE FOR s ELSE .5 END",
    )
    .tokenize()
    .unwrap();
    assert_eq!(
        percentile::validate_sequence_source(&tokens)
            .unwrap_err()
            .number,
        11720
    );
}
