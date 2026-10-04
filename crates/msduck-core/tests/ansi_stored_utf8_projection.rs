//! Original SQL controls and deterministic stored UTF8 projection replay.
use msduck_core::ansi_bytes::{AnsiView, EncodingIdentity};
use msduck_core::ansi_conversion::stored_utf8::{StoredUtf8Error, to_sql_utf16};
use msduck_core::ansi_conversion::{ProjectionError, ProjectionLimits, Resource};
use serde_json::Value;
use std::path::Path;

fn reference() -> Value {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../reference/stored-utf8-projection.json");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn hex(text: &str) -> Vec<u8> {
    assert_eq!(text.len() % 2, 0);
    text.as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

fn project(bytes: &[u8], limits: ProjectionLimits) -> Result<Option<Vec<u16>>, StoredUtf8Error> {
    to_sql_utf16(
        EncodingIdentity::Utf8,
        Some(AnsiView::new(EncodingIdentity::Utf8, bytes, bytes.len()).unwrap()),
        limits,
    )
}

#[test]
fn production_projector_replays_every_original_success_and_failure() {
    let reference = reference();
    let mut checked = 0;
    for run in reference["runs"].as_array().unwrap() {
        for observation in run["observations"].as_array().unwrap() {
            let limits = ProjectionLimits {
                input_bytes: 12401,
                output_bytes: 24802,
            };
            let actual = if let Some(input) = observation["input"].as_str() {
                project(&hex(input), limits)
            } else {
                to_sql_utf16(EncodingIdentity::Utf8, None, limits)
            };
            if !observation["result"]["errors"]
                .as_array()
                .unwrap()
                .is_empty()
            {
                assert_eq!(
                    actual,
                    Err(StoredUtf8Error::InvalidBoundary),
                    "{}",
                    observation["input"]
                );
            } else {
                let expected = &observation["result"]["sets"][1]["rows"][0][0];
                let expected = expected["value"].as_str().map(|bytes| {
                    hex(bytes)
                        .chunks_exact(2)
                        .map(|b| u16::from_le_bytes([b[0], b[1]]))
                        .collect()
                });
                assert_eq!(actual.unwrap(), expected, "{}", observation["input"]);
            }
            checked += 1;
        }
    }
    assert_eq!(checked, 6088);
}

#[test]
fn projector_replays_public_eof_controls_with_frozen_packet_pins() {
    let collector = include_str!("../../../scripts/capture-stored-utf8-projection.mjs");
    let prefix = "export const eofControls = Object.freeze(";
    let start = collector.find(prefix).unwrap() + prefix.len();
    let end = start
        + collector[start..]
            .find(".map(control=>Object.freeze(control)))")
            .unwrap();
    let controls: Vec<(String, Option<String>)> =
        serde_json::from_str(&collector[start..end]).unwrap();
    assert_eq!(controls.len(), 33);
    let limits = ProjectionLimits {
        input_bytes: 32,
        output_bytes: 64,
    };
    for (input, expected) in controls {
        let actual = project(&hex(&input), limits);
        if let Some(expected) = expected {
            let units = hex(&expected)
                .chunks_exact(2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]))
                .collect();
            assert_eq!(actual.unwrap(), Some(units), "{input}");
        } else {
            assert_eq!(actual, Err(StoredUtf8Error::InvalidBoundary), "{input}");
        }
    }
}

#[test]
fn independently_captured_special_suffix_and_malformed_prefix_controls() {
    let limits = ProjectionLimits {
        input_bytes: 32,
        output_bytes: 64,
    };
    for (input, expected) in [
        ("41f380", vec![0x41]),
        ("41e1a0", vec![]),
        ("41efb8", vec![]),
        ("41f3a0", vec![]),
        ("6b060ff3ae", vec![0x6b, 6, 15]),
        ("6b060ff3a0", vec![0x6b, 6]),
        ("eac34ff3a0", vec![0xfffd, 0xfffd]),
        ("eac34ff3ae", vec![0xfffd, 0xfffd, 0x4f]),
        ("41e1a08bef", vec![0x41, 0x180b]),
        ("41efb880efb8", vec![0x41, 0xfe00]),
        ("41e1a08b80", vec![]),
        ("41e1a08be1a08b80", vec![0x41, 0x180b]),
        ("41f3a084", vec![]),
        ("41f3a083", vec![0x41]),
        ("41e1a08eef", vec![0x41]),
        ("41e1a08fef", vec![0x41]),
        ("41efb890ef", vec![0x41]),
        ("41f3a084c0ef", vec![0x41, 0xfffd, 0xfffd]),
        ("41f3a084f5ef", vec![0x41, 0xfffd, 0xfffd]),
        ("41efb8c0ef", vec![0x41]),
        ("41efb8c080", vec![0x41]),
        ("c2c0c2", vec![0xfffd, 0xfffd]),
        ("c2f5f0", vec![0xfffd, 0xfffd]),
    ] {
        assert_eq!(
            project(&hex(input), limits).unwrap(),
            Some(expected),
            "{input}"
        );
    }
    for input in ["049be4", "4180ef", "41c2e1", "41f3c2"] {
        assert_eq!(
            project(&hex(input), limits),
            Err(StoredUtf8Error::InvalidBoundary),
            "{input}"
        );
    }
}

#[test]
fn exact_limits_precede_allocation_and_input_limit_precedes_boundary_failure() {
    assert_eq!(
        project(
            &[0x80],
            ProjectionLimits {
                input_bytes: 0,
                output_bytes: 0
            }
        ),
        Err(StoredUtf8Error::Projection(ProjectionError::Limit {
            resource: Resource::Input,
            requested: 1,
            maximum: 0
        }))
    );
    let bytes = [0x80, 0x42];
    assert_eq!(
        project(
            &bytes,
            ProjectionLimits {
                input_bytes: 2,
                output_bytes: 3
            }
        ),
        Err(StoredUtf8Error::Projection(ProjectionError::Limit {
            resource: Resource::Output,
            requested: 4,
            maximum: 3
        }))
    );
    assert_eq!(
        project(
            &bytes,
            ProjectionLimits {
                input_bytes: 2,
                output_bytes: 4
            }
        )
        .unwrap(),
        Some(vec![0xfffd, 0x42])
    );
    assert_eq!(bytes, [0x80, 0x42]);
    assert_eq!(
        project(
            &[0xc2],
            ProjectionLimits {
                input_bytes: 1,
                output_bytes: 0
            }
        )
        .unwrap(),
        Some(vec![])
    );
}

#[test]
fn null_and_identity_errors_do_not_depend_on_payload_contents() {
    let limits = ProjectionLimits {
        input_bytes: 0,
        output_bytes: 0,
    };
    assert_eq!(to_sql_utf16(EncodingIdentity::Utf8, None, limits), Ok(None));
    assert_eq!(
        to_sql_utf16(EncodingIdentity::Cp1251, None, limits),
        Err(StoredUtf8Error::Projection(
            ProjectionError::UnsupportedSource(EncodingIdentity::Cp1251)
        ))
    );
    let value = AnsiView::new(EncodingIdentity::Cp1252, &[0x80], 1).unwrap();
    assert_eq!(
        to_sql_utf16(EncodingIdentity::Utf8, Some(value), limits),
        Err(StoredUtf8Error::Projection(
            ProjectionError::SourceEncodingMismatch {
                declared: EncodingIdentity::Utf8,
                actual: EncodingIdentity::Cp1252,
            }
        ))
    );
}

#[test]
fn stored_repair_does_not_change_strict_scalar_domain_or_pair_byte_limits() {
    use msduck_core::ansi_conversion::{ProjectionTarget, project as strict_project};
    let malformed = [0xf0, 0x80, 0x80, 0x80];
    let limits = ProjectionLimits {
        input_bytes: 4,
        output_bytes: 6,
    };
    assert_eq!(project(&malformed, limits).unwrap(), Some(vec![0xfffd; 3]));
    let view = AnsiView::new(EncodingIdentity::Utf8, &malformed, 4).unwrap();
    assert!(matches!(
        strict_project(
            EncodingIdentity::Utf8,
            Some(view),
            ProjectionTarget::SqlUtf16,
            limits
        ),
        Err(ProjectionError::InvalidUtf8 { valid_up_to: 0, .. })
    ));
    let maximum_scalar = [0xf4, 0x8f, 0xbf, 0xbf];
    assert_eq!(
        project(
            &maximum_scalar,
            ProjectionLimits {
                input_bytes: 4,
                output_bytes: 3
            }
        ),
        Err(StoredUtf8Error::Projection(ProjectionError::Limit {
            resource: Resource::Output,
            requested: 4,
            maximum: 3
        }))
    );
    assert_eq!(
        project(
            &maximum_scalar,
            ProjectionLimits {
                input_bytes: 4,
                output_bytes: 4
            }
        )
        .unwrap(),
        Some(vec![0xdbff, 0xdfff])
    );
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
