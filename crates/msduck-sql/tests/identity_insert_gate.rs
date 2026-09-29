#[path = "../src/identity_insert_gate.rs"]
mod identity_insert_gate;

use identity_insert_gate::{GateError, Permit, ResolvedTarget, preflight};
use serde_json::Value;
use sqlparser::ast::{ObjectName, Statement};

fn fixture(name: &str) -> Value {
    serde_json::from_str(match name {
        "batch" => include_str!("../../../reference/identity-insert.json"),
        "errors" => include_str!("../../../reference/identity-insert-errors.json"),
        "rpc" => include_str!("../../../reference/identity-insert-rpc.json"),
        "multirow" => include_str!("../../../reference/identity-insert-multirow.json"),
        "shapes" => include_str!("../../../reference/identity-insert-shapes.json"),
        _ => panic!("unknown fixture"),
    })
    .unwrap()
}

fn insert(sql: &str) -> Statement {
    msduck_sql::batch::parse(sql)
        .unwrap()
        .into_iter()
        .find(|statement| matches!(statement, Statement::Insert(_)))
        .unwrap()
}

fn resolve_column(name: &ObjectName) -> Option<usize> {
    let id = name.0.last()?.as_ident()?;
    if id.value.eq_ignore_ascii_case("id") {
        Some(0)
    } else if id.value.eq_ignore_ascii_case("v") {
        Some(1)
    } else {
        None
    }
}

fn decision(sql: &str, table: &str, setting_on: bool) -> Result<Permit, GateError> {
    let key = 7u64;
    let other = 8u64;
    preflight(
        &insert(sql),
        &ResolvedTarget {
            key: &key,
            schema: "dbo",
            table,
            column_count: 2,
            identity_column: Some(0),
        },
        setting_on.then_some(&key).or(Some(&other)),
        resolve_column,
    )
}

fn case<'a>(run: &'a Value, name: &str) -> &'a Value {
    run.as_array()
        .unwrap()
        .iter()
        .find(|item| item["name"] == name)
        .unwrap()
}

fn assert_diagnostic(record: &Value, table: &str, setting_on: bool) {
    let failure = decision(record["sql"].as_str().unwrap(), table, setting_on).unwrap_err();
    let GateError::Diagnostic {
        error,
        done_command,
    } = failure
    else {
        panic!("expected SQL diagnostic for {}", record["name"])
    };
    let captured = &record["result"]["errors"][0];
    assert_eq!(error.number as i64, captured["number"].as_i64().unwrap());
    assert_eq!(error.state as u64, captured["state"].as_u64().unwrap());
    assert_eq!(error.severity as u64, captured["class"].as_u64().unwrap());
    assert_eq!(error.message, captured["message"].as_str().unwrap());
    assert_eq!(
        done_command as u64,
        record["result"]["doneTokens"][0]["command"]
            .as_u64()
            .unwrap()
    );
    assert_eq!(record["result"]["doneTokens"][0]["status"], 2);
}

#[test]
fn batch_capture_replays_permission_and_messages() {
    let retained = fixture("batch");
    for run in retained["runs"].as_array().unwrap() {
        for (name, table, on) in [
            ("A explicit without column list", "alpha", true),
            ("B explicit while off", "alpha", false),
            ("A automatic while on", "alpha", true),
            ("B explicit after off", "alpha", false),
            ("A explicit after off", "alpha", false),
            ("A beta automatic while on", "beta", true),
            ("A explicit after final off", "alpha", false),
        ] {
            assert_diagnostic(case(run, name), table, on);
        }
        for (name, table, on, expected) in [
            (
                "A high explicit",
                "alpha",
                true,
                Permit::Explicit { source_column: 0 },
            ),
            (
                "B explicit while on",
                "alpha",
                true,
                Permit::Explicit { source_column: 0 },
            ),
            (
                "A alpha still on",
                "alpha",
                true,
                Permit::Explicit { source_column: 0 },
            ),
            ("A automatic after off", "alpha", false, Permit::Generated),
            (
                "A beta explicit high",
                "beta",
                true,
                Permit::Explicit { source_column: 0 },
            ),
            (
                "A beta automatic after off",
                "beta",
                false,
                Permit::Generated,
            ),
            (
                "explicit after setting rollback",
                "alpha",
                true,
                Permit::Explicit { source_column: 0 },
            ),
        ] {
            let record = case(run, name);
            assert_eq!(
                decision(record["sql"].as_str().unwrap(), table, on),
                Ok(expected),
                "{name}"
            );
        }
    }
}

#[test]
fn error_and_multirow_captures_preflight_before_source_failure() {
    for run in fixture("errors")["runs"].as_array().unwrap() {
        assert_diagnostic(case(run, "DEFAULT VALUES while ON"), "alpha", true);
        for name in [
            "failed UNIQUE",
            "failed CHECK",
            "failed NOT NULL",
            "failed conversion",
            "explicit inside transaction",
        ] {
            let record = case(run, name);
            assert_eq!(
                decision(record["sql"].as_str().unwrap(), "alpha", true),
                Ok(Permit::Explicit { source_column: 0 }),
                "{name}"
            );
        }
        let generated = case(run, "generated after failures");
        assert_eq!(
            decision(generated["sql"].as_str().unwrap(), "alpha", false),
            Ok(Permit::Generated)
        );
    }
    for run in fixture("multirow")["runs"].as_array().unwrap() {
        for name in [
            "multi VALUES OUTPUT",
            "multi INSERT SELECT",
            "later UNIQUE with OUTPUT",
            "later CHECK",
            "later conversion",
            "later SELECT UNIQUE with OUTPUT",
            "multi VALUES inside transaction",
        ] {
            let record = case(run, name);
            assert_eq!(
                decision(record["sql"].as_str().unwrap(), "alpha", true),
                Ok(Permit::Explicit { source_column: 0 }),
                "{name}"
            );
        }
        for name in [
            "generated after UNIQUE",
            "generated after CHECK",
            "generated after conversion",
            "generated after SELECT UNIQUE",
            "generated after rollback",
        ] {
            let record = case(run, name);
            assert_eq!(
                decision(record["sql"].as_str().unwrap(), "alpha", false),
                Ok(Permit::Generated)
            );
        }
    }
}

#[test]
fn rpc_capture_uses_caller_setting_not_nested_set_text() {
    for run in fixture("rpc")["runs"].as_array().unwrap() {
        for name in [
            "outer explicit after RPC ON",
            "outer explicit after RPC OFF",
            "outer explicit after prepare only",
            "outer explicit after prepared execute",
            "outer explicit after unprepare",
        ] {
            assert_diagnostic(case(run, name), "alpha", false);
        }
        for name in [
            "outer explicit after nested RPC OFF",
            "RPC explicit while outer ON",
        ] {
            let record = case(run, name);
            assert_eq!(
                decision(record["sql"].as_str().unwrap(), "alpha", true),
                Ok(Permit::Explicit { source_column: 0 }),
                "{name}"
            );
        }
    }
}

#[test]
fn insert_shape_capture_replays_positional_and_source_precedence() {
    for run in fixture("shapes")["runs"].as_array().unwrap() {
        for (name, on) in [
            ("OFF positional VALUES", false),
            ("OFF positional DEFAULT", false),
            ("ON positional VALUES", true),
            ("ON positional DEFAULT", true),
            ("OFF explicit conversion", false),
            ("ON omitted INSERT SELECT", true),
            ("ON omitted conversion", true),
            ("ON duplicate identity columns", true),
            ("ON invalid column", true),
        ] {
            assert_diagnostic(case(run, name), "alpha", on);
        }
        for (name, on, expected) in [
            (
                "ON explicit conversion",
                true,
                Permit::Explicit { source_column: 0 },
            ),
            (
                "ON explicit INSERT SELECT",
                true,
                Permit::Explicit { source_column: 0 },
            ),
            ("OFF omitted INSERT SELECT", false, Permit::Generated),
        ] {
            let record = case(run, name);
            assert_eq!(
                decision(record["sql"].as_str().unwrap(), "alpha", on),
                Ok(expected),
                "{name}"
            );
        }
    }
}

#[test]
fn catalog_identity_aliases_sessions_and_preparation_are_explicit() {
    let statement = insert("INSERT [dbo].[alpha] ([v],[id]) VALUES(4,20)");
    let key = (1u32, 17u32);
    let alias = (1u32, 17u32);
    let other = (2u32, 17u32);
    let target = ResolvedTarget {
        key: &key,
        schema: "dbo",
        table: "alpha",
        column_count: 2,
        identity_column: Some(0),
    };
    assert_eq!(
        preflight(&statement, &target, Some(&alias), resolve_column),
        Ok(Permit::Explicit { source_column: 1 })
    );
    let GateError::Diagnostic { error, .. } =
        preflight(&statement, &target, Some(&other), resolve_column).unwrap_err()
    else {
        panic!("other session must be OFF")
    };
    assert_eq!(error.number, 544);
    assert!(
        matches!(preflight(&statement, &target, None, resolve_column),
        Err(GateError::Diagnostic { error, .. }) if error.number == 544)
    );
    // A prepared statement can be classified repeatedly without changing the
    // caller's immutable setting; runtime values and source conversion are not read.
    assert_eq!(
        preflight(&statement, &target, Some(&alias), resolve_column),
        Ok(Permit::Explicit { source_column: 1 })
    );
    let plain = ResolvedTarget {
        key: &key,
        schema: "dbo",
        table: "plain",
        column_count: 2,
        identity_column: None,
    };
    assert_eq!(
        preflight(&statement, &plain, None, resolve_column),
        Ok(Permit::NotApplicable)
    );
}

#[test]
fn unknown_shapes_fail_closed_before_diagnostic_precedence() {
    let key = 1u64;
    let target = ResolvedTarget {
        key: &key,
        schema: "dbo",
        table: "alpha",
        column_count: 2,
        identity_column: Some(0),
    };
    for sql in [
        "INSERT dbo.alpha(id,v) VALUES(2)",
        "INSERT dbo.alpha(v,v) VALUES(2,3)",
        "INSERT dbo.alpha(id,id,unknown) VALUES(2,3,4)",
        "INSERT dbo.alpha(id,v) VALUES(DEFAULT,3)",
        "INSERT dbo.alpha SELECT id,v FROM dbo.source_rows",
    ] {
        assert!(
            matches!(
                preflight(&insert(sql), &target, Some(&key), resolve_column),
                Err(GateError::Unsupported(_))
            ),
            "{sql}"
        );
    }
    for sql in [
        "INSERT dbo.alpha(id,unknown) VALUES(2,3)",
        "INSERT dbo.alpha(id,id) VALUES(2,3)",
    ] {
        assert!(matches!(
            preflight(&insert(sql), &target, None, resolve_column),
            Err(GateError::Unsupported(_))
        ));
    }
    for sql in [
        "INSERT dbo.alpha VALUES(DEFAULT,3),(40,4)",
        "INSERT dbo.alpha VALUES(20,3),(40,4)",
        "INSERT dbo.alpha VALUES(DEFAULT,(SELECT 3))",
        "INSERT dbo.alpha VALUES(20,ABS(3))",
    ] {
        for active in [None, Some(&key)] {
            assert!(
                matches!(
                    preflight(&insert(sql), &target, active, resolve_column),
                    Err(GateError::Unsupported(_))
                ),
                "{sql}"
            );
        }
    }
    let mut listed_default = insert("INSERT dbo.alpha(id) VALUES(2)");
    let Statement::Insert(insert) = &mut listed_default else {
        unreachable!()
    };
    insert.source = None;
    assert!(matches!(
        preflight(&listed_default, &target, Some(&key), resolve_column),
        Err(GateError::Unsupported(_))
    ));
    assert_eq!(
        preflight(
            &msduck_sql::batch::parse("SELECT 1").unwrap()[0],
            &target,
            Some(&key),
            resolve_column
        ),
        Ok(Permit::NotApplicable)
    );
}
