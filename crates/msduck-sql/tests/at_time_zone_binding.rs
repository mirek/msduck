// The shared lib.rs export is reserved; compile this pure module by path.
#[path = "../src/at_time_zone.rs"]
mod at_time_zone;

use at_time_zone::{BindError, bind_timestamp};
use msduck_core::types::{Scale, Type};
use serde_json::Value;

fn scale(value: u8) -> Scale {
    Scale::new(value).unwrap()
}

#[test]
fn retained_result_descriptors_and_input_errors() {
    let fixture: Value =
        serde_json::from_str(include_str!("../../../reference/at-time-zone.json")).unwrap();
    let types = [
        ("datetime2 UTC", Type::DateTime2(scale(7))),
        ("datetime UTC", Type::DateTime),
        ("smalldatetime UTC", Type::SmallDateTime),
        ("datetime2 named zone", Type::DateTime2(scale(7))),
        (
            "datetimeoffset changes zone",
            Type::DateTimeOffset(scale(7)),
        ),
        ("chained zone conversion", Type::DateTimeOffset(scale(7))),
        ("typed NULL input", Type::DateTime2(scale(7))),
        ("NULL zone", Type::DateTime2(scale(7))),
        ("invalid zone", Type::DateTime2(scale(7))),
        ("empty result metadata", Type::DateTime2(scale(7))),
        ("unsupported date input", Type::Date),
        ("unsupported integer input", Type::Int),
    ];
    for (name, input) in types {
        let entry = fixture["results"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["name"] == name)
            .unwrap();
        let reference = &entry["reference"];
        match bind_timestamp(input) {
            Ok(bound) => {
                let column = &reference["sets"][0]["columns"][0];
                assert_eq!(column["type"], "DateTimeOffset", "{name}");
                assert_eq!(column["scale"], bound.result_scale(), "{name}");
                assert!(bound.nullable, "{name}");
                let expected_flags = if name == "invalid zone" { 33 } else { 1 };
                assert_eq!(column["flags"], expected_flags, "{name}");
                if name != "invalid zone" {
                    assert!(reference["errors"].as_array().unwrap().is_empty(), "{name}");
                }
            }
            Err(BindError::Sql(error)) => {
                assert!(reference["sets"].as_array().unwrap().is_empty(), "{name}");
                let captured = &reference["errors"][0];
                assert_eq!(captured["number"], error.number, "{name}");
                assert_eq!(captured["state"], error.state, "{name}");
                assert_eq!(captured["class"], error.severity, "{name}");
                assert_eq!(captured["message"], error.message, "{name}");
            }
            Err(BindError::Unsupported(reason)) => panic!("captured {name}: {reason}"),
        }
    }
    for run in fixture["prepared"]["runs"].as_array().unwrap() {
        let bound = bind_timestamp(Type::DateTime2(scale(7))).unwrap();
        assert_eq!(
            run["reference"]["sets"][0]["columns"][0]["scale"],
            bound.result_scale()
        );
        assert_eq!(
            run["reference"]["sets"][0]["columns"][0]["type"],
            "DateTimeOffset"
        );
    }
}

impl at_time_zone::BoundExpression {
    fn result_scale(self) -> u8 {
        match self.result {
            Type::DateTimeOffset(scale) => scale.get(),
            _ => unreachable!(),
        }
    }
}

#[test]
fn uncaptured_inputs_are_explicitly_unresolved_and_scales_are_preserved() {
    for scale_value in 0..=7 {
        let scale = scale(scale_value);
        for input in [Type::DateTime2(scale), Type::DateTimeOffset(scale)] {
            assert_eq!(
                bind_timestamp(input).unwrap().result,
                Type::DateTimeOffset(scale)
            );
        }
    }
    for input in [Type::Time(scale(7)), Type::BigInt, Type::Variant] {
        assert!(matches!(
            bind_timestamp(input),
            Err(BindError::Unsupported(_))
        ));
    }
}
