// Include the pure binder until its lib.rs export can be claimed separately.
#[path = "../src/compression.rs"]
mod compression;

use compression::{Argument, BindError, Function, bind};
use msduck_core::{
    character::{CharacterType, Family, Length},
    types::{BinaryType, Type},
};
use serde_json::Value;

fn text(family: Family, length: Length) -> Argument {
    Argument::Known(Type::Character(CharacterType::new(family, length).unwrap()))
}

fn binary(fixed: bool, length: Length) -> Argument {
    Argument::Known(Type::Binary(BinaryType::new(fixed, length).unwrap()))
}

fn case_argument(name: &str) -> Option<(Function, Vec<Argument>)> {
    use Argument::*;
    use Function::*;
    use Length::*;
    let varchar = || text(Family::Varchar, Bounded(20));
    let nvarchar = || text(Family::Nvarchar, Bounded(20));
    let varbinary = || binary(false, Bounded(20));
    let (function, args) = match name {
        "compress varchar"
        | "compress varchar typed"
        | "compress empty varchar"
        | "compress typed null varchar"
        | "rpc compress varchar"
        | "rpc compress empty" => (Compress, vec![varchar()]),
        "compress nvarchar"
        | "compress empty nvarchar"
        | "rpc compress nvarchar"
        | "rpc compress null" => (Compress, vec![nvarchar()]),
        "compress varchar max" | "compress empty varchar max" | "rpc compress large text" => {
            (Compress, vec![text(Family::Varchar, Max)])
        }
        "compress nvarchar max"
        | "compress typed null nvarchar max"
        | "rpc compress nvarchar max" => (Compress, vec![text(Family::Nvarchar, Max)]),
        "compress char padded" => (Compress, vec![text(Family::Char, Bounded(5))]),
        "compress nchar padded" => (Compress, vec![text(Family::Nchar, Bounded(5))]),
        "compress varbinary"
        | "compress empty varbinary"
        | "compress typed null varbinary"
        | "rpc compress varbinary" => (Compress, vec![varbinary()]),
        "compress varbinary max" | "rpc compress varbinary max" | "rpc compress large noise" => {
            (Compress, vec![binary(false, Max)])
        }
        "compress binary padded" => (Compress, vec![binary(true, Bounded(5))]),
        "compress untyped null" => (Compress, vec![UntypedNull]),
        "compress int" | "rpc compress int" => (Compress, vec![Known(Type::Int)]),
        "compress decimal" => (Compress, vec![NumericLiteral]),
        "compress float" => (Compress, vec![Known(Type::Float)]),
        "compress bit" => (Compress, vec![Known(Type::Bit)]),
        "compress datetime" => (Compress, vec![Known(Type::DateTime)]),
        "compress date" => (Compress, vec![Known(Type::Date)]),
        "compress uniqueidentifier" => (Compress, vec![Known(Type::UniqueIdentifier)]),
        "compress xml" => (Compress, vec![Known(Type::Xml)]),
        "compress text" => (Compress, vec![Known(Type::Text)]),
        "compress ntext" => (Compress, vec![Known(Type::Ntext)]),
        "compress image" => (Compress, vec![Known(Type::Image)]),
        "compress sql_variant" => (Compress, vec![Known(Type::Variant)]),
        "compress rowversion-like" => (Compress, vec![Timestamp]),
        "compress json type" => (Compress, vec![Json]),
        "compress no arguments" => (Compress, vec![]),
        "compress two arguments" => (Compress, vec![varchar(), varchar()]),
        "decompress untyped null" => (Decompress, vec![UntypedNull]),
        "decompress typed null" | "decompress empty" => (Decompress, vec![varbinary()]),
        "rpc decompress valid" | "rpc decompress invalid" | "rpc decompress null" => {
            (Decompress, vec![binary(false, Bounded(100))])
        }
        "decompress binary padded" => (Decompress, vec![binary(true, Bounded(30))]),
        "decompress varchar input" => (Decompress, vec![varchar()]),
        "decompress nvarchar input" => (Decompress, vec![nvarchar()]),
        "decompress varchar gzip bytes" => (Decompress, vec![text(Family::Varchar, Max)]),
        "decompress int input" => (Decompress, vec![Known(Type::Int)]),
        "decompress no arguments" => (Decompress, vec![]),
        _ => return None,
    };
    Some((function, args))
}

fn protocol_argument(parameter: &Value) -> Argument {
    let length = match &parameter["options"]["length"] {
        Value::String(value) if value == "max" => Length::Max,
        Value::Number(value) => Length::Bounded(value.as_u64().unwrap() as u16),
        _ => Length::Bounded(1),
    };
    match parameter["type"].as_str().unwrap() {
        "VarChar" => text(Family::Varchar, length),
        "NVarChar" => text(Family::Nvarchar, length),
        "VarBinary" => binary(false, length),
        "Int" => Argument::Known(Type::Int),
        other => panic!("uncaptured protocol declaration {other}"),
    }
}

#[test]
fn retained_type_arity_and_descriptor_cases() {
    let fixture: Value =
        serde_json::from_str(include_str!("../../../reference/compress-decompress.json")).unwrap();
    let mut checked = 0;
    for container in fixture["containers"].as_array().unwrap() {
        for run in container["runs"].as_array().unwrap() {
            for case in run.as_array().unwrap() {
                let name = case["name"].as_str().unwrap();
                let Some((function, args)) = case_argument(name) else {
                    continue;
                };
                if let Some(parameters) = case["parameters"].as_array() {
                    assert_eq!(args, vec![protocol_argument(&parameters[0])], "{name}");
                }
                let result = &case["result"];
                let observed_errors = result["errors"].as_array().unwrap();
                match bind(function, &args) {
                    Err(BindError::Sql(error)) => {
                        assert_eq!(observed_errors.len(), 1, "{name}");
                        let observed = &observed_errors[0];
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
                        if name == "rpc decompress invalid" {
                            assert_eq!(observed_errors[0]["number"], 9826, "{name}");
                        } else {
                            assert!(observed_errors.is_empty(), "{name}");
                        }
                        assert_eq!(
                            bound.result,
                            Type::Binary(BinaryType::new(false, Length::Max).unwrap())
                        );
                        assert!(bound.nullable);
                        assert!(!bound.computed_column_deterministic);
                        assert!(bound.computed_column_precise);
                        let column = &result["sets"][0]["columns"][0];
                        assert_eq!(column["type"], "VarBinary", "{name}");
                        assert_eq!(column["length"], 65535, "{name}");
                        assert_eq!(column["flags"], 33, "{name}");
                    }
                    Err(other) => panic!("{name}: {other:?}"),
                }
                checked += 1;
            }
        }
    }
    assert_eq!(checked, 56 * 4);
}

#[test]
fn prepared_metadata_does_not_depend_on_parameter_values() {
    let fixture: Value =
        serde_json::from_str(include_str!("../../../reference/compress-decompress.json")).unwrap();
    let cases = [
        (
            "prepared compress nvarchar",
            Function::Compress,
            text(Family::Nvarchar, Length::Bounded(4000)),
        ),
        (
            "prepared compress varbinary max",
            Function::Compress,
            binary(false, Length::Max),
        ),
        (
            "prepared decompress",
            Function::Decompress,
            binary(false, Length::Bounded(8000)),
        ),
    ];
    let mut checked = 0;
    for container in fixture["containers"].as_array().unwrap() {
        for run in container["runs"].as_array().unwrap() {
            for (name, function, argument) in cases {
                let case = run
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|item| item["name"] == name)
                    .unwrap();
                assert_eq!(
                    argument,
                    protocol_argument(&case["declarations"][0]),
                    "{name}"
                );
                let bound = bind(function, &[argument]).unwrap();
                assert_eq!(
                    bound.result,
                    Type::Binary(BinaryType::new(false, Length::Max).unwrap())
                );
                let prepared = &case["prepare"]["sets"][0]["columns"][0];
                assert_eq!(prepared["type"], "VarBinary", "{name}");
                assert_eq!(prepared["length"], 65535, "{name}");
                assert_eq!(prepared["flags"], 33, "{name}");
                for execution in case["executions"].as_array().unwrap() {
                    let column = &execution["result"]["sets"][0]["columns"][0];
                    assert_eq!(column, prepared, "{name}");
                    checked += 1;
                }
            }
        }
    }
    assert_eq!(checked, 14 * 4);
}

#[test]
fn uncaptured_families_remain_explicit() {
    for function in [Function::Compress, Function::Decompress] {
        for argument in [Argument::Known(Type::Real), Argument::Uncaptured] {
            assert!(matches!(
                bind(function, &[argument]),
                Err(BindError::Unsupported(_))
            ));
        }
    }
}

#[test]
fn computed_columns_are_precise_but_nondeterministic() {
    let fixture: Value =
        serde_json::from_str(include_str!("../../../reference/compress-decompress.json")).unwrap();
    for container in fixture["containers"].as_array().unwrap() {
        for run in container["runs"].as_array().unwrap() {
            let run = run.as_array().unwrap();
            let persisted = run
                .iter()
                .find(|item| item["name"] == "computed column persisted")
                .unwrap();
            assert_eq!(persisted["result"]["errors"][0]["number"], 4936);
            let properties = run
                .iter()
                .find(|item| item["name"] == "computed column nonpersisted")
                .unwrap();
            for (function, argument) in [
                (
                    Function::Compress,
                    text(Family::Varchar, Length::Bounded(20)),
                ),
                (Function::Decompress, binary(false, Length::Max)),
            ] {
                let bound = bind(function, &[argument]).unwrap();
                assert!(!bound.computed_column_deterministic);
                assert!(bound.computed_column_precise);
            }
            for row in properties["result"]["sets"][0]["rows"].as_array().unwrap() {
                assert_eq!(row[1], 0);
                assert_eq!(row[2], 1);
            }
        }
    }
}
