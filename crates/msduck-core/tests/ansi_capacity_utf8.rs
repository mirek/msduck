use msduck_core::ansi_bytes::{AnsiView, EncodingIdentity};
use msduck_core::ansi_conversion::capacity::{Capacity, Family, Plan, SourceForm};
use msduck_core::ansi_conversion::{ProjectedValue, ProjectionLimits, ProjectionTarget};
use serde_json::Value;

const REFERENCE: &str = include_str!("../../../reference/bulk-character-utf8-capacity.json");

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

fn projected_bytes(value: ProjectedValue) -> Vec<u8> {
    match value {
        ProjectedValue::Native(value) => value.into_parts().1,
        ProjectedValue::SqlUtf16(units) => {
            units.iter().flat_map(|unit| unit.to_le_bytes()).collect()
        }
    }
}

#[test]
fn successful_original_utf8_capacity_observations_preserve_native_and_sql_units() {
    // Some failed-load messages contain isolated surrogate escapes. Select
    // successful original observations before parsing Rust Unicode strings;
    // exact cardinalities below ensure no successful observation disappears.
    let mut selected = Vec::new();
    let mut position = 0;
    while let Some(offset) = REFERENCE[position..].find("{\"case\":") {
        let start = position + offset;
        let end = object_end(REFERENCE, start);
        let raw = &REFERENCE[start..end];
        if !raw.contains("\"errors\":[{\"number\":") {
            selected.push(serde_json::from_str::<Value>(raw).unwrap());
        }
        position = end;
    }
    let mut observations = 0;
    let mut applications = 0;
    let mut names = std::collections::BTreeMap::new();
    for observation in selected {
        if !observation["execution"]["result"]["errors"]
            .as_array()
            .unwrap()
            .is_empty()
        {
            continue;
        }
        let case = &observation["case"];
        let name = case["name"].as_str().unwrap();
        let target = match case["target"].as_str().unwrap() {
            "utf8" => ProjectionTarget::Native(EncodingIdentity::Utf8),
            "unicode" => ProjectionTarget::SqlUtf16,
            "cp1251" => ProjectionTarget::Native(EncodingIdentity::Cp1251),
            "cp1252" => ProjectionTarget::Native(EncodingIdentity::Cp1252),
            _ => panic!("unknown original target"),
        };
        let plan = Plan::new(
            EncodingIdentity::Utf8,
            if case["sourceWidth"] == "max" {
                SourceForm::Max
            } else {
                SourceForm::Bounded
            },
            target,
            if matches!(case["targetFamily"].as_str(), Some("char" | "nchar")) {
                Family::Fixed
            } else {
                Family::Variable
            },
            Capacity::Bounded(case["targetWidth"].as_u64().unwrap() as usize),
        )
        .unwrap_or_else(|error| panic!("{name}: missing capacity plan: {error}"));
        let input = observation["input"].as_array().unwrap();
        let rows = observation["readback"]["result"]["sets"][1]["rows"]
            .as_array()
            .unwrap();
        assert_eq!(input.len(), 2);
        assert_eq!(rows.len(), input.len(), "{name}");
        for (input, row) in input.iter().zip(rows) {
            let bytes = input["valueHex"].as_str().map(hex);
            let actual = plan
                .apply(
                    bytes.as_deref().map(|bytes| {
                        AnsiView::new(EncodingIdentity::Utf8, bytes, bytes.len()).unwrap()
                    }),
                    ProjectionLimits {
                        input_bytes: 1024,
                        output_bytes: 1024,
                    },
                )
                .unwrap_or_else(|error| panic!("{name}: {error}"))
                .map(projected_bytes);
            let expected = if row[2].is_null() {
                None
            } else {
                assert_eq!(row[2]["kind"], "binary");
                Some(hex(row[2]["value"].as_str().unwrap()))
            };
            assert_eq!(actual, expected, "{name}");
            applications += 1;
        }
        observations += 1;
        *names.entry(name.to_owned()).or_insert(0) += 1;
    }
    assert_eq!(observations, 248);
    assert_eq!(applications, 496);
    assert_eq!(names.len(), 62);
    assert!(names.values().all(|count| *count == 4));
}
