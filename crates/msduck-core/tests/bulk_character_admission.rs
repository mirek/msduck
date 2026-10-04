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
