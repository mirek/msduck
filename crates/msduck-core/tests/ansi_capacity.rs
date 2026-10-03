use msduck_core::ansi_bytes::{AnsiView, EncodingIdentity};
use msduck_core::ansi_conversion::capacity::{Capacity, CapacityError, Family, Plan, SourceForm};
use msduck_core::ansi_conversion::{
    ProjectedValue, ProjectionError, ProjectionLimits, ProjectionTarget, Resource,
};
use serde_json::Value;
const CONVERSION: &str = include_str!("../../../reference/bulk-character-conversion.json");
const CAPACITY: &str = include_str!("../../../reference/bulk-character-capacity.json");
const SPACES: &str = include_str!("../../../reference/bulk-character-trailing-space.json");
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
fn all_observations(reference: &str) -> Vec<Value> {
    let mut selected = Vec::new();
    let mut position = 0;
    while let Some(offset) = reference[position..].find("{\"case\":") {
        let start = position + offset;
        let end = object_end(reference, start);
        let cs = start + "{\"case\":".len();
        let ce = object_end(reference, cs);
        let case: Value = serde_json::from_str(&reference[cs..ce]).unwrap();
        if matches!(case["declared"].as_str(), Some("cp1251" | "cp1252"))
            && matches!(case["target"].as_str(), Some("cp1252" | "utf8" | "unicode"))
            && case["declared"] == case["wire"]
            && (case.get("wireWidth").is_none() || case["sourceWidth"] == case["wireWidth"])
        {
            let o: Value = serde_json::from_str(&reference[start..end]).unwrap();
            if o["execution"]["result"]["errors"]
                .as_array()
                .unwrap()
                .iter()
                .all(|e| e["number"] == 2628)
            {
                selected.push(o);
            }
        }
        position = end;
    }
    selected
}
fn actual_bytes(value: ProjectedValue) -> Vec<u8> {
    match value {
        ProjectedValue::Native(value) => value.into_parts().1,
        ProjectedValue::SqlUtf16(units) => units.iter().flat_map(|u| u.to_le_bytes()).collect(),
    }
}
fn identity(name: &str) -> EncodingIdentity {
    match name {
        "cp1251" => EncodingIdentity::Cp1251,
        "cp1252" => EncodingIdentity::Cp1252,
        "utf8" => EncodingIdentity::Utf8,
        _ => panic!("unknownprofile"),
    }
}
fn target(name: &str) -> ProjectionTarget {
    if name == "unicode" {
        ProjectionTarget::SqlUtf16
    } else {
        ProjectionTarget::Native(identity(name))
    }
}
fn plan_for(case: &Value) -> Plan {
    let family = if matches!(case["targetFamily"].as_str(), Some("char" | "nchar")) {
        Family::Fixed
    } else {
        Family::Variable
    };
    let capacity = if case["targetWidth"] == "max" {
        Capacity::Max
    } else {
        Capacity::Bounded(case["targetWidth"].as_u64().unwrap() as usize)
    };
    Plan::new(
        identity(case["declared"].as_str().unwrap()),
        if case["sourceWidth"] == "max" {
            SourceForm::Max
        } else {
            SourceForm::Bounded
        },
        target(case["target"].as_str().unwrap()),
        family,
        capacity,
    )
    .unwrap()
}
#[test]
fn public_capacity_replays_all_admitted_original_observations() {
    let mut total = 0;
    let mut failures = 0;
    let mut rows = 0;
    for reference in [CONVERSION, CAPACITY, SPACES] {
        let selected = all_observations(reference);
        assert!(!selected.is_empty());
        let mut names = std::collections::BTreeMap::new();
        for o in selected {
            total += 1;
            let c = &o["case"];
            let name = c["name"].as_str().unwrap();
            *names.entry(name.to_owned()).or_insert(0) += 1;
            let source = identity(c["declared"].as_str().unwrap());
            let plan = plan_for(c);
            let failed = !o["execution"]["result"]["errors"]
                .as_array()
                .unwrap()
                .is_empty();
            let captured = o["readback"]["result"]["sets"][1]["rows"]
                .as_array()
                .unwrap();
            let mut rejected = false;
            for row in o["input"].as_array().unwrap() {
                rows += 1;
                let bytes = row["valueHex"].as_str().map(hex);
                let view = bytes
                    .as_deref()
                    .map(|b| AnsiView::new(source, b, b.len()).unwrap());
                let actual = plan.apply(
                    view,
                    ProjectionLimits {
                        input_bytes: 65536,
                        output_bytes: 131072,
                    },
                );
                if failed {
                    match actual {
                        Err(CapacityError::Truncation { .. }) => rejected = true,
                        Ok(_) => {}
                        Err(e) => panic!("{name}: {e:?}"),
                    }
                } else {
                    let actual = actual.unwrap_or_else(|e| panic!("{name}: {e:?}"));
                    let native = captured.iter().find(|r| r[0] == row["id"]).unwrap();
                    let expected = binary(&native[2]);
                    assert_eq!(
                        actual.map(actual_bytes),
                        expected,
                        "{name}: row{}",
                        row["id"]
                    );
                }
            }
            if failed {
                failures += 1;
                assert!(rejected, "{name}: failedload must include rejectedrow");
                assert!(captured.is_empty(), "originalwholeloadatomicity");
            }
        }
        assert!(names.values().all(|&n| n == 4), "allfouroriginalruns");
    }
    eprintln!(
        "capacity reference replay: {total} observations, {rows} rows, {failures} failed loads"
    );
    assert_eq!(total, 476);
    assert_eq!(failures, 64);
    assert_eq!(rows, 3248);
}

fn apply(
    plan: Plan,
    source: EncodingIdentity,
    bytes: &[u8],
    output_bytes: usize,
) -> Result<Option<ProjectedValue>, CapacityError> {
    plan.apply(
        Some(AnsiView::new(source, bytes, bytes.len()).unwrap()),
        ProjectionLimits {
            input_bytes: bytes.len(),
            output_bytes,
        },
    )
}

#[test]
fn complete_character_cropping_precedes_final_allocation_limits() {
    let original = [0xcf, 0xf0];
    let variable = Plan::new(
        EncodingIdentity::Cp1251,
        SourceForm::Max,
        ProjectionTarget::Native(EncodingIdentity::Utf8),
        Family::Variable,
        Capacity::Bounded(2),
    )
    .unwrap();
    let projected = apply(variable, EncodingIdentity::Cp1251, &original, 2)
        .unwrap()
        .unwrap();
    assert_eq!(actual_bytes(projected), hex("d09f"));
    assert_eq!(original, [0xcf, 0xf0]);
    let fixed = Plan::new(
        EncodingIdentity::Cp1252,
        SourceForm::Max,
        ProjectionTarget::Native(EncodingIdentity::Utf8),
        Family::Fixed,
        Capacity::Bounded(1),
    )
    .unwrap();
    assert_eq!(
        actual_bytes(
            apply(fixed, EncodingIdentity::Cp1252, &[0xa9], 1)
                .unwrap()
                .unwrap()
        ),
        [b' ']
    );
    let variable = Plan::new(
        EncodingIdentity::Cp1252,
        SourceForm::Max,
        ProjectionTarget::Native(EncodingIdentity::Utf8),
        Family::Variable,
        Capacity::Bounded(1),
    )
    .unwrap();
    assert_eq!(
        actual_bytes(
            apply(variable, EncodingIdentity::Cp1252, &[0xa9], 0)
                .unwrap()
                .unwrap()
        ),
        Vec::<u8>::new()
    );
}

#[test]
fn final_fixed_utf16_bounds_count_bytes_and_keep_null_separate() {
    let plan = Plan::new(
        EncodingIdentity::Cp1252,
        SourceForm::Max,
        ProjectionTarget::SqlUtf16,
        Family::Fixed,
        Capacity::Bounded(4),
    )
    .unwrap();
    assert_eq!(
        plan.apply(
            None,
            ProjectionLimits {
                input_bytes: 0,
                output_bytes: 0
            }
        )
        .unwrap(),
        None
    );
    assert_eq!(
        actual_bytes(
            apply(plan, EncodingIdentity::Cp1252, &[], 8)
                .unwrap()
                .unwrap()
        ),
        hex("2000200020002000")
    );
    assert_eq!(
        apply(plan, EncodingIdentity::Cp1252, &[], 7),
        Err(CapacityError::Projection(ProjectionError::Limit {
            resource: Resource::Output,
            requested: 8,
            maximum: 7
        }))
    );
}

#[test]
fn validation_rejects_unproven_profiles_and_invalid_declarations() {
    for source in [
        EncodingIdentity::Utf8,
        EncodingIdentity::Opaque(1251),
        EncodingIdentity::Opaque(1252),
        EncodingIdentity::Opaque(65001),
    ] {
        assert_eq!(
            Plan::new(
                source,
                SourceForm::Max,
                ProjectionTarget::SqlUtf16,
                Family::Variable,
                Capacity::Max
            ),
            Err(CapacityError::Projection(
                ProjectionError::UnsupportedSource(source)
            ))
        );
    }
    for target in [
        EncodingIdentity::Cp1251,
        EncodingIdentity::Opaque(1252),
        EncodingIdentity::Opaque(65001),
    ] {
        assert_eq!(
            Plan::new(
                EncodingIdentity::Cp1251,
                SourceForm::Max,
                ProjectionTarget::Native(target),
                Family::Variable,
                Capacity::Max
            ),
            Err(CapacityError::Projection(
                ProjectionError::UnsupportedTarget(target)
            ))
        );
    }
    assert_eq!(
        Plan::new(
            EncodingIdentity::Cp1252,
            SourceForm::Max,
            ProjectionTarget::SqlUtf16,
            Family::Fixed,
            Capacity::Max
        ),
        Err(CapacityError::FixedMax)
    );
    for (target, maximum) in [
        (ProjectionTarget::SqlUtf16, 4000),
        (ProjectionTarget::Native(EncodingIdentity::Utf8), 8000),
    ] {
        for requested in [0, maximum + 1, usize::MAX] {
            assert_eq!(
                Plan::new(
                    EncodingIdentity::Cp1252,
                    SourceForm::Max,
                    target,
                    Family::Variable,
                    Capacity::Bounded(requested)
                ),
                Err(CapacityError::InvalidWidth { requested, maximum })
            );
        }
    }
}

#[test]
fn carrier_mismatch_input_limits_and_max_expansion_are_checked() {
    let plan = Plan::new(
        EncodingIdentity::Cp1251,
        SourceForm::Max,
        ProjectionTarget::Native(EncodingIdentity::Utf8),
        Family::Variable,
        Capacity::Max,
    )
    .unwrap();
    assert_eq!(
        plan.apply(
            Some(AnsiView::new(EncodingIdentity::Cp1252, &[0x80], 1).unwrap()),
            ProjectionLimits {
                input_bytes: 0,
                output_bytes: 0
            }
        ),
        Err(CapacityError::Projection(
            ProjectionError::SourceEncodingMismatch {
                declared: EncodingIdentity::Cp1251,
                actual: EncodingIdentity::Cp1252
            }
        ))
    );
    assert_eq!(
        plan.apply(
            Some(AnsiView::new(EncodingIdentity::Cp1251, &[0x80], 1).unwrap()),
            ProjectionLimits {
                input_bytes: 0,
                output_bytes: 0
            }
        ),
        Err(CapacityError::Projection(ProjectionError::Limit {
            resource: Resource::Input,
            requested: 1,
            maximum: 0
        }))
    );
    assert_eq!(
        apply(plan, EncodingIdentity::Cp1251, &[0xcf, 0xf0], 3),
        Err(CapacityError::Projection(ProjectionError::Limit {
            resource: Resource::Output,
            requested: 4,
            maximum: 3
        }))
    );
    assert_eq!(
        actual_bytes(
            apply(plan, EncodingIdentity::Cp1251, &[0xcf, 0xf0], 4)
                .unwrap()
                .unwrap()
        ),
        hex("d09fd180")
    );
}

// Original native outcomes of four independently retained private probe matrices.
// All four databases agree on these rows/errors. Full raw/counter differences
// and source/artifact SHA256 provenance are retained and described in the docs.
const PRIVATE_PROBES: &str = r#"cp1251-cp1252-varchar-1-4100|cp1251|cp1252|varchar|1|4100|ERR
cp1251-cp1252-varchar-1-41a0|cp1251|cp1252|varchar|1|41a0|ERR
cp1251-cp1252-varchar-1-412042|cp1251|cp1252|varchar|1|412042|41
cp1251-cp1252-varchar-1-412000|cp1251|cp1252|varchar|1|412000|41
cp1251-cp1252-varchar-1-4120a0|cp1251|cp1252|varchar|1|4120a0|41
cp1251-cp1252-varchar-1-202041|cp1251|cp1252|varchar|1|202041|20
cp1251-cp1252-char-1-4100|cp1251|cp1252|char|1|4100|ERR
cp1251-cp1252-char-1-41a0|cp1251|cp1252|char|1|41a0|ERR
cp1251-cp1252-char-1-412042|cp1251|cp1252|char|1|412042|41
cp1251-cp1252-char-1-412000|cp1251|cp1252|char|1|412000|41
cp1251-cp1252-char-1-4120a0|cp1251|cp1252|char|1|4120a0|41
cp1251-cp1252-char-1-202041|cp1251|cp1252|char|1|202041|20
cp1251-utf8-varchar-1-4100|cp1251|utf8|varchar|1|4100|ERR
cp1251-utf8-varchar-1-41a0|cp1251|utf8|varchar|1|41a0|ERR
cp1251-utf8-varchar-1-412042|cp1251|utf8|varchar|1|412042|41
cp1251-utf8-varchar-1-412000|cp1251|utf8|varchar|1|412000|41
cp1251-utf8-varchar-1-4120a0|cp1251|utf8|varchar|1|4120a0|41
cp1251-utf8-varchar-1-202041|cp1251|utf8|varchar|1|202041|20
cp1251-utf8-char-1-4100|cp1251|utf8|char|1|4100|ERR
cp1251-utf8-char-1-41a0|cp1251|utf8|char|1|41a0|ERR
cp1251-utf8-char-1-412042|cp1251|utf8|char|1|412042|41
cp1251-utf8-char-1-412000|cp1251|utf8|char|1|412000|41
cp1251-utf8-char-1-4120a0|cp1251|utf8|char|1|4120a0|41
cp1251-utf8-char-1-202041|cp1251|utf8|char|1|202041|20
cp1251-unicode-nvarchar-1-4100|cp1251|unicode|nvarchar|1|4100|ERR
cp1251-unicode-nvarchar-1-41a0|cp1251|unicode|nvarchar|1|41a0|ERR
cp1251-unicode-nvarchar-1-412042|cp1251|unicode|nvarchar|1|412042|4100
cp1251-unicode-nvarchar-1-412000|cp1251|unicode|nvarchar|1|412000|4100
cp1251-unicode-nvarchar-1-4120a0|cp1251|unicode|nvarchar|1|4120a0|4100
cp1251-unicode-nvarchar-1-202041|cp1251|unicode|nvarchar|1|202041|2000
cp1251-unicode-nchar-1-4100|cp1251|unicode|nchar|1|4100|ERR
cp1251-unicode-nchar-1-41a0|cp1251|unicode|nchar|1|41a0|ERR
cp1251-unicode-nchar-1-412042|cp1251|unicode|nchar|1|412042|4100
cp1251-unicode-nchar-1-412000|cp1251|unicode|nchar|1|412000|4100
cp1251-unicode-nchar-1-4120a0|cp1251|unicode|nchar|1|4120a0|4100
cp1251-unicode-nchar-1-202041|cp1251|unicode|nchar|1|202041|2000
cp1251-utf8-varchar-1-a920|cp1251|utf8|varchar|1|a920|
cp1251-utf8-varchar-2-a920|cp1251|utf8|varchar|2|a920|c2a9
cp1251-utf8-char-1-a920|cp1251|utf8|char|1|a920|20
cp1251-utf8-char-2-a920|cp1251|utf8|char|2|a920|c2a9
cp1251-utf8-varchar-2-4100|cp1251|utf8|varchar|2|4100|4100
cp1251-utf8-varchar-2-41a0|cp1251|utf8|varchar|2|41a0|41
cp1251-utf8-char-2-4100|cp1251|utf8|char|2|4100|4100
cp1251-utf8-char-2-41a0|cp1251|utf8|char|2|41a0|4120
cp1251-cp1252-varchar-2-4100|cp1251|cp1252|varchar|2|4100|4100
cp1251-cp1252-varchar-2-41a0|cp1251|cp1252|varchar|2|41a0|41a0
cp1251-unicode-nvarchar-2-4100|cp1251|unicode|nvarchar|2|4100|41000000
cp1251-unicode-nvarchar-2-41a0|cp1251|unicode|nvarchar|2|41a0|4100a000
cp1252-cp1252-varchar-1-4100|cp1252|cp1252|varchar|1|4100|ERR
cp1252-cp1252-varchar-1-41a0|cp1252|cp1252|varchar|1|41a0|ERR
cp1252-cp1252-varchar-1-412042|cp1252|cp1252|varchar|1|412042|ERR
cp1252-cp1252-varchar-1-412000|cp1252|cp1252|varchar|1|412000|ERR
cp1252-cp1252-varchar-1-4120a0|cp1252|cp1252|varchar|1|4120a0|ERR
cp1252-cp1252-varchar-1-202041|cp1252|cp1252|varchar|1|202041|ERR
cp1252-cp1252-char-1-4100|cp1252|cp1252|char|1|4100|ERR
cp1252-cp1252-char-1-41a0|cp1252|cp1252|char|1|41a0|ERR
cp1252-cp1252-char-1-412042|cp1252|cp1252|char|1|412042|ERR
cp1252-cp1252-char-1-412000|cp1252|cp1252|char|1|412000|ERR
cp1252-cp1252-char-1-4120a0|cp1252|cp1252|char|1|4120a0|ERR
cp1252-cp1252-char-1-202041|cp1252|cp1252|char|1|202041|ERR
cp1252-utf8-varchar-1-4100|cp1252|utf8|varchar|1|4100|ERR
cp1252-utf8-varchar-1-41a0|cp1252|utf8|varchar|1|41a0|ERR
cp1252-utf8-varchar-1-412042|cp1252|utf8|varchar|1|412042|41
cp1252-utf8-varchar-1-412000|cp1252|utf8|varchar|1|412000|41
cp1252-utf8-varchar-1-4120a0|cp1252|utf8|varchar|1|4120a0|41
cp1252-utf8-varchar-1-202041|cp1252|utf8|varchar|1|202041|20
cp1252-utf8-char-1-4100|cp1252|utf8|char|1|4100|ERR
cp1252-utf8-char-1-41a0|cp1252|utf8|char|1|41a0|ERR
cp1252-utf8-char-1-412042|cp1252|utf8|char|1|412042|41
cp1252-utf8-char-1-412000|cp1252|utf8|char|1|412000|41
cp1252-utf8-char-1-4120a0|cp1252|utf8|char|1|4120a0|41
cp1252-utf8-char-1-202041|cp1252|utf8|char|1|202041|20
cp1252-unicode-nvarchar-1-4100|cp1252|unicode|nvarchar|1|4100|ERR
cp1252-unicode-nvarchar-1-41a0|cp1252|unicode|nvarchar|1|41a0|ERR
cp1252-unicode-nvarchar-1-412042|cp1252|unicode|nvarchar|1|412042|4100
cp1252-unicode-nvarchar-1-412000|cp1252|unicode|nvarchar|1|412000|4100
cp1252-unicode-nvarchar-1-4120a0|cp1252|unicode|nvarchar|1|4120a0|4100
cp1252-unicode-nvarchar-1-202041|cp1252|unicode|nvarchar|1|202041|2000
cp1252-unicode-nchar-1-4100|cp1252|unicode|nchar|1|4100|ERR
cp1252-unicode-nchar-1-41a0|cp1252|unicode|nchar|1|41a0|ERR
cp1252-unicode-nchar-1-412042|cp1252|unicode|nchar|1|412042|4100
cp1252-unicode-nchar-1-412000|cp1252|unicode|nchar|1|412000|4100
cp1252-unicode-nchar-1-4120a0|cp1252|unicode|nchar|1|4120a0|4100
cp1252-unicode-nchar-1-202041|cp1252|unicode|nchar|1|202041|2000
cp1252-utf8-varchar-1-a920|cp1252|utf8|varchar|1|a920|
cp1252-utf8-varchar-2-a920|cp1252|utf8|varchar|2|a920|c2a9
cp1252-utf8-char-1-a920|cp1252|utf8|char|1|a920|20
cp1252-utf8-char-2-a920|cp1252|utf8|char|2|a920|c2a9
cp1252-utf8-varchar-2-4100|cp1252|utf8|varchar|2|4100|4100
cp1252-utf8-varchar-2-41a0|cp1252|utf8|varchar|2|41a0|41
cp1252-utf8-char-2-4100|cp1252|utf8|char|2|4100|4100
cp1252-utf8-char-2-41a0|cp1252|utf8|char|2|41a0|4120
cp1252-cp1252-varchar-2-4100|cp1252|cp1252|varchar|2|4100|4100
cp1252-cp1252-varchar-2-41a0|cp1252|cp1252|varchar|2|41a0|41a0
cp1252-unicode-nvarchar-2-4100|cp1252|unicode|nvarchar|2|4100|41000000
cp1252-unicode-nvarchar-2-41a0|cp1252|unicode|nvarchar|2|41a0|4100a000
cp1251-cp1252-varchar-2-41422043|cp1251|cp1252|varchar|2|41422043|ERR
cp1251-cp1252-varchar-2-41424320|cp1251|cp1252|varchar|2|41424320|ERR
cp1251-cp1252-varchar-2-41420043|cp1251|cp1252|varchar|2|41420043|ERR
cp1251-cp1252-varchar-2-4142a043|cp1251|cp1252|varchar|2|4142a043|ERR
cp1251-cp1252-varchar-3-4142432044|cp1251|cp1252|varchar|3|4142432044|ERR
cp1251-cp1252-varchar-3-4142434420|cp1251|cp1252|varchar|3|4142434420|ERR
cp1251-cp1252-varchar-3-4142430044|cp1251|cp1252|varchar|3|4142430044|ERR
cp1251-cp1252-varchar-3-414243a044|cp1251|cp1252|varchar|3|414243a044|ERR
cp1251-cp1252-char-2-41422043|cp1251|cp1252|char|2|41422043|ERR
cp1251-cp1252-char-2-41424320|cp1251|cp1252|char|2|41424320|ERR
cp1251-cp1252-char-2-41420043|cp1251|cp1252|char|2|41420043|ERR
cp1251-cp1252-char-2-4142a043|cp1251|cp1252|char|2|4142a043|ERR
cp1251-cp1252-char-3-4142432044|cp1251|cp1252|char|3|4142432044|ERR
cp1251-cp1252-char-3-4142434420|cp1251|cp1252|char|3|4142434420|ERR
cp1251-cp1252-char-3-4142430044|cp1251|cp1252|char|3|4142430044|ERR
cp1251-cp1252-char-3-414243a044|cp1251|cp1252|char|3|414243a044|ERR
cp1251-utf8-varchar-2-41422043|cp1251|utf8|varchar|2|41422043|ERR
cp1251-utf8-varchar-2-41424320|cp1251|utf8|varchar|2|41424320|ERR
cp1251-utf8-varchar-2-41420043|cp1251|utf8|varchar|2|41420043|ERR
cp1251-utf8-varchar-2-4142a043|cp1251|utf8|varchar|2|4142a043|ERR
cp1251-utf8-varchar-3-4142432044|cp1251|utf8|varchar|3|4142432044|ERR
cp1251-utf8-varchar-3-4142434420|cp1251|utf8|varchar|3|4142434420|ERR
cp1251-utf8-varchar-3-4142430044|cp1251|utf8|varchar|3|4142430044|ERR
cp1251-utf8-varchar-3-414243a044|cp1251|utf8|varchar|3|414243a044|ERR
cp1251-utf8-char-2-41422043|cp1251|utf8|char|2|41422043|ERR
cp1251-utf8-char-2-41424320|cp1251|utf8|char|2|41424320|ERR
cp1251-utf8-char-2-41420043|cp1251|utf8|char|2|41420043|ERR
cp1251-utf8-char-2-4142a043|cp1251|utf8|char|2|4142a043|ERR
cp1251-utf8-char-3-4142432044|cp1251|utf8|char|3|4142432044|ERR
cp1251-utf8-char-3-4142434420|cp1251|utf8|char|3|4142434420|ERR
cp1251-utf8-char-3-4142430044|cp1251|utf8|char|3|4142430044|ERR
cp1251-utf8-char-3-414243a044|cp1251|utf8|char|3|414243a044|ERR
cp1251-unicode-nvarchar-2-41422043|cp1251|unicode|nvarchar|2|41422043|ERR
cp1251-unicode-nvarchar-2-41424320|cp1251|unicode|nvarchar|2|41424320|ERR
cp1251-unicode-nvarchar-2-41420043|cp1251|unicode|nvarchar|2|41420043|ERR
cp1251-unicode-nvarchar-2-4142a043|cp1251|unicode|nvarchar|2|4142a043|ERR
cp1251-unicode-nvarchar-3-4142432044|cp1251|unicode|nvarchar|3|4142432044|ERR
cp1251-unicode-nvarchar-3-4142434420|cp1251|unicode|nvarchar|3|4142434420|ERR
cp1251-unicode-nvarchar-3-4142430044|cp1251|unicode|nvarchar|3|4142430044|ERR
cp1251-unicode-nvarchar-3-414243a044|cp1251|unicode|nvarchar|3|414243a044|ERR
cp1251-unicode-nchar-2-41422043|cp1251|unicode|nchar|2|41422043|ERR
cp1251-unicode-nchar-2-41424320|cp1251|unicode|nchar|2|41424320|ERR
cp1251-unicode-nchar-2-41420043|cp1251|unicode|nchar|2|41420043|ERR
cp1251-unicode-nchar-2-4142a043|cp1251|unicode|nchar|2|4142a043|ERR
cp1251-unicode-nchar-3-4142432044|cp1251|unicode|nchar|3|4142432044|ERR
cp1251-unicode-nchar-3-4142434420|cp1251|unicode|nchar|3|4142434420|ERR
cp1251-unicode-nchar-3-4142430044|cp1251|unicode|nchar|3|4142430044|ERR
cp1251-unicode-nchar-3-414243a044|cp1251|unicode|nchar|3|414243a044|ERR
cp1252-cp1252-varchar-2-41422043|cp1252|cp1252|varchar|2|41422043|ERR
cp1252-cp1252-varchar-2-41424320|cp1252|cp1252|varchar|2|41424320|ERR
cp1252-cp1252-varchar-2-41420043|cp1252|cp1252|varchar|2|41420043|ERR
cp1252-cp1252-varchar-2-4142a043|cp1252|cp1252|varchar|2|4142a043|ERR
cp1252-cp1252-varchar-3-4142432044|cp1252|cp1252|varchar|3|4142432044|ERR
cp1252-cp1252-varchar-3-4142434420|cp1252|cp1252|varchar|3|4142434420|ERR
cp1252-cp1252-varchar-3-4142430044|cp1252|cp1252|varchar|3|4142430044|ERR
cp1252-cp1252-varchar-3-414243a044|cp1252|cp1252|varchar|3|414243a044|ERR
cp1252-cp1252-char-2-41422043|cp1252|cp1252|char|2|41422043|ERR
cp1252-cp1252-char-2-41424320|cp1252|cp1252|char|2|41424320|ERR
cp1252-cp1252-char-2-41420043|cp1252|cp1252|char|2|41420043|ERR
cp1252-cp1252-char-2-4142a043|cp1252|cp1252|char|2|4142a043|ERR
cp1252-cp1252-char-3-4142432044|cp1252|cp1252|char|3|4142432044|ERR
cp1252-cp1252-char-3-4142434420|cp1252|cp1252|char|3|4142434420|ERR
cp1252-cp1252-char-3-4142430044|cp1252|cp1252|char|3|4142430044|ERR
cp1252-cp1252-char-3-414243a044|cp1252|cp1252|char|3|414243a044|ERR
cp1252-utf8-varchar-2-41422043|cp1252|utf8|varchar|2|41422043|ERR
cp1252-utf8-varchar-2-41424320|cp1252|utf8|varchar|2|41424320|ERR
cp1252-utf8-varchar-2-41420043|cp1252|utf8|varchar|2|41420043|ERR
cp1252-utf8-varchar-2-4142a043|cp1252|utf8|varchar|2|4142a043|ERR
cp1252-utf8-varchar-3-4142432044|cp1252|utf8|varchar|3|4142432044|ERR
cp1252-utf8-varchar-3-4142434420|cp1252|utf8|varchar|3|4142434420|ERR
cp1252-utf8-varchar-3-4142430044|cp1252|utf8|varchar|3|4142430044|ERR
cp1252-utf8-varchar-3-414243a044|cp1252|utf8|varchar|3|414243a044|ERR
cp1252-utf8-char-2-41422043|cp1252|utf8|char|2|41422043|ERR
cp1252-utf8-char-2-41424320|cp1252|utf8|char|2|41424320|ERR
cp1252-utf8-char-2-41420043|cp1252|utf8|char|2|41420043|ERR
cp1252-utf8-char-2-4142a043|cp1252|utf8|char|2|4142a043|ERR
cp1252-utf8-char-3-4142432044|cp1252|utf8|char|3|4142432044|ERR
cp1252-utf8-char-3-4142434420|cp1252|utf8|char|3|4142434420|ERR
cp1252-utf8-char-3-4142430044|cp1252|utf8|char|3|4142430044|ERR
cp1252-utf8-char-3-414243a044|cp1252|utf8|char|3|414243a044|ERR
cp1252-unicode-nvarchar-2-41422043|cp1252|unicode|nvarchar|2|41422043|ERR
cp1252-unicode-nvarchar-2-41424320|cp1252|unicode|nvarchar|2|41424320|ERR
cp1252-unicode-nvarchar-2-41420043|cp1252|unicode|nvarchar|2|41420043|ERR
cp1252-unicode-nvarchar-2-4142a043|cp1252|unicode|nvarchar|2|4142a043|ERR
cp1252-unicode-nvarchar-3-4142432044|cp1252|unicode|nvarchar|3|4142432044|ERR
cp1252-unicode-nvarchar-3-4142434420|cp1252|unicode|nvarchar|3|4142434420|ERR
cp1252-unicode-nvarchar-3-4142430044|cp1252|unicode|nvarchar|3|4142430044|ERR
cp1252-unicode-nvarchar-3-414243a044|cp1252|unicode|nvarchar|3|414243a044|ERR
cp1252-unicode-nchar-2-41422043|cp1252|unicode|nchar|2|41422043|ERR
cp1252-unicode-nchar-2-41424320|cp1252|unicode|nchar|2|41424320|ERR
cp1252-unicode-nchar-2-41420043|cp1252|unicode|nchar|2|41420043|ERR
cp1252-unicode-nchar-2-4142a043|cp1252|unicode|nchar|2|4142a043|ERR
cp1252-unicode-nchar-3-4142432044|cp1252|unicode|nchar|3|4142432044|ERR
cp1252-unicode-nchar-3-4142434420|cp1252|unicode|nchar|3|4142434420|ERR
cp1252-unicode-nchar-3-4142430044|cp1252|unicode|nchar|3|4142430044|ERR
cp1252-unicode-nchar-3-414243a044|cp1252|unicode|nchar|3|414243a044|ERR
cp1251-cp1252-varchar-1-4120|cp1251|cp1252|varchar|1|4120|41
cp1251-cp1252-varchar-1-412042|cp1251|cp1252|varchar|1|412042|41
cp1251-cp1252-varchar-1-414220|cp1251|cp1252|varchar|1|414220|ERR
cp1251-cp1252-varchar-1-202041|cp1251|cp1252|varchar|1|202041|20
cp1251-cp1252-varchar-1-41422043|cp1251|cp1252|varchar|1|41422043|ERR
cp1251-cp1252-varchar-1-41204243|cp1251|cp1252|varchar|1|41204243|41
cp1251-cp1252-varchar-1-41424320|cp1251|cp1252|varchar|1|41424320|ERR
cp1251-cp1252-varchar-1-4142432044|cp1251|cp1252|varchar|1|4142432044|ERR
cp1251-cp1252-varchar-1-4142204344|cp1251|cp1252|varchar|1|4142204344|ERR
cp1251-cp1252-varchar-1-4120424344|cp1251|cp1252|varchar|1|4120424344|41
cp1251-cp1252-varchar-1-41202042|cp1251|cp1252|varchar|1|41202042|41
cp1251-cp1252-varchar-1-4142202043|cp1251|cp1252|varchar|1|4142202043|ERR
cp1251-cp1252-varchar-1-414243202044|cp1251|cp1252|varchar|1|414243202044|ERR
cp1251-cp1252-varchar-1-2020414243|cp1251|cp1252|varchar|1|2020414243|20
cp1251-cp1252-varchar-1-4120202042|cp1251|cp1252|varchar|1|4120202042|41
cp1251-cp1252-varchar-1-414220202043|cp1251|cp1252|varchar|1|414220202043|ERR
cp1251-cp1252-varchar-2-4120|cp1251|cp1252|varchar|2|4120|4120
cp1251-cp1252-varchar-2-412042|cp1251|cp1252|varchar|2|412042|ERR
cp1251-cp1252-varchar-2-414220|cp1251|cp1252|varchar|2|414220|4142
cp1251-cp1252-varchar-2-202041|cp1251|cp1252|varchar|2|202041|ERR
cp1251-cp1252-varchar-2-41422043|cp1251|cp1252|varchar|2|41422043|ERR
cp1251-cp1252-varchar-2-41204243|cp1251|cp1252|varchar|2|41204243|ERR
cp1251-cp1252-varchar-2-41424320|cp1251|cp1252|varchar|2|41424320|ERR
cp1251-cp1252-varchar-2-4142432044|cp1251|cp1252|varchar|2|4142432044|ERR
cp1251-cp1252-varchar-2-4142204344|cp1251|cp1252|varchar|2|4142204344|ERR
cp1251-cp1252-varchar-2-4120424344|cp1251|cp1252|varchar|2|4120424344|ERR
cp1251-cp1252-varchar-2-41202042|cp1251|cp1252|varchar|2|41202042|ERR
cp1251-cp1252-varchar-2-4142202043|cp1251|cp1252|varchar|2|4142202043|4142
cp1251-cp1252-varchar-2-414243202044|cp1251|cp1252|varchar|2|414243202044|ERR
cp1251-cp1252-varchar-2-2020414243|cp1251|cp1252|varchar|2|2020414243|ERR
cp1251-cp1252-varchar-2-4120202042|cp1251|cp1252|varchar|2|4120202042|4120
cp1251-cp1252-varchar-2-414220202043|cp1251|cp1252|varchar|2|414220202043|4142
cp1251-cp1252-varchar-3-4120|cp1251|cp1252|varchar|3|4120|4120
cp1251-cp1252-varchar-3-412042|cp1251|cp1252|varchar|3|412042|412042
cp1251-cp1252-varchar-3-414220|cp1251|cp1252|varchar|3|414220|414220
cp1251-cp1252-varchar-3-202041|cp1251|cp1252|varchar|3|202041|202041
cp1251-cp1252-varchar-3-41422043|cp1251|cp1252|varchar|3|41422043|ERR
cp1251-cp1252-varchar-3-41204243|cp1251|cp1252|varchar|3|41204243|ERR
cp1251-cp1252-varchar-3-41424320|cp1251|cp1252|varchar|3|41424320|414243
cp1251-cp1252-varchar-3-4142432044|cp1251|cp1252|varchar|3|4142432044|ERR
cp1251-cp1252-varchar-3-4142204344|cp1251|cp1252|varchar|3|4142204344|ERR
cp1251-cp1252-varchar-3-4120424344|cp1251|cp1252|varchar|3|4120424344|ERR
cp1251-cp1252-varchar-3-41202042|cp1251|cp1252|varchar|3|41202042|ERR
cp1251-cp1252-varchar-3-4142202043|cp1251|cp1252|varchar|3|4142202043|ERR
cp1251-cp1252-varchar-3-414243202044|cp1251|cp1252|varchar|3|414243202044|ERR
cp1251-cp1252-varchar-3-2020414243|cp1251|cp1252|varchar|3|2020414243|ERR
cp1251-cp1252-varchar-3-4120202042|cp1251|cp1252|varchar|3|4120202042|ERR
cp1251-cp1252-varchar-3-414220202043|cp1251|cp1252|varchar|3|414220202043|ERR
cp1252-cp1252-varchar-1-4120|cp1252|cp1252|varchar|1|4120|41
cp1252-cp1252-varchar-1-412042|cp1252|cp1252|varchar|1|412042|ERR
cp1252-cp1252-varchar-1-414220|cp1252|cp1252|varchar|1|414220|ERR
cp1252-cp1252-varchar-1-202041|cp1252|cp1252|varchar|1|202041|ERR
cp1252-cp1252-varchar-1-41422043|cp1252|cp1252|varchar|1|41422043|ERR
cp1252-cp1252-varchar-1-41204243|cp1252|cp1252|varchar|1|41204243|ERR
cp1252-cp1252-varchar-1-41424320|cp1252|cp1252|varchar|1|41424320|ERR
cp1252-cp1252-varchar-1-4142432044|cp1252|cp1252|varchar|1|4142432044|ERR
cp1252-cp1252-varchar-1-4142204344|cp1252|cp1252|varchar|1|4142204344|ERR
cp1252-cp1252-varchar-1-4120424344|cp1252|cp1252|varchar|1|4120424344|ERR
cp1252-cp1252-varchar-1-41202042|cp1252|cp1252|varchar|1|41202042|ERR
cp1252-cp1252-varchar-1-4142202043|cp1252|cp1252|varchar|1|4142202043|ERR
cp1252-cp1252-varchar-1-414243202044|cp1252|cp1252|varchar|1|414243202044|ERR
cp1252-cp1252-varchar-1-2020414243|cp1252|cp1252|varchar|1|2020414243|ERR
cp1252-cp1252-varchar-1-4120202042|cp1252|cp1252|varchar|1|4120202042|ERR
cp1252-cp1252-varchar-1-414220202043|cp1252|cp1252|varchar|1|414220202043|ERR
cp1252-cp1252-varchar-2-4120|cp1252|cp1252|varchar|2|4120|4120
cp1252-cp1252-varchar-2-412042|cp1252|cp1252|varchar|2|412042|ERR
cp1252-cp1252-varchar-2-414220|cp1252|cp1252|varchar|2|414220|4142
cp1252-cp1252-varchar-2-202041|cp1252|cp1252|varchar|2|202041|ERR
cp1252-cp1252-varchar-2-41422043|cp1252|cp1252|varchar|2|41422043|ERR
cp1252-cp1252-varchar-2-41204243|cp1252|cp1252|varchar|2|41204243|ERR
cp1252-cp1252-varchar-2-41424320|cp1252|cp1252|varchar|2|41424320|ERR
cp1252-cp1252-varchar-2-4142432044|cp1252|cp1252|varchar|2|4142432044|ERR
cp1252-cp1252-varchar-2-4142204344|cp1252|cp1252|varchar|2|4142204344|ERR
cp1252-cp1252-varchar-2-4120424344|cp1252|cp1252|varchar|2|4120424344|ERR
cp1252-cp1252-varchar-2-41202042|cp1252|cp1252|varchar|2|41202042|ERR
cp1252-cp1252-varchar-2-4142202043|cp1252|cp1252|varchar|2|4142202043|ERR
cp1252-cp1252-varchar-2-414243202044|cp1252|cp1252|varchar|2|414243202044|ERR
cp1252-cp1252-varchar-2-2020414243|cp1252|cp1252|varchar|2|2020414243|ERR
cp1252-cp1252-varchar-2-4120202042|cp1252|cp1252|varchar|2|4120202042|ERR
cp1252-cp1252-varchar-2-414220202043|cp1252|cp1252|varchar|2|414220202043|ERR
cp1252-cp1252-varchar-3-4120|cp1252|cp1252|varchar|3|4120|4120
cp1252-cp1252-varchar-3-412042|cp1252|cp1252|varchar|3|412042|412042
cp1252-cp1252-varchar-3-414220|cp1252|cp1252|varchar|3|414220|414220
cp1252-cp1252-varchar-3-202041|cp1252|cp1252|varchar|3|202041|202041
cp1252-cp1252-varchar-3-41422043|cp1252|cp1252|varchar|3|41422043|ERR
cp1252-cp1252-varchar-3-41204243|cp1252|cp1252|varchar|3|41204243|ERR
cp1252-cp1252-varchar-3-41424320|cp1252|cp1252|varchar|3|41424320|414243
cp1252-cp1252-varchar-3-4142432044|cp1252|cp1252|varchar|3|4142432044|ERR
cp1252-cp1252-varchar-3-4142204344|cp1252|cp1252|varchar|3|4142204344|ERR
cp1252-cp1252-varchar-3-4120424344|cp1252|cp1252|varchar|3|4120424344|ERR
cp1252-cp1252-varchar-3-41202042|cp1252|cp1252|varchar|3|41202042|ERR
cp1252-cp1252-varchar-3-4142202043|cp1252|cp1252|varchar|3|4142202043|ERR
cp1252-cp1252-varchar-3-414243202044|cp1252|cp1252|varchar|3|414243202044|ERR
cp1252-cp1252-varchar-3-2020414243|cp1252|cp1252|varchar|3|2020414243|ERR
cp1252-cp1252-varchar-3-4120202042|cp1252|cp1252|varchar|3|4120202042|ERR
cp1252-cp1252-varchar-3-414220202043|cp1252|cp1252|varchar|3|414220202043|ERR
cp1251-cp1252-varchar-3-4142432020205a|cp1251|cp1252|varchar|3|4142432020205a|414243
cp1251-cp1252-varchar-3-41424320205a20|cp1251|cp1252|varchar|3|41424320205a20|ERR
cp1251-cp1252-varchar-3-4142432020005a|cp1251|cp1252|varchar|3|4142432020005a|ERR
cp1251-cp1252-varchar-3-4142432020a05a|cp1251|cp1252|varchar|3|4142432020a05a|ERR
cp1251-cp1252-varchar-4-41424344202020205a|cp1251|cp1252|varchar|4|41424344202020205a|41424344
cp1251-cp1252-varchar-4-414243442020205a20|cp1251|cp1252|varchar|4|414243442020205a20|ERR
cp1251-cp1252-varchar-4-41424344202020005a|cp1251|cp1252|varchar|4|41424344202020005a|ERR
cp1251-cp1252-varchar-4-41424344202020a05a|cp1251|cp1252|varchar|4|41424344202020a05a|ERR
cp1251-cp1252-char-3-4142432020205a|cp1251|cp1252|char|3|4142432020205a|414243
cp1251-cp1252-char-3-41424320205a20|cp1251|cp1252|char|3|41424320205a20|ERR
cp1251-cp1252-char-3-4142432020005a|cp1251|cp1252|char|3|4142432020005a|ERR
cp1251-cp1252-char-3-4142432020a05a|cp1251|cp1252|char|3|4142432020a05a|ERR
cp1251-cp1252-char-4-41424344202020205a|cp1251|cp1252|char|4|41424344202020205a|41424344
cp1251-cp1252-char-4-414243442020205a20|cp1251|cp1252|char|4|414243442020205a20|ERR
cp1251-cp1252-char-4-41424344202020005a|cp1251|cp1252|char|4|41424344202020005a|ERR
cp1251-cp1252-char-4-41424344202020a05a|cp1251|cp1252|char|4|41424344202020a05a|ERR
cp1251-utf8-varchar-3-4142432020205a|cp1251|utf8|varchar|3|4142432020205a|414243
cp1251-utf8-varchar-3-41424320205a20|cp1251|utf8|varchar|3|41424320205a20|ERR
cp1251-utf8-varchar-3-4142432020005a|cp1251|utf8|varchar|3|4142432020005a|ERR
cp1251-utf8-varchar-3-4142432020a05a|cp1251|utf8|varchar|3|4142432020a05a|ERR
cp1251-utf8-varchar-4-41424344202020205a|cp1251|utf8|varchar|4|41424344202020205a|41424344
cp1251-utf8-varchar-4-414243442020205a20|cp1251|utf8|varchar|4|414243442020205a20|ERR
cp1251-utf8-varchar-4-41424344202020005a|cp1251|utf8|varchar|4|41424344202020005a|ERR
cp1251-utf8-varchar-4-41424344202020a05a|cp1251|utf8|varchar|4|41424344202020a05a|ERR
cp1251-utf8-char-3-4142432020205a|cp1251|utf8|char|3|4142432020205a|414243
cp1251-utf8-char-3-41424320205a20|cp1251|utf8|char|3|41424320205a20|ERR
cp1251-utf8-char-3-4142432020005a|cp1251|utf8|char|3|4142432020005a|ERR
cp1251-utf8-char-3-4142432020a05a|cp1251|utf8|char|3|4142432020a05a|ERR
cp1251-utf8-char-4-41424344202020205a|cp1251|utf8|char|4|41424344202020205a|41424344
cp1251-utf8-char-4-414243442020205a20|cp1251|utf8|char|4|414243442020205a20|ERR
cp1251-utf8-char-4-41424344202020005a|cp1251|utf8|char|4|41424344202020005a|ERR
cp1251-utf8-char-4-41424344202020a05a|cp1251|utf8|char|4|41424344202020a05a|ERR
cp1251-unicode-nvarchar-3-4142432020205a|cp1251|unicode|nvarchar|3|4142432020205a|410042004300
cp1251-unicode-nvarchar-3-41424320205a20|cp1251|unicode|nvarchar|3|41424320205a20|ERR
cp1251-unicode-nvarchar-3-4142432020005a|cp1251|unicode|nvarchar|3|4142432020005a|ERR
cp1251-unicode-nvarchar-3-4142432020a05a|cp1251|unicode|nvarchar|3|4142432020a05a|ERR
cp1251-unicode-nvarchar-4-41424344202020205a|cp1251|unicode|nvarchar|4|41424344202020205a|4100420043004400
cp1251-unicode-nvarchar-4-414243442020205a20|cp1251|unicode|nvarchar|4|414243442020205a20|ERR
cp1251-unicode-nvarchar-4-41424344202020005a|cp1251|unicode|nvarchar|4|41424344202020005a|ERR
cp1251-unicode-nvarchar-4-41424344202020a05a|cp1251|unicode|nvarchar|4|41424344202020a05a|ERR
cp1251-unicode-nchar-3-4142432020205a|cp1251|unicode|nchar|3|4142432020205a|410042004300
cp1251-unicode-nchar-3-41424320205a20|cp1251|unicode|nchar|3|41424320205a20|ERR
cp1251-unicode-nchar-3-4142432020005a|cp1251|unicode|nchar|3|4142432020005a|ERR
cp1251-unicode-nchar-3-4142432020a05a|cp1251|unicode|nchar|3|4142432020a05a|ERR
cp1251-unicode-nchar-4-41424344202020205a|cp1251|unicode|nchar|4|41424344202020205a|4100420043004400
cp1251-unicode-nchar-4-414243442020205a20|cp1251|unicode|nchar|4|414243442020205a20|ERR
cp1251-unicode-nchar-4-41424344202020005a|cp1251|unicode|nchar|4|41424344202020005a|ERR
cp1251-unicode-nchar-4-41424344202020a05a|cp1251|unicode|nchar|4|41424344202020a05a|ERR
cp1252-cp1252-varchar-3-4142432020205a|cp1252|cp1252|varchar|3|4142432020205a|ERR
cp1252-cp1252-varchar-3-41424320205a20|cp1252|cp1252|varchar|3|41424320205a20|ERR
cp1252-cp1252-varchar-3-4142432020005a|cp1252|cp1252|varchar|3|4142432020005a|ERR
cp1252-cp1252-varchar-3-4142432020a05a|cp1252|cp1252|varchar|3|4142432020a05a|ERR
cp1252-cp1252-varchar-4-41424344202020205a|cp1252|cp1252|varchar|4|41424344202020205a|ERR
cp1252-cp1252-varchar-4-414243442020205a20|cp1252|cp1252|varchar|4|414243442020205a20|ERR
cp1252-cp1252-varchar-4-41424344202020005a|cp1252|cp1252|varchar|4|41424344202020005a|ERR
cp1252-cp1252-varchar-4-41424344202020a05a|cp1252|cp1252|varchar|4|41424344202020a05a|ERR
cp1252-cp1252-char-3-4142432020205a|cp1252|cp1252|char|3|4142432020205a|ERR
cp1252-cp1252-char-3-41424320205a20|cp1252|cp1252|char|3|41424320205a20|ERR
cp1252-cp1252-char-3-4142432020005a|cp1252|cp1252|char|3|4142432020005a|ERR
cp1252-cp1252-char-3-4142432020a05a|cp1252|cp1252|char|3|4142432020a05a|ERR
cp1252-cp1252-char-4-41424344202020205a|cp1252|cp1252|char|4|41424344202020205a|ERR
cp1252-cp1252-char-4-414243442020205a20|cp1252|cp1252|char|4|414243442020205a20|ERR
cp1252-cp1252-char-4-41424344202020005a|cp1252|cp1252|char|4|41424344202020005a|ERR
cp1252-cp1252-char-4-41424344202020a05a|cp1252|cp1252|char|4|41424344202020a05a|ERR
cp1252-utf8-varchar-3-4142432020205a|cp1252|utf8|varchar|3|4142432020205a|414243
cp1252-utf8-varchar-3-41424320205a20|cp1252|utf8|varchar|3|41424320205a20|ERR
cp1252-utf8-varchar-3-4142432020005a|cp1252|utf8|varchar|3|4142432020005a|ERR
cp1252-utf8-varchar-3-4142432020a05a|cp1252|utf8|varchar|3|4142432020a05a|ERR
cp1252-utf8-varchar-4-41424344202020205a|cp1252|utf8|varchar|4|41424344202020205a|41424344
cp1252-utf8-varchar-4-414243442020205a20|cp1252|utf8|varchar|4|414243442020205a20|ERR
cp1252-utf8-varchar-4-41424344202020005a|cp1252|utf8|varchar|4|41424344202020005a|ERR
cp1252-utf8-varchar-4-41424344202020a05a|cp1252|utf8|varchar|4|41424344202020a05a|ERR
cp1252-utf8-char-3-4142432020205a|cp1252|utf8|char|3|4142432020205a|414243
cp1252-utf8-char-3-41424320205a20|cp1252|utf8|char|3|41424320205a20|ERR
cp1252-utf8-char-3-4142432020005a|cp1252|utf8|char|3|4142432020005a|ERR
cp1252-utf8-char-3-4142432020a05a|cp1252|utf8|char|3|4142432020a05a|ERR
cp1252-utf8-char-4-41424344202020205a|cp1252|utf8|char|4|41424344202020205a|41424344
cp1252-utf8-char-4-414243442020205a20|cp1252|utf8|char|4|414243442020205a20|ERR
cp1252-utf8-char-4-41424344202020005a|cp1252|utf8|char|4|41424344202020005a|ERR
cp1252-utf8-char-4-41424344202020a05a|cp1252|utf8|char|4|41424344202020a05a|ERR
cp1252-unicode-nvarchar-3-4142432020205a|cp1252|unicode|nvarchar|3|4142432020205a|410042004300
cp1252-unicode-nvarchar-3-41424320205a20|cp1252|unicode|nvarchar|3|41424320205a20|ERR
cp1252-unicode-nvarchar-3-4142432020005a|cp1252|unicode|nvarchar|3|4142432020005a|ERR
cp1252-unicode-nvarchar-3-4142432020a05a|cp1252|unicode|nvarchar|3|4142432020a05a|ERR
cp1252-unicode-nvarchar-4-41424344202020205a|cp1252|unicode|nvarchar|4|41424344202020205a|4100420043004400
cp1252-unicode-nvarchar-4-414243442020205a20|cp1252|unicode|nvarchar|4|414243442020205a20|ERR
cp1252-unicode-nvarchar-4-41424344202020005a|cp1252|unicode|nvarchar|4|41424344202020005a|ERR
cp1252-unicode-nvarchar-4-41424344202020a05a|cp1252|unicode|nvarchar|4|41424344202020a05a|ERR
cp1252-unicode-nchar-3-4142432020205a|cp1252|unicode|nchar|3|4142432020205a|410042004300
cp1252-unicode-nchar-3-41424320205a20|cp1252|unicode|nchar|3|41424320205a20|ERR
cp1252-unicode-nchar-3-4142432020005a|cp1252|unicode|nchar|3|4142432020005a|ERR
cp1252-unicode-nchar-3-4142432020a05a|cp1252|unicode|nchar|3|4142432020a05a|ERR
cp1252-unicode-nchar-4-41424344202020205a|cp1252|unicode|nchar|4|41424344202020205a|4100420043004400
cp1252-unicode-nchar-4-414243442020205a20|cp1252|unicode|nchar|4|414243442020205a20|ERR
cp1252-unicode-nchar-4-41424344202020005a|cp1252|unicode|nchar|4|41424344202020005a|ERR
cp1252-unicode-nchar-4-41424344202020a05a|cp1252|unicode|nchar|4|41424344202020a05a|ERR"#;

#[test]
fn private_probe_native_outcomes_are_preserved_as_public_vectors() {
    assert_eq!(PRIVATE_PROBES.lines().count(), 384);
    for line in PRIVATE_PROBES.lines() {
        let fields: Vec<_> = line.split('|').collect();
        let [name, source, target_name, family, width, input, expected] = fields.as_slice() else {
            panic!("invalid probe record")
        };
        let source = identity(source);
        let plan = Plan::new(
            source,
            SourceForm::Max,
            target(target_name),
            if matches!(*family, "char" | "nchar") {
                Family::Fixed
            } else {
                Family::Variable
            },
            Capacity::Bounded(width.parse().unwrap()),
        )
        .unwrap();
        assert_eq!(
            plan.apply(
                None,
                ProjectionLimits {
                    input_bytes: 0,
                    output_bytes: 0
                }
            )
            .unwrap(),
            None
        );
        let bytes = hex(input);
        let result = apply(plan, source, &bytes, 128);
        if *expected == "ERR" {
            assert!(
                matches!(result, Err(CapacityError::Truncation { .. })),
                "{name}: {result:?}"
            );
        } else {
            assert_eq!(
                actual_bytes(result.unwrap_or_else(|e| panic!("{name}: {e:?}")).unwrap()),
                hex(expected),
                "{name}"
            );
        }
    }
}

const BOUNDED_SOURCE_PROBES: &str = r#"cp1251-varchar8-cp1252-1-412042|cp1251|cp1252|412042
cp1251-varchar8-cp1252-1-412000|cp1251|cp1252|412000
cp1251-varchar8-cp1252-2-4142202043|cp1251|cp1252|4142202043
cp1251-varchar8-cp1252-2-4142202000|cp1251|cp1252|4142202000
cp1251-varchar8-utf8-1-412042|cp1251|utf8|412042
cp1251-varchar8-utf8-1-412000|cp1251|utf8|412000
cp1251-varchar8-utf8-2-4142202043|cp1251|utf8|4142202043
cp1251-varchar8-utf8-2-4142202000|cp1251|utf8|4142202000
cp1251-varchar8-unicode-1-412042|cp1251|unicode|412042
cp1251-varchar8-unicode-1-412000|cp1251|unicode|412000
cp1251-varchar8-unicode-2-4142202043|cp1251|unicode|4142202043
cp1251-varchar8-unicode-2-4142202000|cp1251|unicode|4142202000
cp1251-varchar64-cp1252-1-412042|cp1251|cp1252|412042
cp1251-varchar64-cp1252-1-412000|cp1251|cp1252|412000
cp1251-varchar64-cp1252-2-4142202043|cp1251|cp1252|4142202043
cp1251-varchar64-cp1252-2-4142202000|cp1251|cp1252|4142202000
cp1251-varchar64-utf8-1-412042|cp1251|utf8|412042
cp1251-varchar64-utf8-1-412000|cp1251|utf8|412000
cp1251-varchar64-utf8-2-4142202043|cp1251|utf8|4142202043
cp1251-varchar64-utf8-2-4142202000|cp1251|utf8|4142202000
cp1251-varchar64-unicode-1-412042|cp1251|unicode|412042
cp1251-varchar64-unicode-1-412000|cp1251|unicode|412000
cp1251-varchar64-unicode-2-4142202043|cp1251|unicode|4142202043
cp1251-varchar64-unicode-2-4142202000|cp1251|unicode|4142202000
cp1251-char8-cp1252-1-412042|cp1251|cp1252|412042
cp1251-char8-cp1252-1-412000|cp1251|cp1252|412000
cp1251-char8-cp1252-2-4142202043|cp1251|cp1252|4142202043
cp1251-char8-cp1252-2-4142202000|cp1251|cp1252|4142202000
cp1251-char8-utf8-1-412042|cp1251|utf8|412042
cp1251-char8-utf8-1-412000|cp1251|utf8|412000
cp1251-char8-utf8-2-4142202043|cp1251|utf8|4142202043
cp1251-char8-utf8-2-4142202000|cp1251|utf8|4142202000
cp1251-char8-unicode-1-412042|cp1251|unicode|412042
cp1251-char8-unicode-1-412000|cp1251|unicode|412000
cp1251-char8-unicode-2-4142202043|cp1251|unicode|4142202043
cp1251-char8-unicode-2-4142202000|cp1251|unicode|4142202000
cp1251-char64-cp1252-1-412042|cp1251|cp1252|412042
cp1251-char64-cp1252-1-412000|cp1251|cp1252|412000
cp1251-char64-cp1252-2-4142202043|cp1251|cp1252|4142202043
cp1251-char64-cp1252-2-4142202000|cp1251|cp1252|4142202000
cp1251-char64-utf8-1-412042|cp1251|utf8|412042
cp1251-char64-utf8-1-412000|cp1251|utf8|412000
cp1251-char64-utf8-2-4142202043|cp1251|utf8|4142202043
cp1251-char64-utf8-2-4142202000|cp1251|utf8|4142202000
cp1251-char64-unicode-1-412042|cp1251|unicode|412042
cp1251-char64-unicode-1-412000|cp1251|unicode|412000
cp1251-char64-unicode-2-4142202043|cp1251|unicode|4142202043
cp1251-char64-unicode-2-4142202000|cp1251|unicode|4142202000
cp1252-varchar8-cp1252-1-412042|cp1252|cp1252|412042
cp1252-varchar8-cp1252-1-412000|cp1252|cp1252|412000
cp1252-varchar8-cp1252-2-4142202043|cp1252|cp1252|4142202043
cp1252-varchar8-cp1252-2-4142202000|cp1252|cp1252|4142202000
cp1252-varchar8-utf8-1-412042|cp1252|utf8|412042
cp1252-varchar8-utf8-1-412000|cp1252|utf8|412000
cp1252-varchar8-utf8-2-4142202043|cp1252|utf8|4142202043
cp1252-varchar8-utf8-2-4142202000|cp1252|utf8|4142202000
cp1252-varchar8-unicode-1-412042|cp1252|unicode|412042
cp1252-varchar8-unicode-1-412000|cp1252|unicode|412000
cp1252-varchar8-unicode-2-4142202043|cp1252|unicode|4142202043
cp1252-varchar8-unicode-2-4142202000|cp1252|unicode|4142202000
cp1252-varchar64-cp1252-1-412042|cp1252|cp1252|412042
cp1252-varchar64-cp1252-1-412000|cp1252|cp1252|412000
cp1252-varchar64-cp1252-2-4142202043|cp1252|cp1252|4142202043
cp1252-varchar64-cp1252-2-4142202000|cp1252|cp1252|4142202000
cp1252-varchar64-utf8-1-412042|cp1252|utf8|412042
cp1252-varchar64-utf8-1-412000|cp1252|utf8|412000
cp1252-varchar64-utf8-2-4142202043|cp1252|utf8|4142202043
cp1252-varchar64-utf8-2-4142202000|cp1252|utf8|4142202000
cp1252-varchar64-unicode-1-412042|cp1252|unicode|412042
cp1252-varchar64-unicode-1-412000|cp1252|unicode|412000
cp1252-varchar64-unicode-2-4142202043|cp1252|unicode|4142202043
cp1252-varchar64-unicode-2-4142202000|cp1252|unicode|4142202000
cp1252-char8-cp1252-1-412042|cp1252|cp1252|412042
cp1252-char8-cp1252-1-412000|cp1252|cp1252|412000
cp1252-char8-cp1252-2-4142202043|cp1252|cp1252|4142202043
cp1252-char8-cp1252-2-4142202000|cp1252|cp1252|4142202000
cp1252-char8-utf8-1-412042|cp1252|utf8|412042
cp1252-char8-utf8-1-412000|cp1252|utf8|412000
cp1252-char8-utf8-2-4142202043|cp1252|utf8|4142202043
cp1252-char8-utf8-2-4142202000|cp1252|utf8|4142202000
cp1252-char8-unicode-1-412042|cp1252|unicode|412042
cp1252-char8-unicode-1-412000|cp1252|unicode|412000
cp1252-char8-unicode-2-4142202043|cp1252|unicode|4142202043
cp1252-char8-unicode-2-4142202000|cp1252|unicode|4142202000
cp1252-char64-cp1252-1-412042|cp1252|cp1252|412042
cp1252-char64-cp1252-1-412000|cp1252|cp1252|412000
cp1252-char64-cp1252-2-4142202043|cp1252|cp1252|4142202043
cp1252-char64-cp1252-2-4142202000|cp1252|cp1252|4142202000
cp1252-char64-utf8-1-412042|cp1252|utf8|412042
cp1252-char64-utf8-1-412000|cp1252|utf8|412000
cp1252-char64-utf8-2-4142202043|cp1252|utf8|4142202043
cp1252-char64-utf8-2-4142202000|cp1252|utf8|4142202000
cp1252-char64-unicode-1-412042|cp1252|unicode|412042
cp1252-char64-unicode-1-412000|cp1252|unicode|412000
cp1252-char64-unicode-2-4142202043|cp1252|unicode|4142202043
cp1252-char64-unicode-2-4142202000|cp1252|unicode|4142202000"#;
#[test]
fn bounded_varchar_and_char_sources_keep_distinct_overflow_admission() {
    assert_eq!(BOUNDED_SOURCE_PROBES.lines().count(), 96);
    for line in BOUNDED_SOURCE_PROBES.lines() {
        let fields: Vec<_> = line.split('|').collect();
        let [name, source, target_name, input] = fields.as_slice() else {
            panic!("bad source probe")
        };
        let width = if name.contains("-1-") { 1 } else { 2 };
        let source = identity(source);
        let plan = Plan::new(
            source,
            SourceForm::Bounded,
            target(target_name),
            Family::Variable,
            Capacity::Bounded(width),
        )
        .unwrap();
        let bytes = hex(input);
        assert!(
            matches!(
                apply(plan, source, &bytes, 128),
                Err(CapacityError::Truncation { .. })
            ),
            "{name}"
        );
    }
}

const LARGE_WINDOW_PROBES: &str = r#"cp1251-cp1252-varchar-8-abd26bd39e7b6ab9|cp1251|cp1252|varchar|8|spaces|OK
cp1251-cp1252-varchar-8-b5655ad03e590369|cp1251|cp1252|varchar|8|nul|ERR
cp1251-cp1252-varchar-64-b754b81fbdb6ca85|cp1251|cp1252|varchar|64|spaces|OK
cp1251-cp1252-varchar-64-157e8c7cf21baf7d|cp1251|cp1252|varchar|64|nul|ERR
cp1251-cp1252-varchar-4000-d4aeeb5c9ee376ee|cp1251|cp1252|varchar|4000|spaces|OK
cp1251-cp1252-varchar-4000-37f2bf37dd24c664|cp1251|cp1252|varchar|4000|nul|ERR
cp1251-cp1252-char-8-abd26bd39e7b6ab9|cp1251|cp1252|char|8|spaces|OK
cp1251-cp1252-char-8-b5655ad03e590369|cp1251|cp1252|char|8|nul|ERR
cp1251-cp1252-char-64-b754b81fbdb6ca85|cp1251|cp1252|char|64|spaces|OK
cp1251-cp1252-char-64-157e8c7cf21baf7d|cp1251|cp1252|char|64|nul|ERR
cp1251-cp1252-char-4000-d4aeeb5c9ee376ee|cp1251|cp1252|char|4000|spaces|OK
cp1251-cp1252-char-4000-37f2bf37dd24c664|cp1251|cp1252|char|4000|nul|ERR
cp1251-utf8-varchar-8-abd26bd39e7b6ab9|cp1251|utf8|varchar|8|spaces|OK
cp1251-utf8-varchar-8-b5655ad03e590369|cp1251|utf8|varchar|8|nul|ERR
cp1251-utf8-varchar-64-b754b81fbdb6ca85|cp1251|utf8|varchar|64|spaces|OK
cp1251-utf8-varchar-64-157e8c7cf21baf7d|cp1251|utf8|varchar|64|nul|ERR
cp1251-utf8-varchar-4000-d4aeeb5c9ee376ee|cp1251|utf8|varchar|4000|spaces|OK
cp1251-utf8-varchar-4000-37f2bf37dd24c664|cp1251|utf8|varchar|4000|nul|ERR
cp1251-utf8-char-8-abd26bd39e7b6ab9|cp1251|utf8|char|8|spaces|OK
cp1251-utf8-char-8-b5655ad03e590369|cp1251|utf8|char|8|nul|ERR
cp1251-utf8-char-64-b754b81fbdb6ca85|cp1251|utf8|char|64|spaces|OK
cp1251-utf8-char-64-157e8c7cf21baf7d|cp1251|utf8|char|64|nul|ERR
cp1251-utf8-char-4000-d4aeeb5c9ee376ee|cp1251|utf8|char|4000|spaces|OK
cp1251-utf8-char-4000-37f2bf37dd24c664|cp1251|utf8|char|4000|nul|ERR
cp1251-unicode-nvarchar-8-abd26bd39e7b6ab9|cp1251|unicode|nvarchar|8|spaces|OK
cp1251-unicode-nvarchar-8-b5655ad03e590369|cp1251|unicode|nvarchar|8|nul|ERR
cp1251-unicode-nvarchar-64-b754b81fbdb6ca85|cp1251|unicode|nvarchar|64|spaces|OK
cp1251-unicode-nvarchar-64-157e8c7cf21baf7d|cp1251|unicode|nvarchar|64|nul|ERR
cp1251-unicode-nvarchar-4000-d4aeeb5c9ee376ee|cp1251|unicode|nvarchar|4000|spaces|OK
cp1251-unicode-nvarchar-4000-37f2bf37dd24c664|cp1251|unicode|nvarchar|4000|nul|ERR
cp1251-unicode-nchar-8-abd26bd39e7b6ab9|cp1251|unicode|nchar|8|spaces|OK
cp1251-unicode-nchar-8-b5655ad03e590369|cp1251|unicode|nchar|8|nul|ERR
cp1251-unicode-nchar-64-b754b81fbdb6ca85|cp1251|unicode|nchar|64|spaces|OK
cp1251-unicode-nchar-64-157e8c7cf21baf7d|cp1251|unicode|nchar|64|nul|ERR
cp1251-unicode-nchar-4000-d4aeeb5c9ee376ee|cp1251|unicode|nchar|4000|spaces|OK
cp1251-unicode-nchar-4000-37f2bf37dd24c664|cp1251|unicode|nchar|4000|nul|ERR
cp1252-cp1252-varchar-8-abd26bd39e7b6ab9|cp1252|cp1252|varchar|8|spaces|ERR
cp1252-cp1252-varchar-8-b5655ad03e590369|cp1252|cp1252|varchar|8|nul|ERR
cp1252-cp1252-varchar-64-b754b81fbdb6ca85|cp1252|cp1252|varchar|64|spaces|ERR
cp1252-cp1252-varchar-64-157e8c7cf21baf7d|cp1252|cp1252|varchar|64|nul|ERR
cp1252-cp1252-varchar-4000-d4aeeb5c9ee376ee|cp1252|cp1252|varchar|4000|spaces|ERR
cp1252-cp1252-varchar-4000-37f2bf37dd24c664|cp1252|cp1252|varchar|4000|nul|ERR
cp1252-cp1252-char-8-abd26bd39e7b6ab9|cp1252|cp1252|char|8|spaces|ERR
cp1252-cp1252-char-8-b5655ad03e590369|cp1252|cp1252|char|8|nul|ERR
cp1252-cp1252-char-64-b754b81fbdb6ca85|cp1252|cp1252|char|64|spaces|ERR
cp1252-cp1252-char-64-157e8c7cf21baf7d|cp1252|cp1252|char|64|nul|ERR
cp1252-cp1252-char-4000-d4aeeb5c9ee376ee|cp1252|cp1252|char|4000|spaces|ERR
cp1252-cp1252-char-4000-37f2bf37dd24c664|cp1252|cp1252|char|4000|nul|ERR
cp1252-utf8-varchar-8-abd26bd39e7b6ab9|cp1252|utf8|varchar|8|spaces|OK
cp1252-utf8-varchar-8-b5655ad03e590369|cp1252|utf8|varchar|8|nul|ERR
cp1252-utf8-varchar-64-b754b81fbdb6ca85|cp1252|utf8|varchar|64|spaces|OK
cp1252-utf8-varchar-64-157e8c7cf21baf7d|cp1252|utf8|varchar|64|nul|ERR
cp1252-utf8-varchar-4000-d4aeeb5c9ee376ee|cp1252|utf8|varchar|4000|spaces|OK
cp1252-utf8-varchar-4000-37f2bf37dd24c664|cp1252|utf8|varchar|4000|nul|ERR
cp1252-utf8-char-8-abd26bd39e7b6ab9|cp1252|utf8|char|8|spaces|OK
cp1252-utf8-char-8-b5655ad03e590369|cp1252|utf8|char|8|nul|ERR
cp1252-utf8-char-64-b754b81fbdb6ca85|cp1252|utf8|char|64|spaces|OK
cp1252-utf8-char-64-157e8c7cf21baf7d|cp1252|utf8|char|64|nul|ERR
cp1252-utf8-char-4000-d4aeeb5c9ee376ee|cp1252|utf8|char|4000|spaces|OK
cp1252-utf8-char-4000-37f2bf37dd24c664|cp1252|utf8|char|4000|nul|ERR
cp1252-unicode-nvarchar-8-abd26bd39e7b6ab9|cp1252|unicode|nvarchar|8|spaces|OK
cp1252-unicode-nvarchar-8-b5655ad03e590369|cp1252|unicode|nvarchar|8|nul|ERR
cp1252-unicode-nvarchar-64-b754b81fbdb6ca85|cp1252|unicode|nvarchar|64|spaces|OK
cp1252-unicode-nvarchar-64-157e8c7cf21baf7d|cp1252|unicode|nvarchar|64|nul|ERR
cp1252-unicode-nvarchar-4000-d4aeeb5c9ee376ee|cp1252|unicode|nvarchar|4000|spaces|OK
cp1252-unicode-nvarchar-4000-37f2bf37dd24c664|cp1252|unicode|nvarchar|4000|nul|ERR
cp1252-unicode-nchar-8-abd26bd39e7b6ab9|cp1252|unicode|nchar|8|spaces|OK
cp1252-unicode-nchar-8-b5655ad03e590369|cp1252|unicode|nchar|8|nul|ERR
cp1252-unicode-nchar-64-b754b81fbdb6ca85|cp1252|unicode|nchar|64|spaces|OK
cp1252-unicode-nchar-64-157e8c7cf21baf7d|cp1252|unicode|nchar|64|nul|ERR
cp1252-unicode-nchar-4000-d4aeeb5c9ee376ee|cp1252|unicode|nchar|4000|spaces|OK
cp1252-unicode-nchar-4000-37f2bf37dd24c664|cp1252|unicode|nchar|4000|nul|ERR
cp1251-cp1252-varchar-8000-acfb9f95cd49ccb4|cp1251|cp1252|varchar|8000|spaces|OK
cp1251-cp1252-varchar-8000-a2e13150bf7befb5|cp1251|cp1252|varchar|8000|nul|ERR
cp1251-cp1252-char-8000-acfb9f95cd49ccb4|cp1251|cp1252|char|8000|spaces|OK
cp1251-cp1252-char-8000-a2e13150bf7befb5|cp1251|cp1252|char|8000|nul|ERR
cp1251-utf8-varchar-8000-acfb9f95cd49ccb4|cp1251|utf8|varchar|8000|spaces|OK
cp1251-utf8-varchar-8000-a2e13150bf7befb5|cp1251|utf8|varchar|8000|nul|ERR
cp1251-utf8-char-8000-acfb9f95cd49ccb4|cp1251|utf8|char|8000|spaces|OK
cp1251-utf8-char-8000-a2e13150bf7befb5|cp1251|utf8|char|8000|nul|ERR
cp1252-cp1252-varchar-8000-acfb9f95cd49ccb4|cp1252|cp1252|varchar|8000|spaces|ERR
cp1252-cp1252-varchar-8000-a2e13150bf7befb5|cp1252|cp1252|varchar|8000|nul|ERR
cp1252-cp1252-char-8000-acfb9f95cd49ccb4|cp1252|cp1252|char|8000|spaces|ERR
cp1252-cp1252-char-8000-a2e13150bf7befb5|cp1252|cp1252|char|8000|nul|ERR
cp1252-utf8-varchar-8000-acfb9f95cd49ccb4|cp1252|utf8|varchar|8000|spaces|OK
cp1252-utf8-varchar-8000-a2e13150bf7befb5|cp1252|utf8|varchar|8000|nul|ERR
cp1252-utf8-char-8000-acfb9f95cd49ccb4|cp1252|utf8|char|8000|spaces|OK
cp1252-utf8-char-8000-a2e13150bf7befb5|cp1252|utf8|char|8000|nul|ERR"#;
#[test]
fn original_large_capacity_probes_cover_native_and_unicode_declaration_limits() {
    assert_eq!(LARGE_WINDOW_PROBES.lines().count(), 88);
    for line in LARGE_WINDOW_PROBES.lines() {
        let fields: Vec<_> = line.split('|').collect();
        let [name, source, target_name, family, width, suffix, expected] = fields.as_slice() else {
            panic!("bad large probe")
        };
        let width: usize = width.parse().unwrap();
        let source = identity(source);
        let plan = Plan::new(
            source,
            SourceForm::Max,
            target(target_name),
            if *family == "varchar" || *family == "nvarchar" {
                Family::Variable
            } else {
                Family::Fixed
            },
            Capacity::Bounded(width),
        )
        .unwrap();
        let mut bytes = vec![b'A'; width];
        bytes.extend(std::iter::repeat_n(
            b' ',
            if *suffix == "nul" { width - 1 } else { width },
        ));
        if *suffix == "nul" {
            bytes.push(0)
        }
        bytes.push(b'Z');
        let result = apply(plan, source, &bytes, width * 2);
        if *expected == "ERR" {
            assert!(
                matches!(result, Err(CapacityError::Truncation { .. })),
                "{name}: {result:?}"
            );
        } else {
            let expected = if *target_name == "unicode" {
                hex(&"4100".repeat(width))
            } else {
                vec![b'A'; width]
            };
            assert_eq!(
                actual_bytes(result.unwrap_or_else(|e| panic!("{name}: {e:?}")).unwrap()),
                expected,
                "{name}"
            );
        }
    }
}
