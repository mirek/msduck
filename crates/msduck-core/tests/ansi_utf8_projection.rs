use msduck_core::ansi_bytes::{AnsiView, EncodingIdentity};
use msduck_core::ansi_conversion::{
    ProjectedValue, ProjectionError, ProjectionLimits, ProjectionTarget, Resource, project,
};

#[test]
fn captured_minimum_supplementary_scalar_projects_without_loss() {
    // Retained #913 valid f0908080 controls: native f0908080, SQL units 00d800dc.
    let input = [0xf0, 0x90, 0x80, 0x80];
    let view = AnsiView::new(EncodingIdentity::Utf8, &input, input.len()).unwrap();
    let limits = ProjectionLimits {
        input_bytes: 4,
        output_bytes: 4,
    };
    let actual = project(
        EncodingIdentity::Utf8,
        Some(view),
        ProjectionTarget::SqlUtf16,
        limits,
    )
    .unwrap()
    .unwrap();
    assert_eq!(actual, ProjectedValue::SqlUtf16(vec![0xd800, 0xdc00]));
}

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

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

const REFERENCES: [&str; 3] = [
    include_str!("../../../reference/bulk-character-conversion.json"),
    include_str!("../../../reference/bulk-character-utf8-boundary.json"),
    include_str!("../../../reference/bulk-character-utf8-bounded-target.json"),
];

fn selected(reference: &str) -> Vec<Value> {
    let mut observations = Vec::new();
    let mut position = 0;
    while let Some(offset) = reference[position..].find("{\"case\":") {
        let start = position + offset;
        let end = object_end(reference, start);
        let cs = start + "{\"case\":".len();
        let ce = object_end(reference, cs);
        let case: Value = serde_json::from_str(&reference[cs..ce]).unwrap();
        // These complete projections exclude fixed storage's padding/capacity,
        // wire/declaration mismatches and single-byte target mapping. Selection
        // uses explicit declarations, never equality with candidate output.
        if case["declared"] == "utf8"
            && case["wire"] == "utf8"
            && matches!(case["target"].as_str(), Some("utf8" | "unicode"))
            && case["sourceFamily"] == "varchar"
            && matches!(case["targetFamily"].as_str(), Some("varchar" | "nvarchar"))
        {
            observations.push(serde_json::from_str(&reference[start..end]).unwrap());
        }
        position = end;
    }
    assert!(!observations.is_empty());
    observations
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
fn raw(value: ProjectedValue) -> Vec<u8> {
    match value {
        ProjectedValue::Native(value) => {
            let (encoding, bytes) = value.into_parts();
            assert_eq!(encoding, EncodingIdentity::Utf8);
            bytes
        }
        ProjectedValue::SqlUtf16(units) => units.iter().flat_map(|u| u.to_le_bytes()).collect(),
    }
}
fn limits(input: usize, output: usize) -> ProjectionLimits {
    ProjectionLimits {
        input_bytes: input,
        output_bytes: output,
    }
}
fn view(input: Option<&[u8]>) -> Option<AnsiView<'_>> {
    input.map(|b| AnsiView::new(EncodingIdentity::Utf8, b, b.len()).unwrap())
}

#[test]
fn all_applicable_valid_rows_use_original_native_and_sql_unit_oracles() {
    let (mut observations, mut failed, mut valid_rows, mut malformed_rows, mut comparisons) =
        (0, 0, 0, 0, 0);
    for reference in REFERENCES {
        let mut names = BTreeMap::new();
        for o in selected(reference) {
            observations += 1;
            *names
                .entry(o["case"]["name"].as_str().unwrap().to_owned())
                .or_insert(0) += 1;
            // Failed declaration/admission/decoding loads have no stored value
            // oracle. Original errors/counters remain untouched in the fixture.
            if !o["execution"]["result"]["errors"]
                .as_array()
                .unwrap()
                .is_empty()
            {
                failed += 1;
                assert!(
                    o["readback"]["result"]["sets"][1]["rows"]
                        .as_array()
                        .unwrap()
                        .is_empty()
                );
                continue;
            }
            assert!(o["execution"]["result"]["error"].is_null());
            let descriptor = &o["metadata"]["result"]["sets"][0]["columns"][0];
            assert_eq!(descriptor["type"], "VarChar");
            let collation = &descriptor["collation"];
            assert_eq!(collation["lcid"], 1033);
            assert_eq!(collation["flags"], 96);
            assert_eq!(collation["version"], 2);
            assert_eq!(collation["sortId"], 0);
            assert_eq!(collation["codepage"], "utf-8");
            let output = &o["readback"]["result"]["sets"][1];
            assert_eq!(output["columns"][2]["type"], "VarBinary");
            assert_eq!(output["columns"][3]["type"], "NVarChar");
            assert_eq!(output["columns"][4]["type"], "VarBinary");
            let native = o["case"]["target"] == "utf8";
            assert_eq!(
                output["columns"][1]["type"],
                if native { "VarChar" } else { "NVarChar" }
            );
            if native {
                assert_eq!(output["columns"][1]["collation"], *collation);
            }
            for input in o["input"].as_array().unwrap() {
                let bytes = input["valueHex"].as_str().map(hex);
                if bytes
                    .as_ref()
                    .is_some_and(|b| std::str::from_utf8(b).is_err())
                {
                    malformed_rows += 1;
                    continue;
                }
                valid_rows += 1;
                let original = bytes.clone();
                let row = output["rows"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|r| r[0] == input["id"])
                    .unwrap();
                let targets = if native {
                    vec![
                        (ProjectionTarget::Native(EncodingIdentity::Utf8), 2),
                        (ProjectionTarget::SqlUtf16, 4),
                    ]
                } else {
                    vec![(ProjectionTarget::SqlUtf16, 4)]
                };
                for (target, field) in targets {
                    comparisons += 1;
                    let expected = binary(&row[field]);
                    let exact = limits(
                        bytes.as_ref().map_or(0, Vec::len),
                        expected.as_ref().map_or(0, Vec::len),
                    );
                    assert_eq!(
                        project(
                            EncodingIdentity::Utf8,
                            view(bytes.as_deref()),
                            target,
                            exact
                        )
                        .unwrap()
                        .map(raw),
                        expected,
                        "{} {:?} id{}",
                        o["case"]["name"],
                        target,
                        input["id"]
                    );
                    // Each original successful value exercises active payload
                    // budgets independently of allocation capacity and NULL.
                    if let (Some(bytes), Some(expected)) = (&bytes, &expected) {
                        for (resource, requested) in [
                            (Resource::Input, bytes.len()),
                            (Resource::Output, expected.len()),
                        ] {
                            if requested == 0 {
                                continue;
                            }
                            let mut bound = exact;
                            match resource {
                                Resource::Input => bound.input_bytes -= 1,
                                Resource::Output => bound.output_bytes -= 1,
                            }
                            assert_eq!(
                                project(EncodingIdentity::Utf8, view(Some(bytes)), target, bound),
                                Err(ProjectionError::Limit {
                                    resource,
                                    requested,
                                    maximum: requested - 1
                                })
                            );
                        }
                    }
                }
                assert_eq!(bytes, original);
            }
        }
        assert!(
            names.values().all(|&count| count == 4),
            "every selected case retains all four observations"
        );
    }
    assert_eq!(
        (
            observations,
            failed,
            valid_rows,
            malformed_rows,
            comparisons
        ),
        (716, 236, 920, 184, 1396)
    );
}

#[test]
fn every_retained_malformed_input_stays_outside_the_scalar_domain() {
    let mut unique = BTreeSet::new();
    for reference in REFERENCES {
        for o in selected(reference) {
            for input in o["input"].as_array().unwrap() {
                let Some(text) = input["valueHex"].as_str() else {
                    continue;
                };
                let bytes = hex(text);
                if std::str::from_utf8(&bytes).is_ok() {
                    continue;
                }
                unique.insert(text.to_owned());
                let original = bytes.clone();
                for target in [
                    ProjectionTarget::Native(EncodingIdentity::Utf8),
                    ProjectionTarget::SqlUtf16,
                ] {
                    assert!(
                        matches!(
                            project(
                                EncodingIdentity::Utf8,
                                view(Some(&bytes)),
                                target,
                                limits(bytes.len(), bytes.len() * 2)
                            ),
                            Err(ProjectionError::InvalidUtf8 { .. })
                        ),
                        "{} {target:?}",
                        o["case"]["name"]
                    );
                }
                assert_eq!(bytes, original);
            }
        }
    }
    for text in [
        "80", "c2", "df", "e0a0", "ed9f", "f09080", "f48fbf", "c228", "e08080", "eda080",
        "f0808080", "f4908080", "f5808080",
    ] {
        assert!(unique.contains(text), "retained malformed {text}");
    }
}

#[test]
fn declarations_nullable_precedence_and_structured_byte_errors_are_explicit() {
    for target in [ProjectionTarget::Native(EncodingIdentity::Opaque(65001))] {
        let ProjectionTarget::Native(encoding) = target else {
            unreachable!()
        };
        for value in [
            None,
            view(Some(b"")),
            Some(AnsiView::new(EncodingIdentity::Cp1251, &[0x80], 1).unwrap()),
        ] {
            assert_eq!(
                project(EncodingIdentity::Utf8, value, target, limits(0, 0)),
                Err(ProjectionError::UnsupportedTarget(encoding))
            );
        }
    }
    for target in [
        ProjectionTarget::Native(EncodingIdentity::Cp1251),
        ProjectionTarget::Native(EncodingIdentity::Cp1252),
        ProjectionTarget::Native(EncodingIdentity::Utf8),
        ProjectionTarget::SqlUtf16,
    ] {
        assert_eq!(
            project(EncodingIdentity::Utf8, None, target, limits(0, 0)).unwrap(),
            None
        );
        assert_eq!(
            project(
                EncodingIdentity::Utf8,
                view(Some(b"")),
                target,
                limits(0, 0)
            )
            .unwrap()
            .map(|value| match value {
                ProjectedValue::Native(value) => {
                    let ProjectionTarget::Native(expected) = target else {
                        panic!("expected UTF16 output")
                    };
                    assert_eq!(value.view().encoding(), expected);
                    value.view().bytes().to_vec()
                }
                ProjectedValue::SqlUtf16(units) => {
                    assert_eq!(target, ProjectionTarget::SqlUtf16);
                    units.into_iter().flat_map(u16::to_le_bytes).collect()
                }
            }),
            Some(vec![])
        );
        assert_eq!(
            project(EncodingIdentity::Opaque(65001), None, target, limits(0, 0)),
            Err(ProjectionError::UnsupportedSource(
                EncodingIdentity::Opaque(65001)
            ))
        );
        assert_eq!(
            project(
                EncodingIdentity::Utf8,
                Some(AnsiView::new(EncodingIdentity::Cp1252, &[0x80], 1).unwrap()),
                target,
                limits(0, 0)
            ),
            Err(ProjectionError::SourceEncodingMismatch {
                declared: EncodingIdentity::Utf8,
                actual: EncodingIdentity::Cp1252
            })
        );
        assert_eq!(
            project(
                EncodingIdentity::Utf8,
                view(Some(&[0x80])),
                target,
                limits(0, 0)
            ),
            Err(ProjectionError::Limit {
                resource: Resource::Input,
                requested: 1,
                maximum: 0
            })
        );
        for (input, up, len) in [
            (&b"Ascii\x80"[..], 5, Some(1)),
            (&b"\xc3\xa9\x80"[..], 2, Some(1)),
            (&b"A\xc2("[..], 1, Some(1)),
            (&b"A\xf0\x90\x80"[..], 1, None),
        ] {
            assert_eq!(
                project(
                    EncodingIdentity::Utf8,
                    view(Some(input)),
                    target,
                    limits(input.len(), input.len() * 2)
                ),
                Err(ProjectionError::InvalidUtf8 {
                    valid_up_to: up,
                    error_len: len
                })
            );
        }
    }
}
