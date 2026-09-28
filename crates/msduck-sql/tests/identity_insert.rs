#[path = "../src/identity_insert.rs"]
mod identity_insert;

use identity_insert::{BindError, Operation, SessionState, Transition, operation};
use serde_json::Value;
use sqlparser::ast::{
    Ident, ObjectName, ObjectNamePart, SessionParamValue, Set, SetSessionParamIdentityInsert,
    SetSessionParamKind, Statement,
};

fn parse_setting(sql: &str) -> Operation {
    let statements = msduck_sql::batch::parse(sql).unwrap();
    statements
        .iter()
        .find_map(|statement| operation(statement).unwrap())
        .unwrap()
}

fn fixture(path: &str) -> Value {
    let bytes = match path {
        "batch" => include_str!("../../../reference/identity-insert.json"),
        "rpc" => include_str!("../../../reference/identity-insert-rpc.json"),
        "errors" => include_str!("../../../reference/identity-insert-errors.json"),
        _ => panic!("unknown fixture"),
    };
    serde_json::from_str(bytes).unwrap()
}

fn observed_error(record: &Value) -> Option<i64> {
    record["result"]["errors"]
        .as_array()
        .unwrap()
        .first()
        .map(|e| e["number"].as_i64().unwrap())
}

#[test]
fn ast_preserves_identifier_parts_and_rejects_unknown_shapes() {
    let setting = parse_setting("SET IDENTITY_INSERT [dbo].[a]]b] ON");
    assert_eq!(
        setting
            .parts
            .iter()
            .map(|part| part.value.as_str())
            .collect::<Vec<_>>(),
        ["dbo", "a]b"]
    );
    assert_eq!(setting.parts[0].quote_style, Some('['));
    assert!(setting.enabled);
    assert_eq!(
        parse_setting("SET IDENTITY_INSERT [Db].[dbo].[a]]b] OFF")
            .parts
            .len(),
        3
    );
    assert!(!parse_setting("SET IDENTITY_INSERT dbo.alpha OFF").enabled);
    assert!(matches!(
        operation(&msduck_sql::batch::parse("SET NOCOUNT ON").unwrap()[0]),
        Ok(None)
    ));
    for sql in [
        "SET IDENTITY_INSERT dbo.alpha MAYBE",
        "SET IDENTITY_INSERT dbo.alpha",
        "SET IDENTITY_INSERT ON",
    ] {
        assert!(msduck_sql::batch::parse(sql).is_err(), "{sql}");
    }
    // Construct parser-supported but unbindable shapes explicitly. Catalog
    // lookup must never receive a function/object-name expression or 4 parts.
    let unsupported = Statement::Set(Set::SetSessionParam(SetSessionParamKind::IdentityInsert(
        SetSessionParamIdentityInsert {
            obj: ObjectName(vec![
                ObjectNamePart::Identifier(Ident::new("a")),
                ObjectNamePart::Identifier(Ident::new("b")),
                ObjectNamePart::Identifier(Ident::new("c")),
                ObjectNamePart::Identifier(Ident::new("d")),
            ]),
            value: SessionParamValue::On,
        },
    )));
    assert_eq!(operation(&unsupported), Err(BindError::UnsupportedTarget));
}

#[test]
fn batch_fixture_replays_two_sessions_and_conflict_preservation() {
    let retained = fixture("batch");
    for run in retained["runs"].as_array().unwrap() {
        let mut a = SessionState::<u64>::default();
        let mut b = SessionState::<u64>::default();
        for record in run.as_array().unwrap() {
            let sql = record["sql"].as_str().unwrap();
            if !sql.contains("IDENTITY_INSERT") {
                continue;
            }
            let setting = parse_setting(sql);
            let table = setting.parts.last().unwrap().value.to_ascii_lowercase();
            let resolved = match table.as_str() {
                "alpha" => 11,
                "beta" => 12,
                _ => panic!("unexpected table"),
            };
            let state = if record["session"] == "B" {
                &mut b
            } else {
                &mut a
            };
            let result = state.apply(&setting, resolved);
            if observed_error(record) == Some(8107) {
                assert_eq!(result.unwrap_err().active, 11);
            } else {
                assert!(result.is_ok(), "{}", record["name"]);
            }
            if record["name"] == "transaction setting rollback" {
                assert_eq!(state.active(), Some(&11), "ROLLBACK must not undo SET");
            }
        }
        assert_eq!(a.active(), None);
        assert_eq!(b.active(), None);
    }
}

#[test]
fn error_fixture_replays_idempotence_and_pre_resolution_failures() {
    let retained = fixture("errors");
    for run in retained["runs"].as_array().unwrap() {
        let mut state = SessionState::<u64>::default();
        for record in run.as_array().unwrap() {
            let sql = record["sql"].as_str().unwrap();
            if !sql.starts_with("SET IDENTITY_INSERT") {
                continue;
            }
            let setting = parse_setting(sql);
            let table = setting.parts.last().unwrap().value.as_str();
            if matches!(table, "plain" | "missing") {
                assert_eq!(
                    observed_error(record),
                    Some(if table == "plain" { 8106 } else { 1088 })
                );
                assert_eq!(state.active(), Some(&11));
                continue; // root catalog resolution fails before apply()
            }
            let transition = state
                .apply(&setting, if table == "alpha" { 11 } else { 12 })
                .unwrap();
            match record["name"].as_str().unwrap() {
                "alpha ON" => assert_eq!(transition, Transition::Enabled),
                "alpha ON repeated" => assert_eq!(transition, Transition::AlreadyEnabled),
                "beta OFF while alpha ON" => {
                    assert_eq!(transition, Transition::OtherTableUnaffected)
                }
                "alpha OFF" => assert_eq!(transition, Transition::Disabled),
                "alpha OFF repeated" => assert_eq!(transition, Transition::AlreadyDisabled),
                other => panic!("unexpected SET case {other}"),
            }
            assert!(observed_error(record).is_none());
        }
        assert_eq!(state.active(), None);
    }
}

#[test]
fn rpc_and_prepare_do_not_mutate_caller_state() {
    let retained = fixture("rpc");
    for run in retained["runs"].as_array().unwrap() {
        let mut caller = SessionState::<u64>::default();
        let get = |name: &str| {
            run.as_array()
                .unwrap()
                .iter()
                .find(|r| r["name"] == name)
                .unwrap()
        };
        let rpc_on = parse_setting(get("RPC set alpha ON")["sql"].as_str().unwrap());
        assert!(observed_error(get("RPC set alpha ON")).is_none());
        let mut nested = caller.fork_rpc();
        assert_eq!(nested.apply(&rpc_on, 11), Ok(Transition::Enabled));
        assert_eq!(caller.active(), None);
        let rpc_off = parse_setting(get("RPC set alpha OFF")["sql"].as_str().unwrap());
        let mut nested = caller.fork_rpc();
        assert_eq!(nested.apply(&rpc_off, 11), Ok(Transition::AlreadyDisabled));
        assert_eq!(caller.active(), None);

        let outer_on = parse_setting(get("outer set alpha ON")["sql"].as_str().unwrap());
        caller.apply(&outer_on, 11).unwrap();
        let inner_off = parse_setting(
            get("RPC set alpha OFF inside outer ON")["sql"]
                .as_str()
                .unwrap(),
        );
        let mut nested = caller.fork_rpc();
        assert_eq!(nested.apply(&inner_off, 11), Ok(Transition::Disabled));
        assert_eq!(caller.active(), Some(&11));
        let outer_off = parse_setting(get("outer set alpha OFF")["sql"].as_str().unwrap());
        assert_eq!(caller.apply(&outer_off, 11), Ok(Transition::Disabled));

        // Parsing and storing a prepared SET operation is effect-free. Only
        // execution on a fork may apply it, and that fork is then discarded.
        let prepared = parse_setting(get("prepare SET ON")["sql"].as_str().unwrap());
        assert_eq!(caller.active(), None);
        let mut nested = caller.fork_rpc();
        assert_eq!(nested.apply(&prepared, 11), Ok(Transition::Enabled));
        assert_eq!(caller.active(), None);
    }
}

#[test]
fn resolution_identity_controls_aliases_and_sessions_are_independent() {
    let named = parse_setting("SET IDENTITY_INSERT [dbo].[Alpha] ON");
    let other_spelling = parse_setting("SET IDENTITY_INSERT alpha ON");
    let off = parse_setting("SET IDENTITY_INSERT dbo.alpha OFF");
    let mut left = SessionState::<(u32, u32)>::default();
    let mut right = SessionState::<(u32, u32)>::default();
    assert_eq!(left.apply(&named, (1, 17)), Ok(Transition::Enabled));
    assert_eq!(
        left.apply(&other_spelling, (1, 17)),
        Ok(Transition::AlreadyEnabled)
    );
    assert_eq!(
        right.apply(&other_spelling, (1, 17)),
        Ok(Transition::Enabled)
    );
    let conflict = left.apply(&named, (2, 17)).unwrap_err();
    assert_eq!((conflict.active, conflict.requested), ((1, 17), (2, 17)));
    assert_eq!(left.active(), Some(&(1, 17)));
    assert_eq!(left.apply(&off, (1, 17)), Ok(Transition::Disabled));
    assert_eq!(right.active(), Some(&(1, 17)));
}
