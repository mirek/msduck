use msduck_core::ansi_bytes::{AnsiView, EncodingIdentity};
use msduck_core::ansi_conversion::{
    ProjectedValue, ProjectionError, ProjectionLimits, ProjectionTarget, Resource, project,
};
use serde_json::Value;

fn native(
    input: Option<&[u8]>,
    target: EncodingIdentity,
    input_limit: usize,
    output_limit: usize,
) -> Result<Option<Vec<u8>>, ProjectionError> {
    project(
        EncodingIdentity::Utf8,
        input.map(|bytes| AnsiView::new(EncodingIdentity::Utf8, bytes, bytes.len()).unwrap()),
        ProjectionTarget::Native(target),
        ProjectionLimits {
            input_bytes: input_limit,
            output_bytes: output_limit,
        },
    )
    .map(|value| {
        value.map(|value| match value {
            ProjectedValue::Native(value) => {
                assert_eq!(value.view().encoding(), target);
                value.view().bytes().to_vec()
            }
            ProjectedValue::SqlUtf16(_) => panic!("unexpected Unicode target"),
        })
    })
}
fn hex(value: &str) -> Vec<u8> {
    assert_eq!(value.len() % 2, 0);
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

#[test]
fn retained_source_collation_controls_match_each_original_native_byte() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../reference/bulk-character-collation-precedence.json"
    ))
    .unwrap();
    assert_eq!(fixture["runs"].as_array().unwrap().len(), 4);
    let mut observations = 0;
    let mut rows = 0;
    for run in fixture["runs"].as_array().unwrap() {
        for observation in run["observations"].as_array().unwrap() {
            let case = &observation["case"];
            let source = if case["declared"].is_null() {
                &case["default"]
            } else {
                &case["declared"]
            };
            if source != "utf8" {
                continue;
            }
            let target = match case["target"].as_str().unwrap() {
                "cp1251" => EncodingIdentity::Cp1251,
                "cp1252" => EncodingIdentity::Cp1252,
                _ => continue,
            };
            let inputs = observation["input"].as_array().unwrap();
            let retained = observation["readback"]["result"]["sets"][1]["rows"]
                .as_array()
                .unwrap();
            assert_eq!(inputs.len(), retained.len());
            observations += 1;
            for (input, row) in inputs.iter().zip(retained) {
                assert_eq!(input["id"], row[0]);
                let bytes = input["valueHex"].as_str().map(hex);
                let expected = if row[2].is_null() {
                    None
                } else {
                    assert_eq!(row[2]["kind"], "binary");
                    Some(hex(row[2]["value"].as_str().unwrap()))
                };
                assert_eq!(
                    native(
                        bytes.as_deref(),
                        target,
                        bytes.as_ref().map_or(0, Vec::len),
                        expected.as_ref().map_or(0, Vec::len)
                    )
                    .unwrap(),
                    expected
                );
                rows += 1;
            }
        }
    }
    assert_eq!(observations, 72);
    assert_eq!(rows, 360);
}

#[test]
fn mixed_scalars_preserve_native_best_fit_and_two_byte_supplementary_replacement() {
    let input = "\0éГ🦆 A".as_bytes();
    let original = input.to_vec();
    for (target, expected) in [
        (
            EncodingIdentity::Cp1251,
            &[0, b'e', 0xc3, b'?', b'?', b' ', b'A'][..],
        ),
        (
            EncodingIdentity::Cp1252,
            &[0, 0xe9, b'?', b'?', b'?', b' ', b'A'][..],
        ),
    ] {
        assert_eq!(
            native(Some(input), target, input.len(), expected.len()).unwrap(),
            Some(expected.to_vec())
        );
        assert_eq!(
            native(Some(input), target, input.len(), expected.len() - 1),
            Err(ProjectionError::Limit {
                resource: Resource::Output,
                requested: expected.len(),
                maximum: expected.len() - 1
            })
        );
        assert_eq!(
            native(Some(input), target, input.len() - 1, 0),
            Err(ProjectionError::Limit {
                resource: Resource::Input,
                requested: input.len(),
                maximum: input.len() - 1
            })
        );
        assert_eq!(native(None, target, 0, 0).unwrap(), None);
        assert_eq!(native(Some(b""), target, 0, 0).unwrap(), Some(vec![]));
        assert_eq!(
            native(Some(b" A "), target, 3, 3).unwrap(),
            Some(b" A ".to_vec())
        );
    }
    assert_eq!(input, original);
}

#[test]
fn malformed_source_remains_strict_and_reports_original_offsets_before_output_limits() {
    for input in [
        &b"A\xff"[..],
        &b"A\xe2\x82"[..],
        &b"A\xed\xa0\x80"[..],
        &b"A\xc0\xaf"[..],
    ] {
        let error = std::str::from_utf8(input).unwrap_err();
        for target in [EncodingIdentity::Cp1251, EncodingIdentity::Cp1252] {
            assert_eq!(
                native(Some(input), target, input.len(), 0),
                Err(ProjectionError::InvalidUtf8 {
                    valid_up_to: error.valid_up_to(),
                    error_len: error.error_len()
                })
            );
        }
    }
}

#[test]
fn unsupported_target_precedes_null_and_identity_precedes_payload_validation() {
    for input in [None, Some(&b""[..]), Some(&b"\xff"[..])] {
        assert_eq!(
            native(input, EncodingIdentity::Opaque(1251), 0, 0),
            Err(ProjectionError::UnsupportedTarget(
                EncodingIdentity::Opaque(1251)
            ))
        );
    }
    for target in [EncodingIdentity::Cp1251, EncodingIdentity::Cp1252] {
        for actual in [
            EncodingIdentity::Cp1251,
            EncodingIdentity::Cp1252,
            EncodingIdentity::Opaque(65001),
        ] {
            assert_eq!(
                project(
                    EncodingIdentity::Utf8,
                    Some(AnsiView::new(actual, &[0xff], 1).unwrap()),
                    ProjectionTarget::Native(target),
                    ProjectionLimits {
                        input_bytes: 0,
                        output_bytes: 0
                    }
                ),
                Err(ProjectionError::SourceEncodingMismatch {
                    declared: EncodingIdentity::Utf8,
                    actual
                })
            );
        }
    }
}
