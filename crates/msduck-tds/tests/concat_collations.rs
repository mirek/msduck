use msduck_core::result::Properties;
use msduck_tds::{Column, Type, collation::Collation, metadata};
use serde_json::Value;

const SC: &str = "Latin1_General_100_CI_AS_SC";
const UTF8: &str = "Latin1_General_100_CI_AS_SC_UTF8";

fn fixture() -> Value {
    let raw = include_str!("../../../reference/concat-ws-translate.json");
    // JSON strings cannot represent an isolated surrogate in serde_json.
    // Retain the four exact captured strings as explicit UTF-16 carriers;
    // all records/descriptors remain present and the fixture stays untouched.
    let isolated = r#""\ud83d-a""#;
    assert_eq!(raw.matches(isolated).count(), 4);
    serde_json::from_str(&raw.replace(isolated, r#"{"utf16":[55357,45,97]}"#)).unwrap()
}

#[test]
fn all_four_sc_and_utf8_observations_encode_exact_column_metadata() {
    let fixture = fixture();
    let containers = fixture["containers"].as_array().unwrap();
    assert_eq!(containers.len(), 2);
    let mut total = 0;
    let mut isolated = 0;
    for container in containers {
        let runs = container["runs"].as_array().unwrap();
        assert_eq!(runs.len(), 2);
        for run in runs {
            let records = run.as_array().unwrap();
            assert_eq!(records.len(), 116);
            let mut count = 0;
            for record in records {
                if let Some(sets) = record["result"]["sets"].as_array() {
                    for set in sets {
                        for row in set["rows"].as_array().unwrap() {
                            for value in row.as_array().unwrap() {
                                if let Some(units) = value.get("utf16") {
                                    assert_eq!(units, &serde_json::json!([0xd83d, 45, 97]));
                                    isolated += 1;
                                }
                            }
                        }
                    }
                } else {
                    assert!(record["prepared"].is_object());
                }
                let sql = record["sql"].as_str().unwrap();
                let name = if sql.contains(UTF8) {
                    UTF8
                } else if sql.contains(SC) {
                    SC
                } else {
                    continue;
                };
                let collation = Collation::for_name(name).unwrap();
                assert_eq!(
                    Collation::for_name(&name.to_ascii_lowercase()),
                    Some(collation)
                );
                for set in record["result"]["sets"].as_array().unwrap() {
                    for column in set["columns"].as_array().unwrap() {
                        assert_eq!(column["type"], "NVarChar");
                        assert_eq!(column["precision"], Value::Null);
                        assert_eq!(column["scale"], Value::Null);
                        let captured = &column["collation"];
                        let expected_collation = Collation::new(
                            captured["lcid"].as_u64().unwrap().try_into().unwrap(),
                            captured["flags"].as_u64().unwrap().try_into().unwrap(),
                            captured["version"].as_u64().unwrap().try_into().unwrap(),
                            captured["sortId"].as_u64().unwrap().try_into().unwrap(),
                        )
                        .unwrap();
                        assert_eq!(collation.bytes(), expected_collation.bytes());
                        assert_eq!(
                            collation.bytes(),
                            if name == UTF8 {
                                [0x09, 0x04, 0xd0, 0x24, 0]
                            } else {
                                [0x09, 0x04, 0xd0, 0x20, 0]
                            }
                        );
                        let length: u16 = column["length"].as_u64().unwrap().try_into().unwrap();
                        assert_eq!(length, 8000);
                        let label = column["name"].as_str().unwrap();
                        let flags: u16 = column["flags"].as_u64().unwrap().try_into().unwrap();
                        assert_eq!(flags, 33);
                        let mut encoded = Vec::new();
                        metadata(
                            &mut encoded,
                            &[Column {
                                name: label.into(),
                                kind: Type::Nvarchar(length / 2),
                                properties: Properties::expression(true),
                                collation: Some(collation),
                            }],
                        )
                        .unwrap();
                        let mut expected = vec![0x81, 1, 0];
                        expected.extend(0u32.to_le_bytes());
                        expected.extend(flags.to_le_bytes());
                        expected.push(0xe7);
                        expected.extend(length.to_le_bytes());
                        expected.extend(expected_collation.bytes());
                        let units: Vec<_> = label.encode_utf16().collect();
                        expected.push(units.len().try_into().unwrap());
                        for unit in units {
                            expected.extend(unit.to_le_bytes());
                        }
                        assert_eq!(encoded, expected, "{}", record["name"]);
                        count += 1;
                    }
                }
            }
            assert_eq!(count, 4);
            total += count;
        }
    }
    assert_eq!(total, 16);
    assert_eq!(isolated, 4);
}

#[test]
fn only_exact_captured_names_are_recognized_and_bounds_stay_checked() {
    for name in [
        "Latin1_General_100_CI_AS_SC_UTF8_unknown",
        "Latin1_General_100_CI_AS_SC_UTF80",
        "Latin1_General_100_CI_AS_SC_UTF_8",
        "Latin1_General_100_CS_AS_SC",
        "Latin1_General_90_CI_AS_SC",
        "SQL_Latin1_General_CP1_CI_AS_SC",
        "Japanese_CI_AS_SC_UTF8",
        " Latin1_General_100_CI_AS_SC",
        "Latin1_General_100_CI_AS_SC ",
    ] {
        assert!(Collation::for_name(name).is_none(), "{name}");
    }
    for name in [SC, UTF8] {
        assert_eq!(
            Collation::for_name(&name.to_ascii_uppercase()),
            Collation::for_name(name)
        );
    }
    assert!(Collation::new(0x100000, 13, 2, 0).is_err());
    assert!(Collation::new(1033, 77, 16, 0).is_err());
}
