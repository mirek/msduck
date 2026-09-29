// Include the pure binder until a separately claimed crate export is free.
#[path = "../src/hashbytes.rs"]
mod hashbytes;

use hashbytes::{Argument, BindError, bind};
use msduck_core::{
    character::{CharacterType, Family, Length},
    types::{BinaryType, Type},
};
use serde_json::Value;

fn text(family: Family, length: Length) -> Argument {
    Argument::Known(Type::Character(CharacterType::new(family, length).unwrap()))
}

fn binary(length: Length) -> Argument {
    Argument::Known(Type::Binary(BinaryType::new(false, length).unwrap()))
}

fn varchar() -> Argument {
    text(Family::Varchar, Length::Bounded(20))
}

fn protocol_argument(parameter: &Value) -> Argument {
    let length = match &parameter["options"]["length"] {
        Value::String(value) if value == "max" => Length::Max,
        Value::Number(value) if value.as_u64().unwrap() > 8000 => Length::Max,
        Value::Number(value) => Length::Bounded(value.as_u64().unwrap() as u16),
        _ => Length::Bounded(1),
    };
    match parameter["type"].as_str().unwrap() {
        "VarChar" => text(Family::Varchar, length),
        "NVarChar" => text(Family::Nvarchar, length),
        "VarBinary" => binary(length),
        "Int" => Argument::Known(Type::Int),
        other => panic!("uncaptured protocol type {other}"),
    }
}

fn arguments(case: &Value) -> Option<Vec<Argument>> {
    let name = case["name"].as_str().unwrap();
    if name.starts_with("rpc hashbytes ") {
        return Some(
            case["parameters"]
                .as_array()
                .unwrap()
                .iter()
                .map(protocol_argument)
                .collect(),
        );
    }
    if name.starts_with("hashbytes ") && name.split_whitespace().count() == 3 {
        match name.split_whitespace().last().unwrap() {
            "varchar" => return Some(vec![varchar(), varchar()]),
            "nvarchar" => {
                return Some(vec![varchar(), text(Family::Nvarchar, Length::Bounded(20))]);
            }
            "varbinary" => return Some(vec![varchar(), binary(Length::Bounded(20))]),
            _ => {}
        }
    }
    let args = match name {
        "hashbytes unicode algorithm" => {
            vec![text(Family::Nvarchar, Length::Bounded(20)), varchar()]
        }
        "hashbytes integer algorithm" => vec![Argument::Known(Type::Int), varchar()],
        "hashbytes untyped null algorithm" => vec![Argument::UntypedNull, varchar()],
        "hashbytes untyped null input" => vec![varchar(), Argument::UntypedNull],
        "hashbytes integer input" => vec![varchar(), Argument::Known(Type::Int)],
        "hashbytes decimal input" => vec![varchar(), Argument::NumericLiteral],
        "hashbytes datetime input" => vec![varchar(), Argument::Known(Type::DateTime)],
        "hashbytes uniqueidentifier input" => {
            vec![varchar(), Argument::Known(Type::UniqueIdentifier)]
        }
        "hashbytes xml input" => vec![varchar(), Argument::Known(Type::Xml)],
        "hashbytes text input" => vec![varchar(), Argument::Known(Type::Text)],
        "hashbytes one argument" => vec![varchar()],
        "hashbytes three arguments" => vec![varchar(), varchar(), varchar()],
        "hashbytes null nvarchar input"
        | "hashbytes supplementary character"
        | "hashbytes nvarchar max"
        | "hashbytes nvarchar 10000 bytes" => vec![varchar(), text(Family::Nvarchar, Length::Max)],
        "hashbytes varbinary max" | "hashbytes varbinary 10000 bytes" => {
            vec![varchar(), binary(Length::Max)]
        }
        "hashbytes char padding" => vec![varchar(), text(Family::Char, Length::Bounded(5))],
        "hashbytes null algorithm"
        | "hashbytes invalid algorithm null input"
        | "hashbytes null varchar input"
        | "hashbytes lowercase algorithm"
        | "hashbytes algorithm trailing space"
        | "hashbytes algorithm leading space"
        | "hashbytes invalid algorithm"
        | "hashbytes sha2_384 algorithm"
        | "hashbytes empty algorithm"
        | "hashbytes empty varchar"
        | "hashbytes case sensitivity"
        | "hashbytes trailing spaces"
        | "hashbytes cp1252 character"
        | "hashbytes utf8 collation"
        | "hashbytes collation clause"
        | "hashbytes varchar max"
        | "hashbytes varchar 8000 bytes"
        | "hashbytes varchar 10000 bytes"
        | "hashbytes md5 10000 bytes" => vec![varchar(), varchar()],
        _ => return None,
    };
    Some(args)
}

#[test]
fn retained_batch_and_rpc_diagnostics_and_metadata() {
    let fixture: Value =
        serde_json::from_str(include_str!("../../../reference/hashbytes-checksum.json")).unwrap();
    let mut checked = 0;
    for container in fixture["containers"].as_array().unwrap() {
        for run in container["runs"].as_array().unwrap() {
            for case in run.as_array().unwrap() {
                let Some(args) = arguments(case) else {
                    continue;
                };
                let name = case["name"].as_str().unwrap();
                let result = &case["result"];
                let errors = result["errors"].as_array().unwrap();
                match bind(&args) {
                    Err(BindError::Sql(error)) => {
                        assert_eq!(errors.len(), 1, "{name}");
                        let observed = &errors[0];
                        assert_eq!(
                            error.number,
                            observed["number"].as_i64().unwrap() as i32,
                            "{name}"
                        );
                        assert_eq!(
                            error.state,
                            observed["state"].as_u64().unwrap() as u8,
                            "{name}"
                        );
                        assert_eq!(
                            error.severity,
                            observed["class"].as_u64().unwrap() as u8,
                            "{name}"
                        );
                        assert_eq!(
                            error.message,
                            observed["message"].as_str().unwrap(),
                            "{name}"
                        );
                        assert!(result["sets"].as_array().unwrap().is_empty(), "{name}");
                    }
                    Ok(bound) => {
                        assert!(errors.is_empty(), "{name}");
                        assert_eq!(
                            bound.result,
                            Type::Binary(BinaryType::new(false, Length::Bounded(8000)).unwrap())
                        );
                        assert!(bound.nullable);
                        let column = &result["sets"][0]["columns"][0];
                        assert_eq!(column["type"], "VarBinary", "{name}");
                        assert_eq!(column["length"], 8000, "{name}");
                        assert_eq!(column["flags"], 33, "{name}");
                    }
                    Err(other) => panic!("{name}: {other:?}"),
                }
                checked += 1;
            }
        }
    }
    assert_eq!(checked, 70 * 4);
}

#[test]
fn prepared_descriptor_ignores_algorithm_and_input_values() {
    let fixture: Value =
        serde_json::from_str(include_str!("../../../reference/hashbytes-checksum.json")).unwrap();
    let args = [varchar(), text(Family::Nvarchar, Length::Bounded(20))];
    let bound = bind(&args).unwrap();
    assert_eq!(
        bound.result,
        Type::Binary(BinaryType::new(false, Length::Bounded(8000)).unwrap())
    );
    let mut executions = 0;
    for container in fixture["containers"].as_array().unwrap() {
        for run in container["runs"].as_array().unwrap() {
            let case = run
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["name"] == "prepared hashbytes and checksums")
                .unwrap();
            let prepared = &case["prepared"]["prepare"]["sets"][0]["columns"][0];
            assert_eq!(prepared["type"], "VarBinary");
            assert_eq!(prepared["length"], 8000);
            assert_eq!(prepared["flags"], 33);
            for execution in case["prepared"]["executions"].as_array().unwrap() {
                assert_eq!(&execution["result"]["sets"][0]["columns"][0], prepared);
                executions += 1;
            }
        }
    }
    assert_eq!(executions, 5 * 4);
}

#[test]
fn uncaptured_families_are_not_guessed() {
    for arguments in [
        vec![Argument::Uncaptured, varchar()],
        vec![varchar(), Argument::Known(Type::Real)],
        vec![Argument::UntypedNull, Argument::UntypedNull],
        vec![varchar(), binary(Length::Bounded(1)), varchar()],
    ] {
        assert!(matches!(bind(&arguments), Err(BindError::Unsupported(_))));
    }
}
