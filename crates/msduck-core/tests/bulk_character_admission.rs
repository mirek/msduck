use msduck_core::{
    bulk_character_admission::{self as admission, Metadata, Row, Wire, WireLength},
    character::{CharacterType, Family, Length},
};
use serde_json::Value;

fn family(value: &Value) -> Family {
    match value.as_str().unwrap() {
        "varchar" => Family::Varchar,
        "char" => Family::Char,
        "nvarchar" => Family::Nvarchar,
        "nchar" => Family::Nchar,
        other => panic!("unmeasured family {other}"),
    }
}

#[test]
fn replay_all_original_metadata_and_source_row_observations() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../reference/bulk-character-metadata.json"
    ))
    .unwrap();
    let mut checked = 0;
    for run in fixture["runs"].as_array().unwrap() {
        for observation in run["observations"].as_array().unwrap() {
            let case = &observation["case"];
            let source_family = family(&case["sourceFamily"]);
            let length = case["sourceWidth"]
                .as_u64()
                .map_or(Length::Max, |n| Length::Bounded(u16::try_from(n).unwrap()));
            let declared = CharacterType::new(source_family, length).unwrap();
            let wire_family = family(&case["wireFamily"]);
            let wire_length = case["wireWidth"].as_u64().map_or(WireLength::Plp, |n| {
                let multiplier = if matches!(wire_family, Family::Nchar | Family::Nvarchar) {
                    2
                } else {
                    1
                };
                WireLength::BoundedBytes(u16::try_from(n * multiplier).unwrap())
            });
            let error = observation["execution"]["result"]["errors"]
                .as_array()
                .unwrap()
                .first();
            let number = error.and_then(|error| error["number"].as_i64());
            let expected = if number == Some(4816) {
                match error.unwrap()["state"].as_u64().unwrap() {
                    1 => Metadata::FamilyOrNullability,
                    2 => Metadata::MaxFraming,
                    other => panic!("unmeasured state {other}"),
                }
            } else {
                Metadata::Admitted
            };
            let actual = admission::metadata(
                declared,
                Wire {
                    family: wire_family,
                    length: wire_length,
                    nullable: true,
                },
                true,
                case["targetWidth"] == "max",
            );
            assert_eq!(actual, expected, "{}", case["name"]);
            if actual == Metadata::Admitted {
                let decisions: Vec<Row> = observation["input"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|input| {
                        admission::row(
                            declared,
                            input["valueHex"].as_str().map(|hex| hex.len() / 2),
                        )
                    })
                    .collect();
                assert_eq!(
                    decisions.contains(&Row::DeclaredLengthExceeded),
                    number == Some(4815),
                    "{}",
                    case["name"]
                );
            }
            checked += 1;
        }
    }
    assert_eq!(checked, 640);
}

#[test]
fn declarations_precede_nulls_and_lengths_are_original_bytes() {
    let declared = CharacterType::new(Family::Nvarchar, Length::Bounded(4000)).unwrap();
    assert_eq!(admission::row(declared, None), Row::Admitted);
    assert_eq!(admission::row(declared, Some(8000)), Row::Admitted);
    assert_eq!(
        admission::row(declared, Some(8001)),
        Row::DeclaredLengthExceeded
    );
    assert_eq!(
        admission::row(declared, Some(usize::MAX)),
        Row::DeclaredLengthExceeded
    );
    for length in [
        WireLength::BoundedBytes(0),
        WireLength::BoundedBytes(3),
        WireLength::BoundedBytes(8002),
    ] {
        assert_eq!(
            admission::metadata(
                declared,
                Wire {
                    family: Family::Nvarchar,
                    length,
                    nullable: true
                },
                true,
                false
            ),
            Metadata::Unknown
        );
    }
    assert_eq!(
        admission::metadata(
            declared,
            Wire {
                family: Family::Nvarchar,
                length: WireLength::BoundedBytes(2),
                nullable: false
            },
            true,
            false
        ),
        Metadata::FamilyOrNullability
    );
}

#[test]
fn fixed_source_padding_counts_original_bytes_and_preserves_null() {
    let ansi = CharacterType::new(Family::Char, Length::Bounded(8)).unwrap();
    let unicode = CharacterType::new(Family::Nchar, Length::Bounded(8)).unwrap();
    assert_eq!(admission::padding_units(ansi, None), None);
    assert_eq!(admission::padding_units(ansi, Some(0)), Some(8));
    assert_eq!(admission::padding_units(ansi, Some(1)), Some(7));
    assert_eq!(admission::padding_units(ansi, Some(8)), Some(0));
    assert_eq!(admission::padding_units(ansi, Some(9)), None);
    assert_eq!(admission::padding_units(unicode, Some(2)), Some(7));
    assert_eq!(admission::padding_units(unicode, Some(3)), None);
    assert_eq!(admission::padding_units(unicode, Some(16)), Some(0));
    assert_eq!(admission::padding_units(unicode, Some(18)), None);
    assert_eq!(admission::padding_units(unicode, Some(usize::MAX)), None);
}

// Original capacity failures can contain isolated surrogate escapes in target
// truncation messages. Extract only the unchanged declaration/input fields and
// the numeric execution identity; never rewrite those diagnostic strings.
fn object_end(text: &str, start: usize) -> usize {
    assert_eq!(text.as_bytes()[start], b'{');
    let (mut depth, mut quoted, mut escaped) = (0, false, false);
    for (offset, &byte) in text.as_bytes()[start..].iter().enumerate() {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
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
    panic!("unterminated original object")
}

fn field_object<'a>(raw: &'a str, field: &str) -> &'a str {
    let needle = format!("\"{field}\":");
    let start = raw.find(&needle).unwrap() + needle.len();
    &raw[start..object_end(raw, start)]
}

#[test]
fn original_capacity_controls_keep_metadata_before_source_and_target_capacity() {
    let reference = include_str!("../../../reference/bulk-character-capacity.json");
    let mut position = 0;
    let mut checked = 0;
    let mut metadata_failures = 0;
    let mut row_failures = 0;
    while let Some(offset) = reference[position..].find("{\"case\":") {
        let start = position + offset;
        let end = object_end(reference, start);
        let raw = &reference[start..end];
        let case: Value = serde_json::from_str(field_object(raw, "case")).unwrap();
        let declared = CharacterType::new(
            family(&case["sourceFamily"]),
            case["sourceWidth"]
                .as_u64()
                .map_or(Length::Max, |n| Length::Bounded(u16::try_from(n).unwrap())),
        )
        .unwrap();
        let wire = Wire {
            family: declared.family(),
            length: case["wireWidth"].as_u64().map_or(WireLength::Plp, |n| {
                WireLength::BoundedBytes(u16::try_from(n).unwrap())
            }),
            nullable: true,
        };
        let execution = field_object(raw, "execution");
        let result = field_object(execution, "result");
        let marker = "\"errors\":[{\"number\":";
        let number = result.find(marker).map(|offset| {
            result[offset + marker.len()..]
                .split(',')
                .next()
                .unwrap()
                .parse::<i64>()
                .unwrap()
        });
        let decision = admission::metadata(declared, wire, true, case["targetWidth"] == "max");
        if number == Some(4816) {
            assert_eq!(decision, Metadata::MaxFraming, "{}", case["name"]);
            let execution: Value = serde_json::from_str(execution).unwrap();
            assert_eq!(execution["result"]["errors"][0]["state"], 2);
            metadata_failures += 1;
        } else {
            assert_eq!(decision, Metadata::Admitted, "{}", case["name"]);
            let decisions: Vec<Row> = case["values"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| admission::row(declared, value.as_str().map(|hex| hex.len() / 2)))
                .collect();
            assert_eq!(
                decisions.contains(&Row::DeclaredLengthExceeded),
                number == Some(4815),
                "{}",
                case["name"]
            );
            if number == Some(4815) {
                row_failures += 1;
            }
        }
        checked += 1;
        position = end;
    }
    assert_eq!(checked, 384);
    assert_eq!(metadata_failures, 48);
    assert_eq!(row_failures, 8);
}
