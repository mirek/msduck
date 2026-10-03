use msduck_core::ansi_bytes::{AnsiView, EncodingIdentity};
use msduck_core::ansi_conversion::capacity::{Capacity, CapacityError, Family, Plan, SourceForm};
use msduck_core::ansi_conversion::{
    ProjectedValue, ProjectionError, ProjectionLimits, ProjectionTarget, Resource,
};
use serde_json::Value;

fn unpack(value: &Value) -> Option<Vec<u8>> {
    if value.is_null() {
        return None;
    }
    let mut bytes = Vec::new();
    for pair in value.as_array().unwrap() {
        let byte = u8::try_from(pair[0].as_u64().unwrap()).unwrap();
        let count = usize::try_from(pair[1].as_u64().unwrap()).unwrap();
        assert!(count > 0 && count <= 16_385);
        bytes.extend(std::iter::repeat_n(byte, count));
    }
    bytes.into()
}
#[test]
fn public_cp1251_capacity_replays_four_native_oracles() {
    let probes: Value = serde_json::from_str(OBSERVED).unwrap();
    let (mut observations, mut rows, mut failed) = (0, 0, 0);
    assert_eq!(probes.as_array().unwrap().len(), 286);
    for probe in probes.as_array().unwrap() {
        let source = match probe["source"].as_str().unwrap() {
            "cp1251" => EncodingIdentity::Cp1251,
            "cp1252" => EncodingIdentity::Cp1252,
            _ => panic!("unobserved source"),
        };
        let p = plan(
            source,
            if probe["form"] == "max" {
                SourceForm::Max
            } else {
                SourceForm::Bounded
            },
            if probe["fixed"].as_bool().unwrap() {
                Family::Fixed
            } else {
                Family::Variable
            },
            if probe["width"] == "max" {
                Capacity::Max
            } else {
                Capacity::Bounded(probe["width"].as_u64().unwrap() as usize)
            },
        );
        let inputs: Vec<_> = probe["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(unpack)
            .collect();
        assert_eq!(probe["oracles"].as_array().unwrap().len(), 4);
        for oracle in probe["oracles"].as_array().unwrap() {
            observations += 1;
            let rejected = !oracle["errors"].as_array().unwrap().is_empty();
            let mut rejected_row = false;
            if rejected {
                failed += 1;
                assert!(
                    oracle["errors"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .all(|n| *n == 2628)
                );
                assert_eq!(oracle["native"].as_array().unwrap().len(), 0);
            } else {
                assert_eq!(oracle["native"].as_array().unwrap().len(), inputs.len());
            }
            for (index, input) in inputs.iter().enumerate() {
                rows += 1;
                let original = input.clone();
                let view = input
                    .as_deref()
                    .map(|v| AnsiView::new(source, v, v.len()).unwrap());
                let actual = p.apply(
                    view,
                    ProjectionLimits {
                        input_bytes: 16_385,
                        output_bytes: 16_385,
                    },
                );
                if rejected {
                    match actual {
                        Err(CapacityError::Truncation {
                            source_bytes,
                            target_capacity,
                        }) => {
                            assert_eq!(source_bytes, input.as_ref().unwrap().len());
                            assert_eq!(target_capacity, probe["width"].as_u64().unwrap() as usize);
                            rejected_row = true;
                        }
                        Ok(_) => {}
                        other => panic!("{}: {other:?}", probe["name"]),
                    }
                } else {
                    let expected = unpack(&oracle["native"][index]);
                    assert_eq!(
                        actual.unwrap().map(bytes),
                        expected,
                        "{} run{observations} row{index}",
                        probe["name"]
                    );
                    // Every retained successful non-NULL value checks exact budgets and each one-under boundary.
                    if let (Some(input), Some(expected)) = (input.as_ref(), expected.as_ref()) {
                        let exact = ProjectionLimits {
                            input_bytes: input.len(),
                            output_bytes: expected.len(),
                        };
                        assert_eq!(
                            p.apply(view, exact).unwrap().map(bytes),
                            Some(expected.clone())
                        );
                        for (resource, requested) in [
                            (Resource::Input, input.len()),
                            (Resource::Output, expected.len()),
                        ] {
                            if requested == 0 {
                                continue;
                            }
                            let mut limits = exact;
                            match resource {
                                Resource::Input => limits.input_bytes -= 1,
                                Resource::Output => limits.output_bytes -= 1,
                            }
                            assert_eq!(
                                p.apply(view, limits),
                                Err(CapacityError::Projection(ProjectionError::Limit {
                                    resource,
                                    requested,
                                    maximum: requested - 1
                                }))
                            );
                        }
                    }
                }
                assert_eq!(*input, original, "source mutated");
            }
            if rejected {
                assert!(
                    rejected_row,
                    "{} failed load must have a rejected row",
                    probe["name"]
                );
            }
        }
    }
    assert_eq!(observations, 1144);
    assert_eq!((rows, failed), OBSERVED_COUNTS);
}
#[test]
fn cp1251_plan_validates_declarations_and_mismatches() {
    for source in [EncodingIdentity::Utf8, EncodingIdentity::Opaque(1251)] {
        assert_eq!(
            Plan::new(
                source,
                SourceForm::Max,
                ProjectionTarget::Native(EncodingIdentity::Cp1251),
                Family::Variable,
                Capacity::Max
            ),
            Err(CapacityError::Projection(
                ProjectionError::UnsupportedSource(source)
            ))
        );
    }
    for width in [0, 8001, usize::MAX] {
        assert_eq!(
            Plan::new(
                EncodingIdentity::Cp1251,
                SourceForm::Max,
                ProjectionTarget::Native(EncodingIdentity::Cp1251),
                Family::Variable,
                Capacity::Bounded(width)
            ),
            Err(CapacityError::InvalidWidth {
                requested: width,
                maximum: 8000
            })
        );
    }
    assert_eq!(
        Plan::new(
            EncodingIdentity::Cp1251,
            SourceForm::Max,
            ProjectionTarget::Native(EncodingIdentity::Cp1251),
            Family::Fixed,
            Capacity::Max
        ),
        Err(CapacityError::FixedMax)
    );
    let p = plan(
        EncodingIdentity::Cp1251,
        SourceForm::Max,
        Family::Fixed,
        Capacity::Bounded(8000),
    );
    let limits = ProjectionLimits {
        input_bytes: 0,
        output_bytes: 0,
    };
    assert_eq!(p.apply(None, limits).unwrap(), None);
    assert_eq!(
        p.apply(
            Some(AnsiView::new(EncodingIdentity::Cp1252, b"", 0).unwrap()),
            limits
        ),
        Err(CapacityError::Projection(
            ProjectionError::SourceEncodingMismatch {
                declared: EncodingIdentity::Cp1251,
                actual: EncodingIdentity::Cp1252
            }
        ))
    );
    assert_eq!(
        p.apply(
            Some(AnsiView::new(EncodingIdentity::Cp1251, b"", 0).unwrap()),
            limits
        ),
        Err(CapacityError::Projection(ProjectionError::Limit {
            resource: Resource::Output,
            requested: 8000,
            maximum: 0
        }))
    );
}

fn plan(source: EncodingIdentity, form: SourceForm, family: Family, capacity: Capacity) -> Plan {
    Plan::new(
        source,
        form,
        ProjectionTarget::Native(EncodingIdentity::Cp1251),
        family,
        capacity,
    )
    .unwrap()
}
fn bytes(value: ProjectedValue) -> Vec<u8> {
    match value {
        ProjectedValue::Native(value) => {
            let (encoding, bytes) = value.into_parts();
            assert_eq!(encoding, EncodingIdentity::Cp1251);
            bytes
        }
        ProjectedValue::SqlUtf16(_) => panic!("native CP1251 plan returned UTF16"),
    }
}
#[test]
fn captured_same_and_cross_encoding_max_cutoffs_are_distinct() {
    let limits = ProjectionLimits {
        input_bytes: 3,
        output_bytes: 1,
    };
    let input = b"A B";
    for source in [EncodingIdentity::Cp1251, EncodingIdentity::Cp1252] {
        let p = plan(
            source,
            SourceForm::Max,
            Family::Variable,
            Capacity::Bounded(1),
        );
        let actual = p.apply(
            Some(AnsiView::new(source, input, input.len()).unwrap()),
            limits,
        );
        if source == EncodingIdentity::Cp1251 {
            assert_eq!(
                actual,
                Err(CapacityError::Truncation {
                    source_bytes: 3,
                    target_capacity: 1
                })
            );
        } else {
            assert_eq!(bytes(actual.unwrap().unwrap()), b"A");
        }
    }
}

// Independently extracted native-byte/error outcomes for every case in four SQL Server captures.
// Each byte array is losslessly run-length packed as [byte, count]; display and counters are not an oracle.
const OBSERVED_COUNTS: (usize, usize) = (2336, 920);
const OBSERVED: &str = r#"[
{"name":"pattern0-cp1251-bounded-varchar-1","source":"cp1251","form":"bounded","fixed":false,"width":1,"inputs":[null,[],[[65,1]]],"oracles":[{"errors":[],"native":[null,[],[[65,1]]]},{"errors":[],"native":[null,[],[[65,1]]]},{"errors":[],"native":[null,[],[[65,1]]]},{"errors":[],"native":[null,[],[[65,1]]]}]},
{"name":"pattern1-cp1251-bounded-varchar-1","source":"cp1251","form":"bounded","fixed":false,"width":1,"inputs":[null,[[65,1],[32,1]]],"oracles":[{"errors":[],"native":[null,[[65,1]]]},{"errors":[],"native":[null,[[65,1]]]},{"errors":[],"native":[null,[[65,1]]]},{"errors":[],"native":[null,[[65,1]]]}]},
{"name":"pattern2-cp1251-bounded-varchar-1","source":"cp1251","form":"bounded","fixed":false,"width":1,"inputs":[null,[[65,1],[0,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern3-cp1251-bounded-varchar-1","source":"cp1251","form":"bounded","fixed":false,"width":1,"inputs":[null,[[65,1],[160,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern4-cp1251-bounded-varchar-1","source":"cp1251","form":"bounded","fixed":false,"width":1,"inputs":[null,[[65,1],[32,1],[66,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern5-cp1251-bounded-varchar-1","source":"cp1251","form":"bounded","fixed":false,"width":1,"inputs":[null,[[65,1],[32,1],[0,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern6-cp1251-bounded-varchar-1","source":"cp1251","form":"bounded","fixed":false,"width":1,"inputs":[null,[[65,1],[32,1],[160,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern7-cp1251-bounded-varchar-1","source":"cp1251","form":"bounded","fixed":false,"width":1,"inputs":[null,[[32,2],[65,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern8-cp1251-bounded-varchar-2","source":"cp1251","form":"bounded","fixed":false,"width":2,"inputs":[null,[[65,1],[66,1],[32,1],[67,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern9-cp1251-bounded-varchar-2","source":"cp1251","form":"bounded","fixed":false,"width":2,"inputs":[null,[[65,1],[66,1],[32,2],[67,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern10-cp1251-bounded-varchar-2","source":"cp1251","form":"bounded","fixed":false,"width":2,"inputs":[null,[[65,1],[66,1],[32,1],[160,1],[67,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern11-cp1251-bounded-varchar-2","source":"cp1251","form":"bounded","fixed":false,"width":2,"inputs":[null,[[65,1],[66,1],[32,1],[0,1],[67,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern0-cp1251-bounded-char-1","source":"cp1251","form":"bounded","fixed":true,"width":1,"inputs":[null,[],[[65,1]]],"oracles":[{"errors":[],"native":[null,[[32,1]],[[65,1]]]},{"errors":[],"native":[null,[[32,1]],[[65,1]]]},{"errors":[],"native":[null,[[32,1]],[[65,1]]]},{"errors":[],"native":[null,[[32,1]],[[65,1]]]}]},
{"name":"pattern1-cp1251-bounded-char-1","source":"cp1251","form":"bounded","fixed":true,"width":1,"inputs":[null,[[65,1],[32,1]]],"oracles":[{"errors":[],"native":[null,[[65,1]]]},{"errors":[],"native":[null,[[65,1]]]},{"errors":[],"native":[null,[[65,1]]]},{"errors":[],"native":[null,[[65,1]]]}]},
{"name":"pattern2-cp1251-bounded-char-1","source":"cp1251","form":"bounded","fixed":true,"width":1,"inputs":[null,[[65,1],[0,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern3-cp1251-bounded-char-1","source":"cp1251","form":"bounded","fixed":true,"width":1,"inputs":[null,[[65,1],[160,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern4-cp1251-bounded-char-1","source":"cp1251","form":"bounded","fixed":true,"width":1,"inputs":[null,[[65,1],[32,1],[66,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern5-cp1251-bounded-char-1","source":"cp1251","form":"bounded","fixed":true,"width":1,"inputs":[null,[[65,1],[32,1],[0,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern6-cp1251-bounded-char-1","source":"cp1251","form":"bounded","fixed":true,"width":1,"inputs":[null,[[65,1],[32,1],[160,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern7-cp1251-bounded-char-1","source":"cp1251","form":"bounded","fixed":true,"width":1,"inputs":[null,[[32,2],[65,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern8-cp1251-bounded-char-2","source":"cp1251","form":"bounded","fixed":true,"width":2,"inputs":[null,[[65,1],[66,1],[32,1],[67,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern9-cp1251-bounded-char-2","source":"cp1251","form":"bounded","fixed":true,"width":2,"inputs":[null,[[65,1],[66,1],[32,2],[67,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern10-cp1251-bounded-char-2","source":"cp1251","form":"bounded","fixed":true,"width":2,"inputs":[null,[[65,1],[66,1],[32,1],[160,1],[67,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern11-cp1251-bounded-char-2","source":"cp1251","form":"bounded","fixed":true,"width":2,"inputs":[null,[[65,1],[66,1],[32,1],[0,1],[67,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern0-cp1251-max-varchar-1","source":"cp1251","form":"max","fixed":false,"width":1,"inputs":[null,[],[[65,1]]],"oracles":[{"errors":[],"native":[null,[],[[65,1]]]},{"errors":[],"native":[null,[],[[65,1]]]},{"errors":[],"native":[null,[],[[65,1]]]},{"errors":[],"native":[null,[],[[65,1]]]}]},
{"name":"pattern1-cp1251-max-varchar-1","source":"cp1251","form":"max","fixed":false,"width":1,"inputs":[null,[[65,1],[32,1]]],"oracles":[{"errors":[],"native":[null,[[65,1]]]},{"errors":[],"native":[null,[[65,1]]]},{"errors":[],"native":[null,[[65,1]]]},{"errors":[],"native":[null,[[65,1]]]}]},
{"name":"pattern2-cp1251-max-varchar-1","source":"cp1251","form":"max","fixed":false,"width":1,"inputs":[null,[[65,1],[0,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern3-cp1251-max-varchar-1","source":"cp1251","form":"max","fixed":false,"width":1,"inputs":[null,[[65,1],[160,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern4-cp1251-max-varchar-1","source":"cp1251","form":"max","fixed":false,"width":1,"inputs":[null,[[65,1],[32,1],[66,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern5-cp1251-max-varchar-1","source":"cp1251","form":"max","fixed":false,"width":1,"inputs":[null,[[65,1],[32,1],[0,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern6-cp1251-max-varchar-1","source":"cp1251","form":"max","fixed":false,"width":1,"inputs":[null,[[65,1],[32,1],[160,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern7-cp1251-max-varchar-1","source":"cp1251","form":"max","fixed":false,"width":1,"inputs":[null,[[32,2],[65,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern8-cp1251-max-varchar-2","source":"cp1251","form":"max","fixed":false,"width":2,"inputs":[null,[[65,1],[66,1],[32,1],[67,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern9-cp1251-max-varchar-2","source":"cp1251","form":"max","fixed":false,"width":2,"inputs":[null,[[65,1],[66,1],[32,2],[67,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern10-cp1251-max-varchar-2","source":"cp1251","form":"max","fixed":false,"width":2,"inputs":[null,[[65,1],[66,1],[32,1],[160,1],[67,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern11-cp1251-max-varchar-2","source":"cp1251","form":"max","fixed":false,"width":2,"inputs":[null,[[65,1],[66,1],[32,1],[0,1],[67,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern0-cp1251-max-char-1","source":"cp1251","form":"max","fixed":true,"width":1,"inputs":[null,[],[[65,1]]],"oracles":[{"errors":[],"native":[null,[[32,1]],[[65,1]]]},{"errors":[],"native":[null,[[32,1]],[[65,1]]]},{"errors":[],"native":[null,[[32,1]],[[65,1]]]},{"errors":[],"native":[null,[[32,1]],[[65,1]]]}]},
{"name":"pattern1-cp1251-max-char-1","source":"cp1251","form":"max","fixed":true,"width":1,"inputs":[null,[[65,1],[32,1]]],"oracles":[{"errors":[],"native":[null,[[65,1]]]},{"errors":[],"native":[null,[[65,1]]]},{"errors":[],"native":[null,[[65,1]]]},{"errors":[],"native":[null,[[65,1]]]}]},
{"name":"pattern2-cp1251-max-char-1","source":"cp1251","form":"max","fixed":true,"width":1,"inputs":[null,[[65,1],[0,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern3-cp1251-max-char-1","source":"cp1251","form":"max","fixed":true,"width":1,"inputs":[null,[[65,1],[160,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern4-cp1251-max-char-1","source":"cp1251","form":"max","fixed":true,"width":1,"inputs":[null,[[65,1],[32,1],[66,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern5-cp1251-max-char-1","source":"cp1251","form":"max","fixed":true,"width":1,"inputs":[null,[[65,1],[32,1],[0,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern6-cp1251-max-char-1","source":"cp1251","form":"max","fixed":true,"width":1,"inputs":[null,[[65,1],[32,1],[160,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern7-cp1251-max-char-1","source":"cp1251","form":"max","fixed":true,"width":1,"inputs":[null,[[32,2],[65,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern8-cp1251-max-char-2","source":"cp1251","form":"max","fixed":true,"width":2,"inputs":[null,[[65,1],[66,1],[32,1],[67,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern9-cp1251-max-char-2","source":"cp1251","form":"max","fixed":true,"width":2,"inputs":[null,[[65,1],[66,1],[32,2],[67,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern10-cp1251-max-char-2","source":"cp1251","form":"max","fixed":true,"width":2,"inputs":[null,[[65,1],[66,1],[32,1],[160,1],[67,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern11-cp1251-max-char-2","source":"cp1251","form":"max","fixed":true,"width":2,"inputs":[null,[[65,1],[66,1],[32,1],[0,1],[67,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern0-cp1252-bounded-varchar-1","source":"cp1252","form":"bounded","fixed":false,"width":1,"inputs":[null,[],[[65,1]]],"oracles":[{"errors":[],"native":[null,[],[[65,1]]]},{"errors":[],"native":[null,[],[[65,1]]]},{"errors":[],"native":[null,[],[[65,1]]]},{"errors":[],"native":[null,[],[[65,1]]]}]},
{"name":"pattern1-cp1252-bounded-varchar-1","source":"cp1252","form":"bounded","fixed":false,"width":1,"inputs":[null,[[65,1],[32,1]]],"oracles":[{"errors":[],"native":[null,[[65,1]]]},{"errors":[],"native":[null,[[65,1]]]},{"errors":[],"native":[null,[[65,1]]]},{"errors":[],"native":[null,[[65,1]]]}]},
{"name":"pattern2-cp1252-bounded-varchar-1","source":"cp1252","form":"bounded","fixed":false,"width":1,"inputs":[null,[[65,1],[0,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern3-cp1252-bounded-varchar-1","source":"cp1252","form":"bounded","fixed":false,"width":1,"inputs":[null,[[65,1],[160,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern4-cp1252-bounded-varchar-1","source":"cp1252","form":"bounded","fixed":false,"width":1,"inputs":[null,[[65,1],[32,1],[66,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern5-cp1252-bounded-varchar-1","source":"cp1252","form":"bounded","fixed":false,"width":1,"inputs":[null,[[65,1],[32,1],[0,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern6-cp1252-bounded-varchar-1","source":"cp1252","form":"bounded","fixed":false,"width":1,"inputs":[null,[[65,1],[32,1],[160,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern7-cp1252-bounded-varchar-1","source":"cp1252","form":"bounded","fixed":false,"width":1,"inputs":[null,[[32,2],[65,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern8-cp1252-bounded-varchar-2","source":"cp1252","form":"bounded","fixed":false,"width":2,"inputs":[null,[[65,1],[66,1],[32,1],[67,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern9-cp1252-bounded-varchar-2","source":"cp1252","form":"bounded","fixed":false,"width":2,"inputs":[null,[[65,1],[66,1],[32,2],[67,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern10-cp1252-bounded-varchar-2","source":"cp1252","form":"bounded","fixed":false,"width":2,"inputs":[null,[[65,1],[66,1],[32,1],[160,1],[67,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern11-cp1252-bounded-varchar-2","source":"cp1252","form":"bounded","fixed":false,"width":2,"inputs":[null,[[65,1],[66,1],[32,1],[0,1],[67,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern0-cp1252-bounded-char-1","source":"cp1252","form":"bounded","fixed":true,"width":1,"inputs":[null,[],[[65,1]]],"oracles":[{"errors":[],"native":[null,[[32,1]],[[65,1]]]},{"errors":[],"native":[null,[[32,1]],[[65,1]]]},{"errors":[],"native":[null,[[32,1]],[[65,1]]]},{"errors":[],"native":[null,[[32,1]],[[65,1]]]}]},
{"name":"pattern1-cp1252-bounded-char-1","source":"cp1252","form":"bounded","fixed":true,"width":1,"inputs":[null,[[65,1],[32,1]]],"oracles":[{"errors":[],"native":[null,[[65,1]]]},{"errors":[],"native":[null,[[65,1]]]},{"errors":[],"native":[null,[[65,1]]]},{"errors":[],"native":[null,[[65,1]]]}]},
{"name":"pattern2-cp1252-bounded-char-1","source":"cp1252","form":"bounded","fixed":true,"width":1,"inputs":[null,[[65,1],[0,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern3-cp1252-bounded-char-1","source":"cp1252","form":"bounded","fixed":true,"width":1,"inputs":[null,[[65,1],[160,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern4-cp1252-bounded-char-1","source":"cp1252","form":"bounded","fixed":true,"width":1,"inputs":[null,[[65,1],[32,1],[66,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern5-cp1252-bounded-char-1","source":"cp1252","form":"bounded","fixed":true,"width":1,"inputs":[null,[[65,1],[32,1],[0,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern6-cp1252-bounded-char-1","source":"cp1252","form":"bounded","fixed":true,"width":1,"inputs":[null,[[65,1],[32,1],[160,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern7-cp1252-bounded-char-1","source":"cp1252","form":"bounded","fixed":true,"width":1,"inputs":[null,[[32,2],[65,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern8-cp1252-bounded-char-2","source":"cp1252","form":"bounded","fixed":true,"width":2,"inputs":[null,[[65,1],[66,1],[32,1],[67,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern9-cp1252-bounded-char-2","source":"cp1252","form":"bounded","fixed":true,"width":2,"inputs":[null,[[65,1],[66,1],[32,2],[67,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern10-cp1252-bounded-char-2","source":"cp1252","form":"bounded","fixed":true,"width":2,"inputs":[null,[[65,1],[66,1],[32,1],[160,1],[67,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern11-cp1252-bounded-char-2","source":"cp1252","form":"bounded","fixed":true,"width":2,"inputs":[null,[[65,1],[66,1],[32,1],[0,1],[67,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern0-cp1252-max-varchar-1","source":"cp1252","form":"max","fixed":false,"width":1,"inputs":[null,[],[[65,1]]],"oracles":[{"errors":[],"native":[null,[],[[65,1]]]},{"errors":[],"native":[null,[],[[65,1]]]},{"errors":[],"native":[null,[],[[65,1]]]},{"errors":[],"native":[null,[],[[65,1]]]}]},
{"name":"pattern1-cp1252-max-varchar-1","source":"cp1252","form":"max","fixed":false,"width":1,"inputs":[null,[[65,1],[32,1]]],"oracles":[{"errors":[],"native":[null,[[65,1]]]},{"errors":[],"native":[null,[[65,1]]]},{"errors":[],"native":[null,[[65,1]]]},{"errors":[],"native":[null,[[65,1]]]}]},
{"name":"pattern2-cp1252-max-varchar-1","source":"cp1252","form":"max","fixed":false,"width":1,"inputs":[null,[[65,1],[0,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern3-cp1252-max-varchar-1","source":"cp1252","form":"max","fixed":false,"width":1,"inputs":[null,[[65,1],[160,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern4-cp1252-max-varchar-1","source":"cp1252","form":"max","fixed":false,"width":1,"inputs":[null,[[65,1],[32,1],[66,1]]],"oracles":[{"errors":[],"native":[null,[[65,1]]]},{"errors":[],"native":[null,[[65,1]]]},{"errors":[],"native":[null,[[65,1]]]},{"errors":[],"native":[null,[[65,1]]]}]},
{"name":"pattern5-cp1252-max-varchar-1","source":"cp1252","form":"max","fixed":false,"width":1,"inputs":[null,[[65,1],[32,1],[0,1]]],"oracles":[{"errors":[],"native":[null,[[65,1]]]},{"errors":[],"native":[null,[[65,1]]]},{"errors":[],"native":[null,[[65,1]]]},{"errors":[],"native":[null,[[65,1]]]}]},
{"name":"pattern6-cp1252-max-varchar-1","source":"cp1252","form":"max","fixed":false,"width":1,"inputs":[null,[[65,1],[32,1],[160,1]]],"oracles":[{"errors":[],"native":[null,[[65,1]]]},{"errors":[],"native":[null,[[65,1]]]},{"errors":[],"native":[null,[[65,1]]]},{"errors":[],"native":[null,[[65,1]]]}]},
{"name":"pattern7-cp1252-max-varchar-1","source":"cp1252","form":"max","fixed":false,"width":1,"inputs":[null,[[32,2],[65,1]]],"oracles":[{"errors":[],"native":[null,[[32,1]]]},{"errors":[],"native":[null,[[32,1]]]},{"errors":[],"native":[null,[[32,1]]]},{"errors":[],"native":[null,[[32,1]]]}]},
{"name":"pattern8-cp1252-max-varchar-2","source":"cp1252","form":"max","fixed":false,"width":2,"inputs":[null,[[65,1],[66,1],[32,1],[67,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern9-cp1252-max-varchar-2","source":"cp1252","form":"max","fixed":false,"width":2,"inputs":[null,[[65,1],[66,1],[32,2],[67,1]]],"oracles":[{"errors":[],"native":[null,[[65,1],[66,1]]]},{"errors":[],"native":[null,[[65,1],[66,1]]]},{"errors":[],"native":[null,[[65,1],[66,1]]]},{"errors":[],"native":[null,[[65,1],[66,1]]]}]},
{"name":"pattern10-cp1252-max-varchar-2","source":"cp1252","form":"max","fixed":false,"width":2,"inputs":[null,[[65,1],[66,1],[32,1],[160,1],[67,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern11-cp1252-max-varchar-2","source":"cp1252","form":"max","fixed":false,"width":2,"inputs":[null,[[65,1],[66,1],[32,1],[0,1],[67,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern0-cp1252-max-char-1","source":"cp1252","form":"max","fixed":true,"width":1,"inputs":[null,[],[[65,1]]],"oracles":[{"errors":[],"native":[null,[[32,1]],[[65,1]]]},{"errors":[],"native":[null,[[32,1]],[[65,1]]]},{"errors":[],"native":[null,[[32,1]],[[65,1]]]},{"errors":[],"native":[null,[[32,1]],[[65,1]]]}]},
{"name":"pattern1-cp1252-max-char-1","source":"cp1252","form":"max","fixed":true,"width":1,"inputs":[null,[[65,1],[32,1]]],"oracles":[{"errors":[],"native":[null,[[65,1]]]},{"errors":[],"native":[null,[[65,1]]]},{"errors":[],"native":[null,[[65,1]]]},{"errors":[],"native":[null,[[65,1]]]}]},
{"name":"pattern2-cp1252-max-char-1","source":"cp1252","form":"max","fixed":true,"width":1,"inputs":[null,[[65,1],[0,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern3-cp1252-max-char-1","source":"cp1252","form":"max","fixed":true,"width":1,"inputs":[null,[[65,1],[160,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern4-cp1252-max-char-1","source":"cp1252","form":"max","fixed":true,"width":1,"inputs":[null,[[65,1],[32,1],[66,1]]],"oracles":[{"errors":[],"native":[null,[[65,1]]]},{"errors":[],"native":[null,[[65,1]]]},{"errors":[],"native":[null,[[65,1]]]},{"errors":[],"native":[null,[[65,1]]]}]},
{"name":"pattern5-cp1252-max-char-1","source":"cp1252","form":"max","fixed":true,"width":1,"inputs":[null,[[65,1],[32,1],[0,1]]],"oracles":[{"errors":[],"native":[null,[[65,1]]]},{"errors":[],"native":[null,[[65,1]]]},{"errors":[],"native":[null,[[65,1]]]},{"errors":[],"native":[null,[[65,1]]]}]},
{"name":"pattern6-cp1252-max-char-1","source":"cp1252","form":"max","fixed":true,"width":1,"inputs":[null,[[65,1],[32,1],[160,1]]],"oracles":[{"errors":[],"native":[null,[[65,1]]]},{"errors":[],"native":[null,[[65,1]]]},{"errors":[],"native":[null,[[65,1]]]},{"errors":[],"native":[null,[[65,1]]]}]},
{"name":"pattern7-cp1252-max-char-1","source":"cp1252","form":"max","fixed":true,"width":1,"inputs":[null,[[32,2],[65,1]]],"oracles":[{"errors":[],"native":[null,[[32,1]]]},{"errors":[],"native":[null,[[32,1]]]},{"errors":[],"native":[null,[[32,1]]]},{"errors":[],"native":[null,[[32,1]]]}]},
{"name":"pattern8-cp1252-max-char-2","source":"cp1252","form":"max","fixed":true,"width":2,"inputs":[null,[[65,1],[66,1],[32,1],[67,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern9-cp1252-max-char-2","source":"cp1252","form":"max","fixed":true,"width":2,"inputs":[null,[[65,1],[66,1],[32,2],[67,1]]],"oracles":[{"errors":[],"native":[null,[[65,1],[66,1]]]},{"errors":[],"native":[null,[[65,1],[66,1]]]},{"errors":[],"native":[null,[[65,1],[66,1]]]},{"errors":[],"native":[null,[[65,1],[66,1]]]}]},
{"name":"pattern10-cp1252-max-char-2","source":"cp1252","form":"max","fixed":true,"width":2,"inputs":[null,[[65,1],[66,1],[32,1],[160,1],[67,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"pattern11-cp1252-max-char-2","source":"cp1252","form":"max","fixed":true,"width":2,"inputs":[null,[[65,1],[66,1],[32,1],[0,1],[67,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window0-cp1251-bounded-varchar-3","source":"cp1251","form":"bounded","fixed":false,"width":3,"inputs":[null,[[65,3],[32,3],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window1-cp1251-bounded-varchar-3","source":"cp1251","form":"bounded","fixed":false,"width":3,"inputs":[null,[[65,3],[32,2],[90,1],[32,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window2-cp1251-bounded-varchar-3","source":"cp1251","form":"bounded","fixed":false,"width":3,"inputs":[null,[[65,3],[32,2],[0,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window3-cp1251-bounded-varchar-3","source":"cp1251","form":"bounded","fixed":false,"width":3,"inputs":[null,[[65,3],[32,2],[160,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window0-cp1251-bounded-varchar-4","source":"cp1251","form":"bounded","fixed":false,"width":4,"inputs":[null,[[65,4],[32,4],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window1-cp1251-bounded-varchar-4","source":"cp1251","form":"bounded","fixed":false,"width":4,"inputs":[null,[[65,4],[32,3],[90,1],[32,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window2-cp1251-bounded-varchar-4","source":"cp1251","form":"bounded","fixed":false,"width":4,"inputs":[null,[[65,4],[32,3],[0,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window3-cp1251-bounded-varchar-4","source":"cp1251","form":"bounded","fixed":false,"width":4,"inputs":[null,[[65,4],[32,3],[160,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window0-cp1251-bounded-char-3","source":"cp1251","form":"bounded","fixed":true,"width":3,"inputs":[null,[[65,3],[32,3],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window1-cp1251-bounded-char-3","source":"cp1251","form":"bounded","fixed":true,"width":3,"inputs":[null,[[65,3],[32,2],[90,1],[32,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window2-cp1251-bounded-char-3","source":"cp1251","form":"bounded","fixed":true,"width":3,"inputs":[null,[[65,3],[32,2],[0,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window3-cp1251-bounded-char-3","source":"cp1251","form":"bounded","fixed":true,"width":3,"inputs":[null,[[65,3],[32,2],[160,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window0-cp1251-bounded-char-4","source":"cp1251","form":"bounded","fixed":true,"width":4,"inputs":[null,[[65,4],[32,4],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window1-cp1251-bounded-char-4","source":"cp1251","form":"bounded","fixed":true,"width":4,"inputs":[null,[[65,4],[32,3],[90,1],[32,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window2-cp1251-bounded-char-4","source":"cp1251","form":"bounded","fixed":true,"width":4,"inputs":[null,[[65,4],[32,3],[0,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window3-cp1251-bounded-char-4","source":"cp1251","form":"bounded","fixed":true,"width":4,"inputs":[null,[[65,4],[32,3],[160,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window0-cp1251-max-varchar-3","source":"cp1251","form":"max","fixed":false,"width":3,"inputs":[null,[[65,3],[32,3],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window1-cp1251-max-varchar-3","source":"cp1251","form":"max","fixed":false,"width":3,"inputs":[null,[[65,3],[32,2],[90,1],[32,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window2-cp1251-max-varchar-3","source":"cp1251","form":"max","fixed":false,"width":3,"inputs":[null,[[65,3],[32,2],[0,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window3-cp1251-max-varchar-3","source":"cp1251","form":"max","fixed":false,"width":3,"inputs":[null,[[65,3],[32,2],[160,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window0-cp1251-max-varchar-4","source":"cp1251","form":"max","fixed":false,"width":4,"inputs":[null,[[65,4],[32,4],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window1-cp1251-max-varchar-4","source":"cp1251","form":"max","fixed":false,"width":4,"inputs":[null,[[65,4],[32,3],[90,1],[32,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window2-cp1251-max-varchar-4","source":"cp1251","form":"max","fixed":false,"width":4,"inputs":[null,[[65,4],[32,3],[0,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window3-cp1251-max-varchar-4","source":"cp1251","form":"max","fixed":false,"width":4,"inputs":[null,[[65,4],[32,3],[160,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window0-cp1251-max-char-3","source":"cp1251","form":"max","fixed":true,"width":3,"inputs":[null,[[65,3],[32,3],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window1-cp1251-max-char-3","source":"cp1251","form":"max","fixed":true,"width":3,"inputs":[null,[[65,3],[32,2],[90,1],[32,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window2-cp1251-max-char-3","source":"cp1251","form":"max","fixed":true,"width":3,"inputs":[null,[[65,3],[32,2],[0,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window3-cp1251-max-char-3","source":"cp1251","form":"max","fixed":true,"width":3,"inputs":[null,[[65,3],[32,2],[160,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window0-cp1251-max-char-4","source":"cp1251","form":"max","fixed":true,"width":4,"inputs":[null,[[65,4],[32,4],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window1-cp1251-max-char-4","source":"cp1251","form":"max","fixed":true,"width":4,"inputs":[null,[[65,4],[32,3],[90,1],[32,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window2-cp1251-max-char-4","source":"cp1251","form":"max","fixed":true,"width":4,"inputs":[null,[[65,4],[32,3],[0,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window3-cp1251-max-char-4","source":"cp1251","form":"max","fixed":true,"width":4,"inputs":[null,[[65,4],[32,3],[160,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window0-cp1251-char64-varchar-3","source":"cp1251","form":"bounded","fixed":false,"width":3,"inputs":[null,[[65,3],[32,3],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window1-cp1251-char64-varchar-3","source":"cp1251","form":"bounded","fixed":false,"width":3,"inputs":[null,[[65,3],[32,2],[90,1],[32,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window2-cp1251-char64-varchar-3","source":"cp1251","form":"bounded","fixed":false,"width":3,"inputs":[null,[[65,3],[32,2],[0,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window3-cp1251-char64-varchar-3","source":"cp1251","form":"bounded","fixed":false,"width":3,"inputs":[null,[[65,3],[32,2],[160,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window0-cp1251-char64-varchar-4","source":"cp1251","form":"bounded","fixed":false,"width":4,"inputs":[null,[[65,4],[32,4],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window1-cp1251-char64-varchar-4","source":"cp1251","form":"bounded","fixed":false,"width":4,"inputs":[null,[[65,4],[32,3],[90,1],[32,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window2-cp1251-char64-varchar-4","source":"cp1251","form":"bounded","fixed":false,"width":4,"inputs":[null,[[65,4],[32,3],[0,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window3-cp1251-char64-varchar-4","source":"cp1251","form":"bounded","fixed":false,"width":4,"inputs":[null,[[65,4],[32,3],[160,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window0-cp1251-char64-char-3","source":"cp1251","form":"bounded","fixed":true,"width":3,"inputs":[null,[[65,3],[32,3],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window1-cp1251-char64-char-3","source":"cp1251","form":"bounded","fixed":true,"width":3,"inputs":[null,[[65,3],[32,2],[90,1],[32,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window2-cp1251-char64-char-3","source":"cp1251","form":"bounded","fixed":true,"width":3,"inputs":[null,[[65,3],[32,2],[0,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window3-cp1251-char64-char-3","source":"cp1251","form":"bounded","fixed":true,"width":3,"inputs":[null,[[65,3],[32,2],[160,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window0-cp1251-char64-char-4","source":"cp1251","form":"bounded","fixed":true,"width":4,"inputs":[null,[[65,4],[32,4],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window1-cp1251-char64-char-4","source":"cp1251","form":"bounded","fixed":true,"width":4,"inputs":[null,[[65,4],[32,3],[90,1],[32,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window2-cp1251-char64-char-4","source":"cp1251","form":"bounded","fixed":true,"width":4,"inputs":[null,[[65,4],[32,3],[0,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window3-cp1251-char64-char-4","source":"cp1251","form":"bounded","fixed":true,"width":4,"inputs":[null,[[65,4],[32,3],[160,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window0-cp1252-bounded-varchar-3","source":"cp1252","form":"bounded","fixed":false,"width":3,"inputs":[null,[[65,3],[32,3],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window1-cp1252-bounded-varchar-3","source":"cp1252","form":"bounded","fixed":false,"width":3,"inputs":[null,[[65,3],[32,2],[90,1],[32,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window2-cp1252-bounded-varchar-3","source":"cp1252","form":"bounded","fixed":false,"width":3,"inputs":[null,[[65,3],[32,2],[0,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window3-cp1252-bounded-varchar-3","source":"cp1252","form":"bounded","fixed":false,"width":3,"inputs":[null,[[65,3],[32,2],[160,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window0-cp1252-bounded-varchar-4","source":"cp1252","form":"bounded","fixed":false,"width":4,"inputs":[null,[[65,4],[32,4],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window1-cp1252-bounded-varchar-4","source":"cp1252","form":"bounded","fixed":false,"width":4,"inputs":[null,[[65,4],[32,3],[90,1],[32,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window2-cp1252-bounded-varchar-4","source":"cp1252","form":"bounded","fixed":false,"width":4,"inputs":[null,[[65,4],[32,3],[0,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window3-cp1252-bounded-varchar-4","source":"cp1252","form":"bounded","fixed":false,"width":4,"inputs":[null,[[65,4],[32,3],[160,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window0-cp1252-bounded-char-3","source":"cp1252","form":"bounded","fixed":true,"width":3,"inputs":[null,[[65,3],[32,3],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window1-cp1252-bounded-char-3","source":"cp1252","form":"bounded","fixed":true,"width":3,"inputs":[null,[[65,3],[32,2],[90,1],[32,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window2-cp1252-bounded-char-3","source":"cp1252","form":"bounded","fixed":true,"width":3,"inputs":[null,[[65,3],[32,2],[0,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window3-cp1252-bounded-char-3","source":"cp1252","form":"bounded","fixed":true,"width":3,"inputs":[null,[[65,3],[32,2],[160,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window0-cp1252-bounded-char-4","source":"cp1252","form":"bounded","fixed":true,"width":4,"inputs":[null,[[65,4],[32,4],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window1-cp1252-bounded-char-4","source":"cp1252","form":"bounded","fixed":true,"width":4,"inputs":[null,[[65,4],[32,3],[90,1],[32,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window2-cp1252-bounded-char-4","source":"cp1252","form":"bounded","fixed":true,"width":4,"inputs":[null,[[65,4],[32,3],[0,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window3-cp1252-bounded-char-4","source":"cp1252","form":"bounded","fixed":true,"width":4,"inputs":[null,[[65,4],[32,3],[160,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window0-cp1252-max-varchar-3","source":"cp1252","form":"max","fixed":false,"width":3,"inputs":[null,[[65,3],[32,3],[90,1]]],"oracles":[{"errors":[],"native":[null,[[65,3]]]},{"errors":[],"native":[null,[[65,3]]]},{"errors":[],"native":[null,[[65,3]]]},{"errors":[],"native":[null,[[65,3]]]}]},
{"name":"window1-cp1252-max-varchar-3","source":"cp1252","form":"max","fixed":false,"width":3,"inputs":[null,[[65,3],[32,2],[90,1],[32,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window2-cp1252-max-varchar-3","source":"cp1252","form":"max","fixed":false,"width":3,"inputs":[null,[[65,3],[32,2],[0,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window3-cp1252-max-varchar-3","source":"cp1252","form":"max","fixed":false,"width":3,"inputs":[null,[[65,3],[32,2],[160,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window0-cp1252-max-varchar-4","source":"cp1252","form":"max","fixed":false,"width":4,"inputs":[null,[[65,4],[32,4],[90,1]]],"oracles":[{"errors":[],"native":[null,[[65,4]]]},{"errors":[],"native":[null,[[65,4]]]},{"errors":[],"native":[null,[[65,4]]]},{"errors":[],"native":[null,[[65,4]]]}]},
{"name":"window1-cp1252-max-varchar-4","source":"cp1252","form":"max","fixed":false,"width":4,"inputs":[null,[[65,4],[32,3],[90,1],[32,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window2-cp1252-max-varchar-4","source":"cp1252","form":"max","fixed":false,"width":4,"inputs":[null,[[65,4],[32,3],[0,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window3-cp1252-max-varchar-4","source":"cp1252","form":"max","fixed":false,"width":4,"inputs":[null,[[65,4],[32,3],[160,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window0-cp1252-max-char-3","source":"cp1252","form":"max","fixed":true,"width":3,"inputs":[null,[[65,3],[32,3],[90,1]]],"oracles":[{"errors":[],"native":[null,[[65,3]]]},{"errors":[],"native":[null,[[65,3]]]},{"errors":[],"native":[null,[[65,3]]]},{"errors":[],"native":[null,[[65,3]]]}]},
{"name":"window1-cp1252-max-char-3","source":"cp1252","form":"max","fixed":true,"width":3,"inputs":[null,[[65,3],[32,2],[90,1],[32,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window2-cp1252-max-char-3","source":"cp1252","form":"max","fixed":true,"width":3,"inputs":[null,[[65,3],[32,2],[0,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window3-cp1252-max-char-3","source":"cp1252","form":"max","fixed":true,"width":3,"inputs":[null,[[65,3],[32,2],[160,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window0-cp1252-max-char-4","source":"cp1252","form":"max","fixed":true,"width":4,"inputs":[null,[[65,4],[32,4],[90,1]]],"oracles":[{"errors":[],"native":[null,[[65,4]]]},{"errors":[],"native":[null,[[65,4]]]},{"errors":[],"native":[null,[[65,4]]]},{"errors":[],"native":[null,[[65,4]]]}]},
{"name":"window1-cp1252-max-char-4","source":"cp1252","form":"max","fixed":true,"width":4,"inputs":[null,[[65,4],[32,3],[90,1],[32,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window2-cp1252-max-char-4","source":"cp1252","form":"max","fixed":true,"width":4,"inputs":[null,[[65,4],[32,3],[0,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window3-cp1252-max-char-4","source":"cp1252","form":"max","fixed":true,"width":4,"inputs":[null,[[65,4],[32,3],[160,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window0-cp1252-char64-varchar-3","source":"cp1252","form":"bounded","fixed":false,"width":3,"inputs":[null,[[65,3],[32,3],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window1-cp1252-char64-varchar-3","source":"cp1252","form":"bounded","fixed":false,"width":3,"inputs":[null,[[65,3],[32,2],[90,1],[32,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window2-cp1252-char64-varchar-3","source":"cp1252","form":"bounded","fixed":false,"width":3,"inputs":[null,[[65,3],[32,2],[0,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window3-cp1252-char64-varchar-3","source":"cp1252","form":"bounded","fixed":false,"width":3,"inputs":[null,[[65,3],[32,2],[160,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window0-cp1252-char64-varchar-4","source":"cp1252","form":"bounded","fixed":false,"width":4,"inputs":[null,[[65,4],[32,4],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window1-cp1252-char64-varchar-4","source":"cp1252","form":"bounded","fixed":false,"width":4,"inputs":[null,[[65,4],[32,3],[90,1],[32,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window2-cp1252-char64-varchar-4","source":"cp1252","form":"bounded","fixed":false,"width":4,"inputs":[null,[[65,4],[32,3],[0,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window3-cp1252-char64-varchar-4","source":"cp1252","form":"bounded","fixed":false,"width":4,"inputs":[null,[[65,4],[32,3],[160,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window0-cp1252-char64-char-3","source":"cp1252","form":"bounded","fixed":true,"width":3,"inputs":[null,[[65,3],[32,3],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window1-cp1252-char64-char-3","source":"cp1252","form":"bounded","fixed":true,"width":3,"inputs":[null,[[65,3],[32,2],[90,1],[32,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window2-cp1252-char64-char-3","source":"cp1252","form":"bounded","fixed":true,"width":3,"inputs":[null,[[65,3],[32,2],[0,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window3-cp1252-char64-char-3","source":"cp1252","form":"bounded","fixed":true,"width":3,"inputs":[null,[[65,3],[32,2],[160,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window0-cp1252-char64-char-4","source":"cp1252","form":"bounded","fixed":true,"width":4,"inputs":[null,[[65,4],[32,4],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window1-cp1252-char64-char-4","source":"cp1252","form":"bounded","fixed":true,"width":4,"inputs":[null,[[65,4],[32,3],[90,1],[32,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window2-cp1252-char64-char-4","source":"cp1252","form":"bounded","fixed":true,"width":4,"inputs":[null,[[65,4],[32,3],[0,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"window3-cp1252-char64-char-4","source":"cp1252","form":"bounded","fixed":true,"width":4,"inputs":[null,[[65,4],[32,3],[160,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window0-cp1251-max-varchar-8","source":"cp1251","form":"max","fixed":false,"width":8,"inputs":[null,[[65,8],[32,8],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window1-cp1251-max-varchar-8","source":"cp1251","form":"max","fixed":false,"width":8,"inputs":[null,[[65,8],[32,7],[90,1],[32,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window2-cp1251-max-varchar-8","source":"cp1251","form":"max","fixed":false,"width":8,"inputs":[null,[[65,8],[32,7],[0,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window3-cp1251-max-varchar-8","source":"cp1251","form":"max","fixed":false,"width":8,"inputs":[null,[[65,8],[32,7],[160,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window0-cp1251-max-varchar-64","source":"cp1251","form":"max","fixed":false,"width":64,"inputs":[null,[[65,64],[32,64],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window1-cp1251-max-varchar-64","source":"cp1251","form":"max","fixed":false,"width":64,"inputs":[null,[[65,64],[32,63],[90,1],[32,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window2-cp1251-max-varchar-64","source":"cp1251","form":"max","fixed":false,"width":64,"inputs":[null,[[65,64],[32,63],[0,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window3-cp1251-max-varchar-64","source":"cp1251","form":"max","fixed":false,"width":64,"inputs":[null,[[65,64],[32,63],[160,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window0-cp1251-max-varchar-4000","source":"cp1251","form":"max","fixed":false,"width":4000,"inputs":[null,[[65,4000],[32,4000],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window1-cp1251-max-varchar-4000","source":"cp1251","form":"max","fixed":false,"width":4000,"inputs":[null,[[65,4000],[32,3999],[90,1],[32,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window2-cp1251-max-varchar-4000","source":"cp1251","form":"max","fixed":false,"width":4000,"inputs":[null,[[65,4000],[32,3999],[0,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window3-cp1251-max-varchar-4000","source":"cp1251","form":"max","fixed":false,"width":4000,"inputs":[null,[[65,4000],[32,3999],[160,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window0-cp1251-max-varchar-8000","source":"cp1251","form":"max","fixed":false,"width":8000,"inputs":[null,[[65,8000],[32,8000],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window1-cp1251-max-varchar-8000","source":"cp1251","form":"max","fixed":false,"width":8000,"inputs":[null,[[65,8000],[32,7999],[90,1],[32,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window2-cp1251-max-varchar-8000","source":"cp1251","form":"max","fixed":false,"width":8000,"inputs":[null,[[65,8000],[32,7999],[0,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window3-cp1251-max-varchar-8000","source":"cp1251","form":"max","fixed":false,"width":8000,"inputs":[null,[[65,8000],[32,7999],[160,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window0-cp1251-max-char-8","source":"cp1251","form":"max","fixed":true,"width":8,"inputs":[null,[[65,8],[32,8],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window1-cp1251-max-char-8","source":"cp1251","form":"max","fixed":true,"width":8,"inputs":[null,[[65,8],[32,7],[90,1],[32,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window2-cp1251-max-char-8","source":"cp1251","form":"max","fixed":true,"width":8,"inputs":[null,[[65,8],[32,7],[0,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window3-cp1251-max-char-8","source":"cp1251","form":"max","fixed":true,"width":8,"inputs":[null,[[65,8],[32,7],[160,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window0-cp1251-max-char-64","source":"cp1251","form":"max","fixed":true,"width":64,"inputs":[null,[[65,64],[32,64],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window1-cp1251-max-char-64","source":"cp1251","form":"max","fixed":true,"width":64,"inputs":[null,[[65,64],[32,63],[90,1],[32,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window2-cp1251-max-char-64","source":"cp1251","form":"max","fixed":true,"width":64,"inputs":[null,[[65,64],[32,63],[0,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window3-cp1251-max-char-64","source":"cp1251","form":"max","fixed":true,"width":64,"inputs":[null,[[65,64],[32,63],[160,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window0-cp1251-max-char-4000","source":"cp1251","form":"max","fixed":true,"width":4000,"inputs":[null,[[65,4000],[32,4000],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window1-cp1251-max-char-4000","source":"cp1251","form":"max","fixed":true,"width":4000,"inputs":[null,[[65,4000],[32,3999],[90,1],[32,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window2-cp1251-max-char-4000","source":"cp1251","form":"max","fixed":true,"width":4000,"inputs":[null,[[65,4000],[32,3999],[0,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window3-cp1251-max-char-4000","source":"cp1251","form":"max","fixed":true,"width":4000,"inputs":[null,[[65,4000],[32,3999],[160,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window0-cp1251-max-char-8000","source":"cp1251","form":"max","fixed":true,"width":8000,"inputs":[null,[[65,8000],[32,8000],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window1-cp1251-max-char-8000","source":"cp1251","form":"max","fixed":true,"width":8000,"inputs":[null,[[65,8000],[32,7999],[90,1],[32,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window2-cp1251-max-char-8000","source":"cp1251","form":"max","fixed":true,"width":8000,"inputs":[null,[[65,8000],[32,7999],[0,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window3-cp1251-max-char-8000","source":"cp1251","form":"max","fixed":true,"width":8000,"inputs":[null,[[65,8000],[32,7999],[160,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window0-cp1252-max-varchar-8","source":"cp1252","form":"max","fixed":false,"width":8,"inputs":[null,[[65,8],[32,8],[90,1]]],"oracles":[{"errors":[],"native":[null,[[65,8]]]},{"errors":[],"native":[null,[[65,8]]]},{"errors":[],"native":[null,[[65,8]]]},{"errors":[],"native":[null,[[65,8]]]}]},
{"name":"edge-window1-cp1252-max-varchar-8","source":"cp1252","form":"max","fixed":false,"width":8,"inputs":[null,[[65,8],[32,7],[90,1],[32,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window2-cp1252-max-varchar-8","source":"cp1252","form":"max","fixed":false,"width":8,"inputs":[null,[[65,8],[32,7],[0,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window3-cp1252-max-varchar-8","source":"cp1252","form":"max","fixed":false,"width":8,"inputs":[null,[[65,8],[32,7],[160,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window0-cp1252-max-varchar-64","source":"cp1252","form":"max","fixed":false,"width":64,"inputs":[null,[[65,64],[32,64],[90,1]]],"oracles":[{"errors":[],"native":[null,[[65,64]]]},{"errors":[],"native":[null,[[65,64]]]},{"errors":[],"native":[null,[[65,64]]]},{"errors":[],"native":[null,[[65,64]]]}]},
{"name":"edge-window1-cp1252-max-varchar-64","source":"cp1252","form":"max","fixed":false,"width":64,"inputs":[null,[[65,64],[32,63],[90,1],[32,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window2-cp1252-max-varchar-64","source":"cp1252","form":"max","fixed":false,"width":64,"inputs":[null,[[65,64],[32,63],[0,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window3-cp1252-max-varchar-64","source":"cp1252","form":"max","fixed":false,"width":64,"inputs":[null,[[65,64],[32,63],[160,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window0-cp1252-max-varchar-4000","source":"cp1252","form":"max","fixed":false,"width":4000,"inputs":[null,[[65,4000],[32,4000],[90,1]]],"oracles":[{"errors":[],"native":[null,[[65,4000]]]},{"errors":[],"native":[null,[[65,4000]]]},{"errors":[],"native":[null,[[65,4000]]]},{"errors":[],"native":[null,[[65,4000]]]}]},
{"name":"edge-window1-cp1252-max-varchar-4000","source":"cp1252","form":"max","fixed":false,"width":4000,"inputs":[null,[[65,4000],[32,3999],[90,1],[32,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window2-cp1252-max-varchar-4000","source":"cp1252","form":"max","fixed":false,"width":4000,"inputs":[null,[[65,4000],[32,3999],[0,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window3-cp1252-max-varchar-4000","source":"cp1252","form":"max","fixed":false,"width":4000,"inputs":[null,[[65,4000],[32,3999],[160,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window0-cp1252-max-varchar-8000","source":"cp1252","form":"max","fixed":false,"width":8000,"inputs":[null,[[65,8000],[32,8000],[90,1]]],"oracles":[{"errors":[],"native":[null,[[65,8000]]]},{"errors":[],"native":[null,[[65,8000]]]},{"errors":[],"native":[null,[[65,8000]]]},{"errors":[],"native":[null,[[65,8000]]]}]},
{"name":"edge-window1-cp1252-max-varchar-8000","source":"cp1252","form":"max","fixed":false,"width":8000,"inputs":[null,[[65,8000],[32,7999],[90,1],[32,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window2-cp1252-max-varchar-8000","source":"cp1252","form":"max","fixed":false,"width":8000,"inputs":[null,[[65,8000],[32,7999],[0,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window3-cp1252-max-varchar-8000","source":"cp1252","form":"max","fixed":false,"width":8000,"inputs":[null,[[65,8000],[32,7999],[160,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window0-cp1252-max-char-8","source":"cp1252","form":"max","fixed":true,"width":8,"inputs":[null,[[65,8],[32,8],[90,1]]],"oracles":[{"errors":[],"native":[null,[[65,8]]]},{"errors":[],"native":[null,[[65,8]]]},{"errors":[],"native":[null,[[65,8]]]},{"errors":[],"native":[null,[[65,8]]]}]},
{"name":"edge-window1-cp1252-max-char-8","source":"cp1252","form":"max","fixed":true,"width":8,"inputs":[null,[[65,8],[32,7],[90,1],[32,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window2-cp1252-max-char-8","source":"cp1252","form":"max","fixed":true,"width":8,"inputs":[null,[[65,8],[32,7],[0,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window3-cp1252-max-char-8","source":"cp1252","form":"max","fixed":true,"width":8,"inputs":[null,[[65,8],[32,7],[160,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window0-cp1252-max-char-64","source":"cp1252","form":"max","fixed":true,"width":64,"inputs":[null,[[65,64],[32,64],[90,1]]],"oracles":[{"errors":[],"native":[null,[[65,64]]]},{"errors":[],"native":[null,[[65,64]]]},{"errors":[],"native":[null,[[65,64]]]},{"errors":[],"native":[null,[[65,64]]]}]},
{"name":"edge-window1-cp1252-max-char-64","source":"cp1252","form":"max","fixed":true,"width":64,"inputs":[null,[[65,64],[32,63],[90,1],[32,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window2-cp1252-max-char-64","source":"cp1252","form":"max","fixed":true,"width":64,"inputs":[null,[[65,64],[32,63],[0,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window3-cp1252-max-char-64","source":"cp1252","form":"max","fixed":true,"width":64,"inputs":[null,[[65,64],[32,63],[160,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window0-cp1252-max-char-4000","source":"cp1252","form":"max","fixed":true,"width":4000,"inputs":[null,[[65,4000],[32,4000],[90,1]]],"oracles":[{"errors":[],"native":[null,[[65,4000]]]},{"errors":[],"native":[null,[[65,4000]]]},{"errors":[],"native":[null,[[65,4000]]]},{"errors":[],"native":[null,[[65,4000]]]}]},
{"name":"edge-window1-cp1252-max-char-4000","source":"cp1252","form":"max","fixed":true,"width":4000,"inputs":[null,[[65,4000],[32,3999],[90,1],[32,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window2-cp1252-max-char-4000","source":"cp1252","form":"max","fixed":true,"width":4000,"inputs":[null,[[65,4000],[32,3999],[0,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window3-cp1252-max-char-4000","source":"cp1252","form":"max","fixed":true,"width":4000,"inputs":[null,[[65,4000],[32,3999],[160,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window0-cp1252-max-char-8000","source":"cp1252","form":"max","fixed":true,"width":8000,"inputs":[null,[[65,8000],[32,8000],[90,1]]],"oracles":[{"errors":[],"native":[null,[[65,8000]]]},{"errors":[],"native":[null,[[65,8000]]]},{"errors":[],"native":[null,[[65,8000]]]},{"errors":[],"native":[null,[[65,8000]]]}]},
{"name":"edge-window1-cp1252-max-char-8000","source":"cp1252","form":"max","fixed":true,"width":8000,"inputs":[null,[[65,8000],[32,7999],[90,1],[32,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window2-cp1252-max-char-8000","source":"cp1252","form":"max","fixed":true,"width":8000,"inputs":[null,[[65,8000],[32,7999],[0,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-window3-cp1252-max-char-8000","source":"cp1252","form":"max","fixed":true,"width":8000,"inputs":[null,[[65,8000],[32,7999],[160,1],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-exact-cp1251-bounded8000-varchar-8","source":"cp1251","form":"bounded","fixed":false,"width":8,"inputs":[null,[[65,7],[152,1]]],"oracles":[{"errors":[],"native":[null,[[65,7],[152,1]]]},{"errors":[],"native":[null,[[65,7],[152,1]]]},{"errors":[],"native":[null,[[65,7],[152,1]]]},{"errors":[],"native":[null,[[65,7],[152,1]]]}]},
{"name":"edge-over-cp1251-bounded8000-varchar-8","source":"cp1251","form":"bounded","fixed":false,"width":8,"inputs":[null,[[65,8],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-exact-cp1251-bounded8000-varchar-64","source":"cp1251","form":"bounded","fixed":false,"width":64,"inputs":[null,[[65,63],[152,1]]],"oracles":[{"errors":[],"native":[null,[[65,63],[152,1]]]},{"errors":[],"native":[null,[[65,63],[152,1]]]},{"errors":[],"native":[null,[[65,63],[152,1]]]},{"errors":[],"native":[null,[[65,63],[152,1]]]}]},
{"name":"edge-over-cp1251-bounded8000-varchar-64","source":"cp1251","form":"bounded","fixed":false,"width":64,"inputs":[null,[[65,64],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-exact-cp1251-bounded8000-varchar-4000","source":"cp1251","form":"bounded","fixed":false,"width":4000,"inputs":[null,[[65,3999],[152,1]]],"oracles":[{"errors":[],"native":[null,[[65,3999],[152,1]]]},{"errors":[],"native":[null,[[65,3999],[152,1]]]},{"errors":[],"native":[null,[[65,3999],[152,1]]]},{"errors":[],"native":[null,[[65,3999],[152,1]]]}]},
{"name":"edge-over-cp1251-bounded8000-varchar-4000","source":"cp1251","form":"bounded","fixed":false,"width":4000,"inputs":[null,[[65,4000],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-exact-cp1251-bounded8000-varchar-8000","source":"cp1251","form":"bounded","fixed":false,"width":8000,"inputs":[null,[[65,7999],[152,1]]],"oracles":[{"errors":[],"native":[null,[[65,7999],[152,1]]]},{"errors":[],"native":[null,[[65,7999],[152,1]]]},{"errors":[],"native":[null,[[65,7999],[152,1]]]},{"errors":[],"native":[null,[[65,7999],[152,1]]]}]},
{"name":"edge-exact-cp1251-bounded8000-char-8","source":"cp1251","form":"bounded","fixed":true,"width":8,"inputs":[null,[[65,7],[152,1]]],"oracles":[{"errors":[],"native":[null,[[65,7],[152,1]]]},{"errors":[],"native":[null,[[65,7],[152,1]]]},{"errors":[],"native":[null,[[65,7],[152,1]]]},{"errors":[],"native":[null,[[65,7],[152,1]]]}]},
{"name":"edge-over-cp1251-bounded8000-char-8","source":"cp1251","form":"bounded","fixed":true,"width":8,"inputs":[null,[[65,8],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-exact-cp1251-bounded8000-char-64","source":"cp1251","form":"bounded","fixed":true,"width":64,"inputs":[null,[[65,63],[152,1]]],"oracles":[{"errors":[],"native":[null,[[65,63],[152,1]]]},{"errors":[],"native":[null,[[65,63],[152,1]]]},{"errors":[],"native":[null,[[65,63],[152,1]]]},{"errors":[],"native":[null,[[65,63],[152,1]]]}]},
{"name":"edge-over-cp1251-bounded8000-char-64","source":"cp1251","form":"bounded","fixed":true,"width":64,"inputs":[null,[[65,64],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-exact-cp1251-bounded8000-char-4000","source":"cp1251","form":"bounded","fixed":true,"width":4000,"inputs":[null,[[65,3999],[152,1]]],"oracles":[{"errors":[],"native":[null,[[65,3999],[152,1]]]},{"errors":[],"native":[null,[[65,3999],[152,1]]]},{"errors":[],"native":[null,[[65,3999],[152,1]]]},{"errors":[],"native":[null,[[65,3999],[152,1]]]}]},
{"name":"edge-over-cp1251-bounded8000-char-4000","source":"cp1251","form":"bounded","fixed":true,"width":4000,"inputs":[null,[[65,4000],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-exact-cp1251-bounded8000-char-8000","source":"cp1251","form":"bounded","fixed":true,"width":8000,"inputs":[null,[[65,7999],[152,1]]],"oracles":[{"errors":[],"native":[null,[[65,7999],[152,1]]]},{"errors":[],"native":[null,[[65,7999],[152,1]]]},{"errors":[],"native":[null,[[65,7999],[152,1]]]},{"errors":[],"native":[null,[[65,7999],[152,1]]]}]},
{"name":"edge-exact-cp1252-bounded8000-varchar-8","source":"cp1252","form":"bounded","fixed":false,"width":8,"inputs":[null,[[65,7],[152,1]]],"oracles":[{"errors":[],"native":[null,[[65,7],[63,1]]]},{"errors":[],"native":[null,[[65,7],[63,1]]]},{"errors":[],"native":[null,[[65,7],[63,1]]]},{"errors":[],"native":[null,[[65,7],[63,1]]]}]},
{"name":"edge-over-cp1252-bounded8000-varchar-8","source":"cp1252","form":"bounded","fixed":false,"width":8,"inputs":[null,[[65,8],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-exact-cp1252-bounded8000-varchar-64","source":"cp1252","form":"bounded","fixed":false,"width":64,"inputs":[null,[[65,63],[152,1]]],"oracles":[{"errors":[],"native":[null,[[65,63],[63,1]]]},{"errors":[],"native":[null,[[65,63],[63,1]]]},{"errors":[],"native":[null,[[65,63],[63,1]]]},{"errors":[],"native":[null,[[65,63],[63,1]]]}]},
{"name":"edge-over-cp1252-bounded8000-varchar-64","source":"cp1252","form":"bounded","fixed":false,"width":64,"inputs":[null,[[65,64],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-exact-cp1252-bounded8000-varchar-4000","source":"cp1252","form":"bounded","fixed":false,"width":4000,"inputs":[null,[[65,3999],[152,1]]],"oracles":[{"errors":[],"native":[null,[[65,3999],[63,1]]]},{"errors":[],"native":[null,[[65,3999],[63,1]]]},{"errors":[],"native":[null,[[65,3999],[63,1]]]},{"errors":[],"native":[null,[[65,3999],[63,1]]]}]},
{"name":"edge-over-cp1252-bounded8000-varchar-4000","source":"cp1252","form":"bounded","fixed":false,"width":4000,"inputs":[null,[[65,4000],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-exact-cp1252-bounded8000-varchar-8000","source":"cp1252","form":"bounded","fixed":false,"width":8000,"inputs":[null,[[65,7999],[152,1]]],"oracles":[{"errors":[],"native":[null,[[65,7999],[63,1]]]},{"errors":[],"native":[null,[[65,7999],[63,1]]]},{"errors":[],"native":[null,[[65,7999],[63,1]]]},{"errors":[],"native":[null,[[65,7999],[63,1]]]}]},
{"name":"edge-exact-cp1252-bounded8000-char-8","source":"cp1252","form":"bounded","fixed":true,"width":8,"inputs":[null,[[65,7],[152,1]]],"oracles":[{"errors":[],"native":[null,[[65,7],[63,1]]]},{"errors":[],"native":[null,[[65,7],[63,1]]]},{"errors":[],"native":[null,[[65,7],[63,1]]]},{"errors":[],"native":[null,[[65,7],[63,1]]]}]},
{"name":"edge-over-cp1252-bounded8000-char-8","source":"cp1252","form":"bounded","fixed":true,"width":8,"inputs":[null,[[65,8],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-exact-cp1252-bounded8000-char-64","source":"cp1252","form":"bounded","fixed":true,"width":64,"inputs":[null,[[65,63],[152,1]]],"oracles":[{"errors":[],"native":[null,[[65,63],[63,1]]]},{"errors":[],"native":[null,[[65,63],[63,1]]]},{"errors":[],"native":[null,[[65,63],[63,1]]]},{"errors":[],"native":[null,[[65,63],[63,1]]]}]},
{"name":"edge-over-cp1252-bounded8000-char-64","source":"cp1252","form":"bounded","fixed":true,"width":64,"inputs":[null,[[65,64],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-exact-cp1252-bounded8000-char-4000","source":"cp1252","form":"bounded","fixed":true,"width":4000,"inputs":[null,[[65,3999],[152,1]]],"oracles":[{"errors":[],"native":[null,[[65,3999],[63,1]]]},{"errors":[],"native":[null,[[65,3999],[63,1]]]},{"errors":[],"native":[null,[[65,3999],[63,1]]]},{"errors":[],"native":[null,[[65,3999],[63,1]]]}]},
{"name":"edge-over-cp1252-bounded8000-char-4000","source":"cp1252","form":"bounded","fixed":true,"width":4000,"inputs":[null,[[65,4000],[90,1]]],"oracles":[{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]},{"errors":[2628],"native":[]}]},
{"name":"edge-exact-cp1252-bounded8000-char-8000","source":"cp1252","form":"bounded","fixed":true,"width":8000,"inputs":[null,[[65,7999],[152,1]]],"oracles":[{"errors":[],"native":[null,[[65,7999],[63,1]]]},{"errors":[],"native":[null,[[65,7999],[63,1]]]},{"errors":[],"native":[null,[[65,7999],[63,1]]]},{"errors":[],"native":[null,[[65,7999],[63,1]]]}]},
{"name":"complete-cp1251-max-varchar-max","source":"cp1251","form":"max","fixed":false,"width":"max","inputs":[null,[],[[0,1],[1,1],[2,1],[3,1],[4,1],[5,1],[6,1],[7,1],[8,1],[9,1],[10,1],[11,1],[12,1],[13,1],[14,1],[15,1],[16,1],[17,1],[18,1],[19,1],[20,1],[21,1],[22,1],[23,1],[24,1],[25,1],[26,1],[27,1],[28,1],[29,1],[30,1],[31,1],[32,1],[33,1],[34,1],[35,1],[36,1],[37,1],[38,1],[39,1],[40,1],[41,1],[42,1],[43,1],[44,1],[45,1],[46,1],[47,1],[48,1],[49,1],[50,1],[51,1],[52,1],[53,1],[54,1],[55,1],[56,1],[57,1],[58,1],[59,1],[60,1],[61,1],[62,1],[63,1],[64,1],[65,1],[66,1],[67,1],[68,1],[69,1],[70,1],[71,1],[72,1],[73,1],[74,1],[75,1],[76,1],[77,1],[78,1],[79,1],[80,1],[81,1],[82,1],[83,1],[84,1],[85,1],[86,1],[87,1],[88,1],[89,1],[90,1],[91,1],[92,1],[93,1],[94,1],[95,1],[96,1],[97,1],[98,1],[99,1],[100,1],[101,1],[102,1],[103,1],[104,1],[105,1],[106,1],[107,1],[108,1],[109,1],[110,1],[111,1],[112,1],[113,1],[114,1],[115,1],[116,1],[117,1],[118,1],[119,1],[120,1],[121,1],[122,1],[123,1],[124,1],[125,1],[126,1],[127,1],[128,1],[129,1],[130,1],[131,1],[132,1],[133,1],[134,1],[135,1],[136,1],[137,1],[138,1],[139,1],[140,1],[141,1],[142,1],[143,1],[144,1],[145,1],[146,1],[147,1],[148,1],[149,1],[150,1],[151,1],[152,1],[153,1],[154,1],[155,1],[156,1],[157,1],[158,1],[159,1],[160,1],[161,1],[162,1],[163,1],[164,1],[165,1],[166,1],[167,1],[168,1],[169,1],[170,1],[171,1],[172,1],[173,1],[174,1],[175,1],[176,1],[177,1],[178,1],[179,1],[180,1],[181,1],[182,1],[183,1],[184,1],[185,1],[186,1],[187,1],[188,1],[189,1],[190,1],[191,1],[192,1],[193,1],[194,1],[195,1],[196,1],[197,1],[198,1],[199,1],[200,1],[201,1],[202,1],[203,1],[204,1],[205,1],[206,1],[207,1],[208,1],[209,1],[210,1],[211,1],[212,1],[213,1],[214,1],[215,1],[216,1],[217,1],[218,1],[219,1],[220,1],[221,1],[222,1],[223,1],[224,1],[225,1],[226,1],[227,1],[228,1],[229,1],[230,1],[231,1],[232,1],[233,1],[234,1],[235,1],[236,1],[237,1],[238,1],[239,1],[240,1],[241,1],[242,1],[243,1],[244,1],[245,1],[246,1],[247,1],[248,1],[249,1],[250,1],[251,1],[252,1],[253,1],[254,1],[255,1]],[[129,1],[141,1],[144,1],[143,1],[157,1]]],"oracles":[{"errors":[],"native":[null,[],[[0,1],[1,1],[2,1],[3,1],[4,1],[5,1],[6,1],[7,1],[8,1],[9,1],[10,1],[11,1],[12,1],[13,1],[14,1],[15,1],[16,1],[17,1],[18,1],[19,1],[20,1],[21,1],[22,1],[23,1],[24,1],[25,1],[26,1],[27,1],[28,1],[29,1],[30,1],[31,1],[32,1],[33,1],[34,1],[35,1],[36,1],[37,1],[38,1],[39,1],[40,1],[41,1],[42,1],[43,1],[44,1],[45,1],[46,1],[47,1],[48,1],[49,1],[50,1],[51,1],[52,1],[53,1],[54,1],[55,1],[56,1],[57,1],[58,1],[59,1],[60,1],[61,1],[62,1],[63,1],[64,1],[65,1],[66,1],[67,1],[68,1],[69,1],[70,1],[71,1],[72,1],[73,1],[74,1],[75,1],[76,1],[77,1],[78,1],[79,1],[80,1],[81,1],[82,1],[83,1],[84,1],[85,1],[86,1],[87,1],[88,1],[89,1],[90,1],[91,1],[92,1],[93,1],[94,1],[95,1],[96,1],[97,1],[98,1],[99,1],[100,1],[101,1],[102,1],[103,1],[104,1],[105,1],[106,1],[107,1],[108,1],[109,1],[110,1],[111,1],[112,1],[113,1],[114,1],[115,1],[116,1],[117,1],[118,1],[119,1],[120,1],[121,1],[122,1],[123,1],[124,1],[125,1],[126,1],[127,1],[128,1],[129,1],[130,1],[131,1],[132,1],[133,1],[134,1],[135,1],[136,1],[137,1],[138,1],[139,1],[140,1],[141,1],[142,1],[143,1],[144,1],[145,1],[146,1],[147,1],[148,1],[149,1],[150,1],[151,1],[152,1],[153,1],[154,1],[155,1],[156,1],[157,1],[158,1],[159,1],[160,1],[161,1],[162,1],[163,1],[164,1],[165,1],[166,1],[167,1],[168,1],[169,1],[170,1],[171,1],[172,1],[173,1],[174,1],[175,1],[176,1],[177,1],[178,1],[179,1],[180,1],[181,1],[182,1],[183,1],[184,1],[185,1],[186,1],[187,1],[188,1],[189,1],[190,1],[191,1],[192,1],[193,1],[194,1],[195,1],[196,1],[197,1],[198,1],[199,1],[200,1],[201,1],[202,1],[203,1],[204,1],[205,1],[206,1],[207,1],[208,1],[209,1],[210,1],[211,1],[212,1],[213,1],[214,1],[215,1],[216,1],[217,1],[218,1],[219,1],[220,1],[221,1],[222,1],[223,1],[224,1],[225,1],[226,1],[227,1],[228,1],[229,1],[230,1],[231,1],[232,1],[233,1],[234,1],[235,1],[236,1],[237,1],[238,1],[239,1],[240,1],[241,1],[242,1],[243,1],[244,1],[245,1],[246,1],[247,1],[248,1],[249,1],[250,1],[251,1],[252,1],[253,1],[254,1],[255,1]],[[129,1],[141,1],[144,1],[143,1],[157,1]]]},{"errors":[],"native":[null,[],[[0,1],[1,1],[2,1],[3,1],[4,1],[5,1],[6,1],[7,1],[8,1],[9,1],[10,1],[11,1],[12,1],[13,1],[14,1],[15,1],[16,1],[17,1],[18,1],[19,1],[20,1],[21,1],[22,1],[23,1],[24,1],[25,1],[26,1],[27,1],[28,1],[29,1],[30,1],[31,1],[32,1],[33,1],[34,1],[35,1],[36,1],[37,1],[38,1],[39,1],[40,1],[41,1],[42,1],[43,1],[44,1],[45,1],[46,1],[47,1],[48,1],[49,1],[50,1],[51,1],[52,1],[53,1],[54,1],[55,1],[56,1],[57,1],[58,1],[59,1],[60,1],[61,1],[62,1],[63,1],[64,1],[65,1],[66,1],[67,1],[68,1],[69,1],[70,1],[71,1],[72,1],[73,1],[74,1],[75,1],[76,1],[77,1],[78,1],[79,1],[80,1],[81,1],[82,1],[83,1],[84,1],[85,1],[86,1],[87,1],[88,1],[89,1],[90,1],[91,1],[92,1],[93,1],[94,1],[95,1],[96,1],[97,1],[98,1],[99,1],[100,1],[101,1],[102,1],[103,1],[104,1],[105,1],[106,1],[107,1],[108,1],[109,1],[110,1],[111,1],[112,1],[113,1],[114,1],[115,1],[116,1],[117,1],[118,1],[119,1],[120,1],[121,1],[122,1],[123,1],[124,1],[125,1],[126,1],[127,1],[128,1],[129,1],[130,1],[131,1],[132,1],[133,1],[134,1],[135,1],[136,1],[137,1],[138,1],[139,1],[140,1],[141,1],[142,1],[143,1],[144,1],[145,1],[146,1],[147,1],[148,1],[149,1],[150,1],[151,1],[152,1],[153,1],[154,1],[155,1],[156,1],[157,1],[158,1],[159,1],[160,1],[161,1],[162,1],[163,1],[164,1],[165,1],[166,1],[167,1],[168,1],[169,1],[170,1],[171,1],[172,1],[173,1],[174,1],[175,1],[176,1],[177,1],[178,1],[179,1],[180,1],[181,1],[182,1],[183,1],[184,1],[185,1],[186,1],[187,1],[188,1],[189,1],[190,1],[191,1],[192,1],[193,1],[194,1],[195,1],[196,1],[197,1],[198,1],[199,1],[200,1],[201,1],[202,1],[203,1],[204,1],[205,1],[206,1],[207,1],[208,1],[209,1],[210,1],[211,1],[212,1],[213,1],[214,1],[215,1],[216,1],[217,1],[218,1],[219,1],[220,1],[221,1],[222,1],[223,1],[224,1],[225,1],[226,1],[227,1],[228,1],[229,1],[230,1],[231,1],[232,1],[233,1],[234,1],[235,1],[236,1],[237,1],[238,1],[239,1],[240,1],[241,1],[242,1],[243,1],[244,1],[245,1],[246,1],[247,1],[248,1],[249,1],[250,1],[251,1],[252,1],[253,1],[254,1],[255,1]],[[129,1],[141,1],[144,1],[143,1],[157,1]]]},{"errors":[],"native":[null,[],[[0,1],[1,1],[2,1],[3,1],[4,1],[5,1],[6,1],[7,1],[8,1],[9,1],[10,1],[11,1],[12,1],[13,1],[14,1],[15,1],[16,1],[17,1],[18,1],[19,1],[20,1],[21,1],[22,1],[23,1],[24,1],[25,1],[26,1],[27,1],[28,1],[29,1],[30,1],[31,1],[32,1],[33,1],[34,1],[35,1],[36,1],[37,1],[38,1],[39,1],[40,1],[41,1],[42,1],[43,1],[44,1],[45,1],[46,1],[47,1],[48,1],[49,1],[50,1],[51,1],[52,1],[53,1],[54,1],[55,1],[56,1],[57,1],[58,1],[59,1],[60,1],[61,1],[62,1],[63,1],[64,1],[65,1],[66,1],[67,1],[68,1],[69,1],[70,1],[71,1],[72,1],[73,1],[74,1],[75,1],[76,1],[77,1],[78,1],[79,1],[80,1],[81,1],[82,1],[83,1],[84,1],[85,1],[86,1],[87,1],[88,1],[89,1],[90,1],[91,1],[92,1],[93,1],[94,1],[95,1],[96,1],[97,1],[98,1],[99,1],[100,1],[101,1],[102,1],[103,1],[104,1],[105,1],[106,1],[107,1],[108,1],[109,1],[110,1],[111,1],[112,1],[113,1],[114,1],[115,1],[116,1],[117,1],[118,1],[119,1],[120,1],[121,1],[122,1],[123,1],[124,1],[125,1],[126,1],[127,1],[128,1],[129,1],[130,1],[131,1],[132,1],[133,1],[134,1],[135,1],[136,1],[137,1],[138,1],[139,1],[140,1],[141,1],[142,1],[143,1],[144,1],[145,1],[146,1],[147,1],[148,1],[149,1],[150,1],[151,1],[152,1],[153,1],[154,1],[155,1],[156,1],[157,1],[158,1],[159,1],[160,1],[161,1],[162,1],[163,1],[164,1],[165,1],[166,1],[167,1],[168,1],[169,1],[170,1],[171,1],[172,1],[173,1],[174,1],[175,1],[176,1],[177,1],[178,1],[179,1],[180,1],[181,1],[182,1],[183,1],[184,1],[185,1],[186,1],[187,1],[188,1],[189,1],[190,1],[191,1],[192,1],[193,1],[194,1],[195,1],[196,1],[197,1],[198,1],[199,1],[200,1],[201,1],[202,1],[203,1],[204,1],[205,1],[206,1],[207,1],[208,1],[209,1],[210,1],[211,1],[212,1],[213,1],[214,1],[215,1],[216,1],[217,1],[218,1],[219,1],[220,1],[221,1],[222,1],[223,1],[224,1],[225,1],[226,1],[227,1],[228,1],[229,1],[230,1],[231,1],[232,1],[233,1],[234,1],[235,1],[236,1],[237,1],[238,1],[239,1],[240,1],[241,1],[242,1],[243,1],[244,1],[245,1],[246,1],[247,1],[248,1],[249,1],[250,1],[251,1],[252,1],[253,1],[254,1],[255,1]],[[129,1],[141,1],[144,1],[143,1],[157,1]]]},{"errors":[],"native":[null,[],[[0,1],[1,1],[2,1],[3,1],[4,1],[5,1],[6,1],[7,1],[8,1],[9,1],[10,1],[11,1],[12,1],[13,1],[14,1],[15,1],[16,1],[17,1],[18,1],[19,1],[20,1],[21,1],[22,1],[23,1],[24,1],[25,1],[26,1],[27,1],[28,1],[29,1],[30,1],[31,1],[32,1],[33,1],[34,1],[35,1],[36,1],[37,1],[38,1],[39,1],[40,1],[41,1],[42,1],[43,1],[44,1],[45,1],[46,1],[47,1],[48,1],[49,1],[50,1],[51,1],[52,1],[53,1],[54,1],[55,1],[56,1],[57,1],[58,1],[59,1],[60,1],[61,1],[62,1],[63,1],[64,1],[65,1],[66,1],[67,1],[68,1],[69,1],[70,1],[71,1],[72,1],[73,1],[74,1],[75,1],[76,1],[77,1],[78,1],[79,1],[80,1],[81,1],[82,1],[83,1],[84,1],[85,1],[86,1],[87,1],[88,1],[89,1],[90,1],[91,1],[92,1],[93,1],[94,1],[95,1],[96,1],[97,1],[98,1],[99,1],[100,1],[101,1],[102,1],[103,1],[104,1],[105,1],[106,1],[107,1],[108,1],[109,1],[110,1],[111,1],[112,1],[113,1],[114,1],[115,1],[116,1],[117,1],[118,1],[119,1],[120,1],[121,1],[122,1],[123,1],[124,1],[125,1],[126,1],[127,1],[128,1],[129,1],[130,1],[131,1],[132,1],[133,1],[134,1],[135,1],[136,1],[137,1],[138,1],[139,1],[140,1],[141,1],[142,1],[143,1],[144,1],[145,1],[146,1],[147,1],[148,1],[149,1],[150,1],[151,1],[152,1],[153,1],[154,1],[155,1],[156,1],[157,1],[158,1],[159,1],[160,1],[161,1],[162,1],[163,1],[164,1],[165,1],[166,1],[167,1],[168,1],[169,1],[170,1],[171,1],[172,1],[173,1],[174,1],[175,1],[176,1],[177,1],[178,1],[179,1],[180,1],[181,1],[182,1],[183,1],[184,1],[185,1],[186,1],[187,1],[188,1],[189,1],[190,1],[191,1],[192,1],[193,1],[194,1],[195,1],[196,1],[197,1],[198,1],[199,1],[200,1],[201,1],[202,1],[203,1],[204,1],[205,1],[206,1],[207,1],[208,1],[209,1],[210,1],[211,1],[212,1],[213,1],[214,1],[215,1],[216,1],[217,1],[218,1],[219,1],[220,1],[221,1],[222,1],[223,1],[224,1],[225,1],[226,1],[227,1],[228,1],[229,1],[230,1],[231,1],[232,1],[233,1],[234,1],[235,1],[236,1],[237,1],[238,1],[239,1],[240,1],[241,1],[242,1],[243,1],[244,1],[245,1],[246,1],[247,1],[248,1],[249,1],[250,1],[251,1],[252,1],[253,1],[254,1],[255,1]],[[129,1],[141,1],[144,1],[143,1],[157,1]]]}]},
{"name":"complete-cp1252-max-varchar-max","source":"cp1252","form":"max","fixed":false,"width":"max","inputs":[null,[],[[0,1],[1,1],[2,1],[3,1],[4,1],[5,1],[6,1],[7,1],[8,1],[9,1],[10,1],[11,1],[12,1],[13,1],[14,1],[15,1],[16,1],[17,1],[18,1],[19,1],[20,1],[21,1],[22,1],[23,1],[24,1],[25,1],[26,1],[27,1],[28,1],[29,1],[30,1],[31,1],[32,1],[33,1],[34,1],[35,1],[36,1],[37,1],[38,1],[39,1],[40,1],[41,1],[42,1],[43,1],[44,1],[45,1],[46,1],[47,1],[48,1],[49,1],[50,1],[51,1],[52,1],[53,1],[54,1],[55,1],[56,1],[57,1],[58,1],[59,1],[60,1],[61,1],[62,1],[63,1],[64,1],[65,1],[66,1],[67,1],[68,1],[69,1],[70,1],[71,1],[72,1],[73,1],[74,1],[75,1],[76,1],[77,1],[78,1],[79,1],[80,1],[81,1],[82,1],[83,1],[84,1],[85,1],[86,1],[87,1],[88,1],[89,1],[90,1],[91,1],[92,1],[93,1],[94,1],[95,1],[96,1],[97,1],[98,1],[99,1],[100,1],[101,1],[102,1],[103,1],[104,1],[105,1],[106,1],[107,1],[108,1],[109,1],[110,1],[111,1],[112,1],[113,1],[114,1],[115,1],[116,1],[117,1],[118,1],[119,1],[120,1],[121,1],[122,1],[123,1],[124,1],[125,1],[126,1],[127,1],[128,1],[129,1],[130,1],[131,1],[132,1],[133,1],[134,1],[135,1],[136,1],[137,1],[138,1],[139,1],[140,1],[141,1],[142,1],[143,1],[144,1],[145,1],[146,1],[147,1],[148,1],[149,1],[150,1],[151,1],[152,1],[153,1],[154,1],[155,1],[156,1],[157,1],[158,1],[159,1],[160,1],[161,1],[162,1],[163,1],[164,1],[165,1],[166,1],[167,1],[168,1],[169,1],[170,1],[171,1],[172,1],[173,1],[174,1],[175,1],[176,1],[177,1],[178,1],[179,1],[180,1],[181,1],[182,1],[183,1],[184,1],[185,1],[186,1],[187,1],[188,1],[189,1],[190,1],[191,1],[192,1],[193,1],[194,1],[195,1],[196,1],[197,1],[198,1],[199,1],[200,1],[201,1],[202,1],[203,1],[204,1],[205,1],[206,1],[207,1],[208,1],[209,1],[210,1],[211,1],[212,1],[213,1],[214,1],[215,1],[216,1],[217,1],[218,1],[219,1],[220,1],[221,1],[222,1],[223,1],[224,1],[225,1],[226,1],[227,1],[228,1],[229,1],[230,1],[231,1],[232,1],[233,1],[234,1],[235,1],[236,1],[237,1],[238,1],[239,1],[240,1],[241,1],[242,1],[243,1],[244,1],[245,1],[246,1],[247,1],[248,1],[249,1],[250,1],[251,1],[252,1],[253,1],[254,1],[255,1]],[[129,1],[141,1],[144,1],[143,1],[157,1]]],"oracles":[{"errors":[],"native":[null,[],[[0,1],[1,1],[2,1],[3,1],[4,1],[5,1],[6,1],[7,1],[8,1],[9,1],[10,1],[11,1],[12,1],[13,1],[14,1],[15,1],[16,1],[17,1],[18,1],[19,1],[20,1],[21,1],[22,1],[23,1],[24,1],[25,1],[26,1],[27,1],[28,1],[29,1],[30,1],[31,1],[32,1],[33,1],[34,1],[35,1],[36,1],[37,1],[38,1],[39,1],[40,1],[41,1],[42,1],[43,1],[44,1],[45,1],[46,1],[47,1],[48,1],[49,1],[50,1],[51,1],[52,1],[53,1],[54,1],[55,1],[56,1],[57,1],[58,1],[59,1],[60,1],[61,1],[62,1],[63,1],[64,1],[65,1],[66,1],[67,1],[68,1],[69,1],[70,1],[71,1],[72,1],[73,1],[74,1],[75,1],[76,1],[77,1],[78,1],[79,1],[80,1],[81,1],[82,1],[83,1],[84,1],[85,1],[86,1],[87,1],[88,1],[89,1],[90,1],[91,1],[92,1],[93,1],[94,1],[95,1],[96,1],[97,1],[98,1],[99,1],[100,1],[101,1],[102,1],[103,1],[104,1],[105,1],[106,1],[107,1],[108,1],[109,1],[110,1],[111,1],[112,1],[113,1],[114,1],[115,1],[116,1],[117,1],[118,1],[119,1],[120,1],[121,1],[122,1],[123,1],[124,1],[125,1],[126,1],[127,1],[136,1],[63,1],[130,1],[63,1],[132,1],[133,1],[134,1],[135,1],[63,1],[137,1],[83,1],[139,1],[63,2],[90,1],[63,2],[145,1],[146,1],[147,1],[148,1],[149,1],[150,1],[151,1],[63,1],[153,1],[115,1],[155,1],[63,2],[122,1],[89,1],[160,1],[63,3],[164,1],[63,1],[166,1],[167,1],[63,1],[169,1],[63,1],[171,1],[172,1],[173,1],[174,1],[63,1],[176,1],[177,1],[63,3],[181,1],[182,1],[183,1],[63,3],[187,1],[63,4],[65,6],[63,1],[67,1],[69,4],[73,4],[63,1],[78,1],[79,5],[63,1],[79,1],[85,4],[89,1],[63,2],[97,6],[63,1],[99,1],[101,4],[105,4],[63,1],[110,1],[111,5],[63,1],[111,1],[117,4],[121,1],[63,1],[121,1]],[[63,5]]]},{"errors":[],"native":[null,[],[[0,1],[1,1],[2,1],[3,1],[4,1],[5,1],[6,1],[7,1],[8,1],[9,1],[10,1],[11,1],[12,1],[13,1],[14,1],[15,1],[16,1],[17,1],[18,1],[19,1],[20,1],[21,1],[22,1],[23,1],[24,1],[25,1],[26,1],[27,1],[28,1],[29,1],[30,1],[31,1],[32,1],[33,1],[34,1],[35,1],[36,1],[37,1],[38,1],[39,1],[40,1],[41,1],[42,1],[43,1],[44,1],[45,1],[46,1],[47,1],[48,1],[49,1],[50,1],[51,1],[52,1],[53,1],[54,1],[55,1],[56,1],[57,1],[58,1],[59,1],[60,1],[61,1],[62,1],[63,1],[64,1],[65,1],[66,1],[67,1],[68,1],[69,1],[70,1],[71,1],[72,1],[73,1],[74,1],[75,1],[76,1],[77,1],[78,1],[79,1],[80,1],[81,1],[82,1],[83,1],[84,1],[85,1],[86,1],[87,1],[88,1],[89,1],[90,1],[91,1],[92,1],[93,1],[94,1],[95,1],[96,1],[97,1],[98,1],[99,1],[100,1],[101,1],[102,1],[103,1],[104,1],[105,1],[106,1],[107,1],[108,1],[109,1],[110,1],[111,1],[112,1],[113,1],[114,1],[115,1],[116,1],[117,1],[118,1],[119,1],[120,1],[121,1],[122,1],[123,1],[124,1],[125,1],[126,1],[127,1],[136,1],[63,1],[130,1],[63,1],[132,1],[133,1],[134,1],[135,1],[63,1],[137,1],[83,1],[139,1],[63,2],[90,1],[63,2],[145,1],[146,1],[147,1],[148,1],[149,1],[150,1],[151,1],[63,1],[153,1],[115,1],[155,1],[63,2],[122,1],[89,1],[160,1],[63,3],[164,1],[63,1],[166,1],[167,1],[63,1],[169,1],[63,1],[171,1],[172,1],[173,1],[174,1],[63,1],[176,1],[177,1],[63,3],[181,1],[182,1],[183,1],[63,3],[187,1],[63,4],[65,6],[63,1],[67,1],[69,4],[73,4],[63,1],[78,1],[79,5],[63,1],[79,1],[85,4],[89,1],[63,2],[97,6],[63,1],[99,1],[101,4],[105,4],[63,1],[110,1],[111,5],[63,1],[111,1],[117,4],[121,1],[63,1],[121,1]],[[63,5]]]},{"errors":[],"native":[null,[],[[0,1],[1,1],[2,1],[3,1],[4,1],[5,1],[6,1],[7,1],[8,1],[9,1],[10,1],[11,1],[12,1],[13,1],[14,1],[15,1],[16,1],[17,1],[18,1],[19,1],[20,1],[21,1],[22,1],[23,1],[24,1],[25,1],[26,1],[27,1],[28,1],[29,1],[30,1],[31,1],[32,1],[33,1],[34,1],[35,1],[36,1],[37,1],[38,1],[39,1],[40,1],[41,1],[42,1],[43,1],[44,1],[45,1],[46,1],[47,1],[48,1],[49,1],[50,1],[51,1],[52,1],[53,1],[54,1],[55,1],[56,1],[57,1],[58,1],[59,1],[60,1],[61,1],[62,1],[63,1],[64,1],[65,1],[66,1],[67,1],[68,1],[69,1],[70,1],[71,1],[72,1],[73,1],[74,1],[75,1],[76,1],[77,1],[78,1],[79,1],[80,1],[81,1],[82,1],[83,1],[84,1],[85,1],[86,1],[87,1],[88,1],[89,1],[90,1],[91,1],[92,1],[93,1],[94,1],[95,1],[96,1],[97,1],[98,1],[99,1],[100,1],[101,1],[102,1],[103,1],[104,1],[105,1],[106,1],[107,1],[108,1],[109,1],[110,1],[111,1],[112,1],[113,1],[114,1],[115,1],[116,1],[117,1],[118,1],[119,1],[120,1],[121,1],[122,1],[123,1],[124,1],[125,1],[126,1],[127,1],[136,1],[63,1],[130,1],[63,1],[132,1],[133,1],[134,1],[135,1],[63,1],[137,1],[83,1],[139,1],[63,2],[90,1],[63,2],[145,1],[146,1],[147,1],[148,1],[149,1],[150,1],[151,1],[63,1],[153,1],[115,1],[155,1],[63,2],[122,1],[89,1],[160,1],[63,3],[164,1],[63,1],[166,1],[167,1],[63,1],[169,1],[63,1],[171,1],[172,1],[173,1],[174,1],[63,1],[176,1],[177,1],[63,3],[181,1],[182,1],[183,1],[63,3],[187,1],[63,4],[65,6],[63,1],[67,1],[69,4],[73,4],[63,1],[78,1],[79,5],[63,1],[79,1],[85,4],[89,1],[63,2],[97,6],[63,1],[99,1],[101,4],[105,4],[63,1],[110,1],[111,5],[63,1],[111,1],[117,4],[121,1],[63,1],[121,1]],[[63,5]]]},{"errors":[],"native":[null,[],[[0,1],[1,1],[2,1],[3,1],[4,1],[5,1],[6,1],[7,1],[8,1],[9,1],[10,1],[11,1],[12,1],[13,1],[14,1],[15,1],[16,1],[17,1],[18,1],[19,1],[20,1],[21,1],[22,1],[23,1],[24,1],[25,1],[26,1],[27,1],[28,1],[29,1],[30,1],[31,1],[32,1],[33,1],[34,1],[35,1],[36,1],[37,1],[38,1],[39,1],[40,1],[41,1],[42,1],[43,1],[44,1],[45,1],[46,1],[47,1],[48,1],[49,1],[50,1],[51,1],[52,1],[53,1],[54,1],[55,1],[56,1],[57,1],[58,1],[59,1],[60,1],[61,1],[62,1],[63,1],[64,1],[65,1],[66,1],[67,1],[68,1],[69,1],[70,1],[71,1],[72,1],[73,1],[74,1],[75,1],[76,1],[77,1],[78,1],[79,1],[80,1],[81,1],[82,1],[83,1],[84,1],[85,1],[86,1],[87,1],[88,1],[89,1],[90,1],[91,1],[92,1],[93,1],[94,1],[95,1],[96,1],[97,1],[98,1],[99,1],[100,1],[101,1],[102,1],[103,1],[104,1],[105,1],[106,1],[107,1],[108,1],[109,1],[110,1],[111,1],[112,1],[113,1],[114,1],[115,1],[116,1],[117,1],[118,1],[119,1],[120,1],[121,1],[122,1],[123,1],[124,1],[125,1],[126,1],[127,1],[136,1],[63,1],[130,1],[63,1],[132,1],[133,1],[134,1],[135,1],[63,1],[137,1],[83,1],[139,1],[63,2],[90,1],[63,2],[145,1],[146,1],[147,1],[148,1],[149,1],[150,1],[151,1],[63,1],[153,1],[115,1],[155,1],[63,2],[122,1],[89,1],[160,1],[63,3],[164,1],[63,1],[166,1],[167,1],[63,1],[169,1],[63,1],[171,1],[172,1],[173,1],[174,1],[63,1],[176,1],[177,1],[63,3],[181,1],[182,1],[183,1],[63,3],[187,1],[63,4],[65,6],[63,1],[67,1],[69,4],[73,4],[63,1],[78,1],[79,5],[63,1],[79,1],[85,4],[89,1],[63,2],[97,6],[63,1],[99,1],[101,4],[105,4],[63,1],[110,1],[111,5],[63,1],[111,1],[117,4],[121,1],[63,1],[121,1]],[[63,5]]]}]}
]"#;
