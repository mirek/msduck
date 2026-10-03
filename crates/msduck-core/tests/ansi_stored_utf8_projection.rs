//! Original SQL controls for the pending stored UTF8 projector.
//! These tests preserve errors and incomplete-tail behavior; they do not assert
//! that the production stored decoder has been implemented.
use serde_json::Value;
use std::path::Path;

fn reference() -> Value {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../reference/stored-utf8-projection.json");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[test]
fn native_identity_survives_every_original_projection_error() {
    let reference = reference();
    let runs = reference["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 4);
    for run in runs {
        let observations = run["observations"].as_array().unwrap();
        assert_eq!(observations.len(), 1522);
        let mut errors = 0;
        for observation in observations {
            let result = &observation["result"];
            let sets = result["sets"].as_array().unwrap();
            assert_eq!(sets.len(), 2);
            assert_eq!(sets[0]["rows"].as_array().unwrap().len(), 1);
            let native = &sets[0]["rows"][0][0];
            if observation["input"].is_null() {
                assert!(native.is_null());
            } else {
                assert_eq!(native["kind"], "binary");
                assert_eq!(native["value"], observation["input"]);
            }
            let diagnostics = result["errors"].as_array().unwrap();
            if diagnostics.is_empty() {
                assert_eq!(sets[1]["rows"].as_array().unwrap().len(), 1);
            } else {
                errors += 1;
                assert_eq!(diagnostics.len(), 1);
                assert_eq!(diagnostics[0]["number"], 9833);
                assert_eq!(diagnostics[0]["state"], 2);
                assert_eq!(diagnostics[0]["class"], 16);
                assert!(sets[1]["rows"].as_array().unwrap().is_empty());
            }
            assert!(result["info"].as_array().unwrap().is_empty());
        }
        assert_eq!(errors, 153);
    }
}

#[test]
fn incomplete_eof_and_invalid_endings_are_distinct_original_outcomes() {
    let reference = reference();
    for run in reference["runs"].as_array().unwrap() {
        let observations = run["observations"].as_array().unwrap();
        for input in ["c2", "e0a0", "e080", "f09080", "f49080"] {
            let observation = observations.iter().find(|o| o["input"] == input).unwrap();
            assert!(
                observation["result"]["errors"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
            let units = &observation["result"]["sets"][1]["rows"][0][0];
            assert_eq!(units["kind"], "binary");
            assert_eq!(units["value"], "");
        }
        for input in ["80", "bf", "c0af", "f5808080"] {
            let observation = observations.iter().find(|o| o["input"] == input).unwrap();
            assert_eq!(observation["result"]["errors"][0]["number"], 9833);
        }
        let safe_suffix = observations.iter().find(|o| o["input"] == "8042").unwrap();
        assert_eq!(
            safe_suffix["result"]["sets"][1]["rows"][0][0]["value"],
            "fdff4200"
        );
    }
}

#[test]
fn null_and_empty_preserve_typed_binary_metadata() {
    let reference = reference();
    for run in reference["runs"].as_array().unwrap() {
        let observations = run["observations"].as_array().unwrap();
        for (input, empty) in [(Value::Null, false), (Value::String(String::new()), true)] {
            let observation = observations.iter().find(|o| o["input"] == input).unwrap();
            for set in observation["result"]["sets"].as_array().unwrap() {
                assert_eq!(set["columns"][0]["type"], "VarBinary");
                assert_eq!(set["columns"][0]["length"], 65535);
                let cell = &set["rows"][0][0];
                if empty {
                    assert_eq!(cell["kind"], "binary");
                    assert_eq!(cell["value"], "");
                } else {
                    assert!(cell.is_null());
                }
            }
        }
    }
}
