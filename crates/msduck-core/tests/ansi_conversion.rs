use msduck_core::ansi_bytes::{AnsiView, EncodingIdentity};
use msduck_core::ansi_conversion::{
    ProjectedValue, ProjectionError, ProjectionLimits, ProjectionTarget, Resource, project,
};
use serde_json::Value;

const CONVERSION: &str = include_str!("../../../reference/bulk-character-conversion.json");
const ENCODING: &str = include_str!("../../../reference/bulk-character-encoding.json");
const CP1252: &str = include_str!("../../../reference/bulk-character-cp1252.json");

fn object_end(text: &str, start: usize) -> usize {
    assert_eq!(text.as_bytes()[start], b'{');
    let (mut depth, mut quoted, mut escaped) = (0, false, false);
    for (offset, &byte) in text.as_bytes()[start..].iter().enumerate() {
        if quoted {
            if escaped {
                escaped = false
            } else if byte == b'\\' {
                escaped = true
            } else if byte == b'"' {
                quoted = false
            }
        } else {
            match byte {
                b'"' => quoted = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return start + offset + 1;
                    }
                }
                _ => {}
            }
        }
    }
    panic!("unterminated retained fixture object")
}

fn observations(reference: &str, name: &str) -> Vec<Value> {
    // Unrelated captured diagnostics legitimately contain isolated UTF16
    // surrogates which serde_json::Value cannot represent. Extract complete
    // selected observations from the original JSON without replacing escapes,
    // rows, errors or any raw bytes. These admitted observations contain scalar
    // strings; exact SQL/native semantics come from their binary projections.
    let mut selected = Vec::new();
    let mut position = 0;
    while let Some(offset) = reference[position..].find("{\"case\":") {
        let start = position + offset;
        let end = object_end(reference, start);
        let case_start = start + "{\"case\":".len();
        let case_end = object_end(reference, case_start);
        let case: Value = serde_json::from_str(&reference[case_start..case_end]).unwrap();
        if case["name"] == name {
            selected.push(serde_json::from_str(&reference[start..end]).unwrap());
        }
        position = end;
    }
    assert_eq!(selected.len(), 4, "all four retained runs of {name}");
    selected
}

fn hex(text: &str) -> Vec<u8> {
    assert_eq!(text.len() % 2, 0);
    text.as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}
fn binary(value: &Value) -> Option<Vec<u8>> {
    if value.is_null() {
        return None;
    }
    assert_eq!(value["kind"], "binary");
    Some(hex(value["value"].as_str().unwrap()))
}
fn sql_units(bytes: &[u8]) -> Vec<u16> {
    assert_eq!(bytes.len() % 2, 0);
    bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect()
}
fn run_projection(
    input: Option<&[u8]>,
    target: ProjectionTarget,
    maximum: usize,
) -> Option<ProjectedValue> {
    let view =
        input.map(|bytes| AnsiView::new(EncodingIdentity::Cp1251, bytes, bytes.len()).unwrap());
    project(
        EncodingIdentity::Cp1251,
        view,
        target,
        ProjectionLimits {
            input_bytes: input.map_or(0, <[u8]>::len),
            output_bytes: maximum,
        },
    )
    .unwrap()
}
fn assert_native(input: Option<&[u8]>, target: EncodingIdentity, expected: Option<&[u8]>) {
    let actual = run_projection(
        input,
        ProjectionTarget::Native(target),
        expected.map_or(0, <[u8]>::len),
    );
    match (actual, expected) {
        (None, None) => {}
        (Some(ProjectedValue::Native(value)), Some(expected)) => {
            assert_eq!(value.view().encoding(), target);
            assert_eq!(value.view().bytes(), expected);
        }
        other => panic!("unexpected native projection {other:?}"),
    }
}
fn assert_utf16(input: Option<&[u8]>, expected: Option<&[u8]>) {
    let actual = run_projection(
        input,
        ProjectionTarget::SqlUtf16,
        expected.map_or(0, <[u8]>::len),
    );
    match (actual, expected) {
        (None, None) => {}
        (Some(ProjectedValue::SqlUtf16(value)), Some(expected)) => {
            assert_eq!(value, sql_units(expected))
        }
        other => panic!("unexpected UTF16 projection {other:?}"),
    }
}

#[test]
fn all_256_native_bytes_match_all_four_sql_server_projections() {
    let native = observations(CONVERSION, "cp1251-every-byte-cp1251");
    let western = observations(CONVERSION, "cp1251-every-byte-cp1252");
    let unicode = observations(CONVERSION, "cp1251-every-byte-unicode");
    for ((native, western), unicode) in native.iter().zip(&western).zip(&unicode) {
        let inputs = native["input"].as_array().unwrap();
        let native_rows = native["readback"]["result"]["sets"][1]["rows"]
            .as_array()
            .unwrap();
        let western_rows = western["readback"]["result"]["sets"][1]["rows"]
            .as_array()
            .unwrap();
        let unicode_rows = unicode["readback"]["result"]["sets"][1]["rows"]
            .as_array()
            .unwrap();
        assert_eq!(inputs.len(), 258);
        assert_eq!(native_rows.len(), 258);
        assert_eq!(western_rows.len(), 258);
        assert_eq!(unicode_rows.len(), 258);
        for (i, input) in inputs.iter().enumerate() {
            let bytes = input["valueHex"].as_str().map(hex);
            if i >= 2 {
                assert_eq!(bytes.as_deref(), Some(&[u8::try_from(i - 2).unwrap()][..]))
            }
            for rows in [native_rows, western_rows, unicode_rows] {
                assert_eq!(rows[i][0], input["id"])
            }
            let native_bytes = binary(&native_rows[i][2]);
            let western_bytes = binary(&western_rows[i][2]);
            let unicode_bytes = binary(&unicode_rows[i][4]);
            assert_eq!(binary(&native_rows[i][4]), unicode_bytes);
            assert_native(
                bytes.as_deref(),
                EncodingIdentity::Cp1251,
                native_bytes.as_deref(),
            );
            assert_native(
                bytes.as_deref(),
                EncodingIdentity::Cp1252,
                western_bytes.as_deref(),
            );
            assert_utf16(bytes.as_deref(), unicode_bytes.as_deref());
            // UTF8 is the standard encoding of the observed SQL units, not of
            // the client's possibly lossy display of the original ANSI bytes.
            let utf8 = unicode_bytes
                .as_deref()
                .map(|bytes| String::from_utf16(&sql_units(bytes)).unwrap().into_bytes());
            assert_native(bytes.as_deref(), EncodingIdentity::Utf8, utf8.as_deref());
        }
    }
}

#[test]
fn earlier_mixed_and_max_native_probes_match_without_client_decoding() {
    for (domain, target) in [
        ("same", ProjectionTarget::Native(EncodingIdentity::Cp1251)),
        ("cp1252", ProjectionTarget::Native(EncodingIdentity::Cp1252)),
        ("unicode", ProjectionTarget::SqlUtf16),
    ] {
        for width in ["bounded", "max"] {
            for o in observations(ENCODING, &format!("CP1251-{domain}-{width}")) {
                let rows = o["readback"]["result"]["sets"][1]["rows"]
                    .as_array()
                    .unwrap();
                let inputs = o["input"].as_array().unwrap();
                assert_eq!(rows.len(), inputs.len());
                for (row, input) in rows.iter().zip(inputs) {
                    assert_eq!(row[0], input["id"]);
                    let source = input["valueHex"].as_str().map(hex);
                    let expected = binary(&row[2]);
                    match target {
                        ProjectionTarget::Native(encoding) => {
                            assert_native(source.as_deref(), encoding, expected.as_deref())
                        }
                        ProjectionTarget::SqlUtf16 => {
                            assert_utf16(source.as_deref(), expected.as_deref())
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn captured_cp1251_utf8_max_and_copyright_expansion_match_exact_bytes() {
    for name in [
        "cp1251-to-utf8-max-fragmented",
        "cp1251-to-utf8-copyright-width2",
    ] {
        for o in observations(CONVERSION, name) {
            let rows = o["readback"]["result"]["sets"][1]["rows"]
                .as_array()
                .unwrap();
            let inputs = o["input"].as_array().unwrap();
            assert_eq!(rows.len(), inputs.len());
            for (row, input) in rows.iter().zip(inputs) {
                let source = input["valueHex"].as_str().map(hex);
                let expected = binary(&row[2]);
                assert_native(
                    source.as_deref(),
                    EncodingIdentity::Utf8,
                    expected.as_deref(),
                );
                assert_utf16(source.as_deref(), binary(&row[4]).as_deref());
            }
        }
    }
}

#[test]
fn undefined_byte_sql_control_unit_is_not_the_clients_replacement_character() {
    for o in observations(CONVERSION, "cp1251-every-byte-cp1251") {
        let row = &o["readback"]["result"]["sets"][1]["rows"][154];
        assert_eq!(row[1], "\u{fffd}");
        assert_eq!(row[4]["value"], "9800");
        assert_native(Some(&[0x98]), EncodingIdentity::Cp1251, Some(&[0x98]));
        assert_native(Some(&[0x98]), EncodingIdentity::Cp1252, Some(b"?"));
        assert_native(Some(&[0x98]), EncodingIdentity::Utf8, Some(&[0xc2, 0x98]));
        assert_utf16(Some(&[0x98]), Some(&[0x98, 0]));
    }
}

#[test]
fn unsupported_declarations_are_rejected_before_null_empty_or_limits() {
    let limits = ProjectionLimits {
        input_bytes: 0,
        output_bytes: 0,
    };
    for source in [
        EncodingIdentity::Opaque(1251),
        EncodingIdentity::Opaque(1252),
        EncodingIdentity::Opaque(65001),
    ] {
        for input in [None, Some(AnsiView::new(source, &[], 0).unwrap())] {
            assert_eq!(
                project(source, input, ProjectionTarget::SqlUtf16, limits),
                Err(ProjectionError::UnsupportedSource(source))
            );
        }
    }
    for target in [
        EncodingIdentity::Opaque(1252),
        EncodingIdentity::Opaque(65001),
    ] {
        for input in [
            None,
            Some(AnsiView::new(EncodingIdentity::Cp1251, &[], 0).unwrap()),
        ] {
            assert_eq!(
                project(
                    EncodingIdentity::Cp1251,
                    input,
                    ProjectionTarget::Native(target),
                    limits
                ),
                Err(ProjectionError::UnsupportedTarget(target))
            );
        }
    }
}

#[test]
fn carrier_identity_must_match_the_explicit_source_declaration() {
    for actual in [
        EncodingIdentity::Cp1252,
        EncodingIdentity::Utf8,
        EncodingIdentity::Opaque(1251),
    ] {
        let input = AnsiView::new(actual, &[0x98], 1).unwrap();
        assert_eq!(
            project(
                EncodingIdentity::Cp1251,
                Some(input),
                ProjectionTarget::SqlUtf16,
                ProjectionLimits {
                    input_bytes: 0,
                    output_bytes: 0
                }
            ),
            Err(ProjectionError::SourceEncodingMismatch {
                declared: EncodingIdentity::Cp1251,
                actual
            })
        );
    }
}

#[test]
fn null_and_empty_remain_distinct_at_zero_limits_for_every_admitted_target() {
    for target in [
        ProjectionTarget::SqlUtf16,
        ProjectionTarget::Native(EncodingIdentity::Cp1251),
        ProjectionTarget::Native(EncodingIdentity::Cp1252),
        ProjectionTarget::Native(EncodingIdentity::Utf8),
    ] {
        assert_eq!(run_projection(None, target, 0), None);
        let empty = run_projection(Some(&[]), target, 0).unwrap();
        match empty {
            ProjectedValue::Native(bytes) => assert!(bytes.view().bytes().is_empty()),
            ProjectedValue::SqlUtf16(units) => assert!(units.is_empty()),
        }
    }
}

#[test]
fn exact_byte_limits_preflight_expansion_and_utf16_without_mutating_input() {
    let input = [0, 0x98, 0xa9, 0xb9, 0xff];
    let view = AnsiView::new(EncodingIdentity::Cp1251, &input, input.len()).unwrap();
    for (target, output_bytes) in [
        (ProjectionTarget::Native(EncodingIdentity::Cp1251), 5),
        (ProjectionTarget::Native(EncodingIdentity::Cp1252), 5),
        (ProjectionTarget::Native(EncodingIdentity::Utf8), 10),
        (ProjectionTarget::SqlUtf16, 10),
    ] {
        assert!(
            project(
                EncodingIdentity::Cp1251,
                Some(view),
                target,
                ProjectionLimits {
                    input_bytes: 5,
                    output_bytes
                }
            )
            .is_ok()
        );
        assert_eq!(
            project(
                EncodingIdentity::Cp1251,
                Some(view),
                target,
                ProjectionLimits {
                    input_bytes: 4,
                    output_bytes
                }
            ),
            Err(ProjectionError::Limit {
                resource: Resource::Input,
                requested: 5,
                maximum: 4
            })
        );
        assert_eq!(
            project(
                EncodingIdentity::Cp1251,
                Some(view),
                target,
                ProjectionLimits {
                    input_bytes: 5,
                    output_bytes: output_bytes - 1
                }
            ),
            Err(ProjectionError::Limit {
                resource: Resource::Output,
                requested: output_bytes,
                maximum: output_bytes - 1
            })
        );
        assert_eq!(view.bytes(), [0, 0x98, 0xa9, 0xb9, 0xff]);
    }
}

#[test]
fn resource_limit_is_not_a_sql_width_or_padding_policy() {
    assert_native(Some(&[0xa9]), EncodingIdentity::Utf8, Some(&[0xc2, 0xa9]));
    let input = AnsiView::new(EncodingIdentity::Cp1251, &[0xa9], 1).unwrap();
    assert_eq!(
        project(
            EncodingIdentity::Cp1251,
            Some(input),
            ProjectionTarget::Native(EncodingIdentity::Utf8),
            ProjectionLimits {
                input_bytes: 1,
                output_bytes: 1
            }
        ),
        Err(ProjectionError::Limit {
            resource: Resource::Output,
            requested: 2,
            maximum: 1
        })
    );
    // SQL's captured VARCHAR(1) BulkLoad result is empty. This kernel reports
    // resource failure instead of silently implementing that width policy.
    for o in observations(CONVERSION, "cp1251-to-utf8-copyright-width1") {
        assert_eq!(
            o["readback"]["result"]["sets"][1]["rows"][3][2]["value"],
            ""
        );
    }
}

fn assert_cp1252_projection(
    input: Option<&[u8]>,
    target: ProjectionTarget,
    expected: Option<&[u8]>,
) {
    let actual = project(
        EncodingIdentity::Cp1252,
        AnsiView::nullable(
            EncodingIdentity::Cp1252,
            input,
            input.map_or(0, <[u8]>::len),
        )
        .unwrap(),
        target,
        ProjectionLimits {
            input_bytes: input.map_or(0, <[u8]>::len),
            output_bytes: expected.map_or(0, <[u8]>::len),
        },
    )
    .unwrap();
    match (actual, target, expected) {
        (None, _, None) => {}
        (
            Some(ProjectedValue::Native(value)),
            ProjectionTarget::Native(encoding),
            Some(expected),
        ) => {
            assert_eq!(value.view().encoding(), encoding);
            assert_eq!(value.view().bytes(), expected)
        }
        (Some(ProjectedValue::SqlUtf16(units)), ProjectionTarget::SqlUtf16, Some(expected)) => {
            assert_eq!(units, sql_units(expected))
        }
        other => panic!("unexpected CP1252 projection {other:?}"),
    }
}

#[test]
fn every_cp1252_byte_and_mixed_fragmented_values_match_all_four_native_runs() {
    for (domain, target) in [
        ("cp1252", ProjectionTarget::Native(EncodingIdentity::Cp1252)),
        ("cp1251", ProjectionTarget::Native(EncodingIdentity::Cp1251)),
        ("utf8", ProjectionTarget::Native(EncodingIdentity::Utf8)),
        ("unicode", ProjectionTarget::SqlUtf16),
    ] {
        for form in ["every-byte", "mixed", "fragmented"] {
            for o in observations(CP1252, &format!("cp1252-{form}-to-{domain}")) {
                let inputs = o["input"].as_array().unwrap();
                let rows = o["readback"]["result"]["sets"][1]["rows"]
                    .as_array()
                    .unwrap();
                assert_eq!(inputs.len(), rows.len());
                if form == "every-byte" {
                    assert_eq!(inputs.len(), 258);
                    for (byte, input) in inputs[2..].iter().enumerate() {
                        assert_eq!(hex(input["valueHex"].as_str().unwrap()), [byte as u8]);
                    }
                }
                for (input, row) in inputs.iter().zip(rows) {
                    assert_eq!(input["id"], row[0]);
                    let source = input["valueHex"].as_str().map(hex);
                    let expected = binary(&row[2]);
                    assert_cp1252_projection(source.as_deref(), target, expected.as_deref());
                }
            }
        }
    }
}

#[test]
fn undefined_cp1252_bytes_keep_sql_control_units_and_direct_utf8_identity() {
    let native = observations(CP1252, "cp1252-every-byte-to-cp1252");
    let utf8 = observations(CP1252, "cp1252-every-byte-to-utf8");
    for (native, utf8) in native.iter().zip(utf8) {
        for byte in [0x81u8, 0x8d, 0x8f, 0x90, 0x9d] {
            let row = &native["readback"]["result"]["sets"][1]["rows"][usize::from(byte) + 2];
            assert_eq!(row[1], "\u{fffd}");
            assert_eq!(binary(&row[2]), Some(vec![byte]));
            assert_eq!(binary(&row[4]), Some(vec![byte, 0]));
            assert_cp1252_projection(Some(&[byte]), ProjectionTarget::SqlUtf16, Some(&[byte, 0]));
            assert_cp1252_projection(
                Some(&[byte]),
                ProjectionTarget::Native(EncodingIdentity::Cp1251),
                Some(b"?"),
            );
            let utf8_row = &utf8["readback"]["result"]["sets"][1]["rows"][usize::from(byte) + 2];
            let expected = binary(&utf8_row[2]).unwrap();
            assert_eq!(expected, [0xc2, byte]);
            assert_cp1252_projection(
                Some(&[byte]),
                ProjectionTarget::Native(EncodingIdentity::Utf8),
                Some(&expected),
            );
        }
    }
}

#[test]
fn cp1252_plans_validate_identity_before_bounds_and_preserve_null_empty() {
    let zero = ProjectionLimits {
        input_bytes: 0,
        output_bytes: 0,
    };
    for target in [
        ProjectionTarget::SqlUtf16,
        ProjectionTarget::Native(EncodingIdentity::Cp1252),
        ProjectionTarget::Native(EncodingIdentity::Cp1251),
        ProjectionTarget::Native(EncodingIdentity::Utf8),
    ] {
        assert_cp1252_projection(None, target, None);
        assert_cp1252_projection(Some(&[]), target, Some(&[]));
        for actual in [
            EncodingIdentity::Cp1251,
            EncodingIdentity::Utf8,
            EncodingIdentity::Opaque(1252),
        ] {
            let value = AnsiView::new(actual, &[0x81], 1).unwrap();
            assert_eq!(
                project(EncodingIdentity::Cp1252, Some(value), target, zero),
                Err(ProjectionError::SourceEncodingMismatch {
                    declared: EncodingIdentity::Cp1252,
                    actual
                })
            );
        }
    }
    for target in [
        EncodingIdentity::Opaque(1251),
        EncodingIdentity::Opaque(1252),
        EncodingIdentity::Opaque(65001),
    ] {
        for value in [
            None,
            AnsiView::nullable(EncodingIdentity::Cp1252, Some(&[]), 0).unwrap(),
        ] {
            assert_eq!(
                project(
                    EncodingIdentity::Cp1252,
                    value,
                    ProjectionTarget::Native(target),
                    zero
                ),
                Err(ProjectionError::UnsupportedTarget(target))
            );
        }
    }
}

#[test]
fn cp1252_limits_count_utf16_bytes_and_utf8_expansion_before_copying() {
    let input = [0, 0x81, 0x80, 0xa9, 0x8c];
    let view = AnsiView::new(EncodingIdentity::Cp1252, &input, 5).unwrap();
    for (target, output_bytes) in [
        (ProjectionTarget::Native(EncodingIdentity::Cp1252), 5),
        (ProjectionTarget::Native(EncodingIdentity::Cp1251), 5),
        (ProjectionTarget::Native(EncodingIdentity::Utf8), 10),
        (ProjectionTarget::SqlUtf16, 10),
    ] {
        assert!(
            project(
                EncodingIdentity::Cp1252,
                Some(view),
                target,
                ProjectionLimits {
                    input_bytes: 5,
                    output_bytes
                }
            )
            .is_ok()
        );
        assert_eq!(
            project(
                EncodingIdentity::Cp1252,
                Some(view),
                target,
                ProjectionLimits {
                    input_bytes: 4,
                    output_bytes
                }
            ),
            Err(ProjectionError::Limit {
                resource: Resource::Input,
                requested: 5,
                maximum: 4
            })
        );
        assert_eq!(
            project(
                EncodingIdentity::Cp1252,
                Some(view),
                target,
                ProjectionLimits {
                    input_bytes: 5,
                    output_bytes: output_bytes - 1
                }
            ),
            Err(ProjectionError::Limit {
                resource: Resource::Output,
                requested: output_bytes,
                maximum: output_bytes - 1
            })
        );
        assert_eq!(view.bytes(), input);
    }
}
