use msduck_core::encoding::encode_cp1252;
use msduck_core::types::uniqueidentifier::{compare, order_key, parse_nvarchar, parse_varchar};
use serde_json::Value;
use std::collections::HashMap;

const REFERENCE: &str = include_str!("../../../reference/guid-conversion-order.json");
const CONVERSION_REFERENCE: &str =
    include_str!("../../../reference/guid-character-conversion.json");

fn case<'a>(run: &'a [Value], name: &str) -> &'a Value {
    run.iter().find(|item| item["name"] == name).unwrap()
}

fn labels(run: &[Value], name: &str) -> Vec<String> {
    case(run, name)["result"]["sets"][0]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row[0].as_str().unwrap().to_owned())
        .collect()
}

fn binary(hex: &str) -> [u8; 16] {
    assert_eq!(hex.len(), 32);
    let mut bytes = [0; 16];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[2 * index..2 * index + 2], 16).unwrap();
    }
    bytes
}

#[test]
fn sql_server_capture_orders_mixed_endian_values_by_guid_key() {
    let capture: Value = serde_json::from_str(REFERENCE).unwrap();
    assert_eq!(capture["runs"].as_array().unwrap().len(), 2);
    let run = capture["runs"][0].as_array().unwrap();
    assert_eq!(capture["runs"][0], capture["runs"][1]);
    let observed = labels(run, "guid order");
    let binary_order = labels(run, "binary order");
    let text_order = labels(run, "text order");
    assert_ne!(observed, binary_order);
    assert_ne!(observed, text_order);

    let bytes: HashMap<String, [u8; 16]> = case(run, "guid bytes")["result"]["sets"][0]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            (
                row[0].as_str().unwrap().to_owned(),
                binary(row[1]["value"].as_str().unwrap()),
            )
        })
        .collect();
    assert_eq!(bytes.len(), observed.len());
    let mut actual: Vec<_> = bytes.keys().cloned().collect();
    actual.sort_by(|left, right| compare(&bytes[left], &bytes[right]));
    assert_eq!(actual, observed);
    actual.sort_by_key(|label| order_key(&bytes[label]));
    assert_eq!(actual, observed);
    actual.reverse();
    assert_eq!(actual, labels(run, "guid descending"));

    let first = &bytes["first_high"];
    let last = &bytes["last"];
    let comparison = &case(run, "guid comparison")["result"]["sets"][0]["rows"][0];
    assert_eq!(compare(first, last).is_lt(), comparison[0] == 1);
    assert_eq!(compare(last, first).is_lt(), comparison[1] == 1);
    assert!(compare(first, first).is_eq());
}

#[test]
fn sql_server_character_conversion_rows_and_errors_match_both_families() {
    let capture: Value = serde_json::from_str(CONVERSION_REFERENCE).unwrap();
    assert_eq!(capture["runs"].as_array().unwrap().len(), 2);
    assert_eq!(capture["runs"][0], capture["runs"][1]);
    let run = capture["runs"][0].as_array().unwrap();
    assert_eq!(run.len(), 95);
    for item in run {
        let Some(family) = item["family"].as_str() else {
            continue;
        };
        let mode = item["mode"].as_str().unwrap();
        let result = &item["result"];
        assert_eq!(result["sets"][0]["columns"][0]["type"], "UniqueIdentifier");
        assert_eq!(result["sets"][0]["columns"][0]["length"], 16);
        assert_eq!(result["sets"][0]["columns"][0]["flags"], 33);
        let Some(input) = item["input"].as_str() else {
            assert!(item["input"].is_null());
            assert_eq!(result["sets"][0]["rows"], serde_json::json!([[null]]));
            continue;
        };
        let parsed = if family == "varchar" {
            parse_varchar(&encode_cp1252(input).unwrap())
        } else {
            assert_eq!(family, "nvarchar");
            parse_nvarchar(&input.encode_utf16().collect::<Vec<_>>())
        };
        let errors = result["errors"].as_array().unwrap();
        if !errors.is_empty() {
            assert_eq!(mode, "CAST");
            assert_eq!(errors.len(), 1);
            let error = parsed.unwrap_err();
            assert_eq!(i64::from(error.number), errors[0]["number"]);
            assert_eq!(u64::from(error.state), errors[0]["state"]);
            assert_eq!(u64::from(error.severity), errors[0]["class"]);
            assert_eq!(error.message, errors[0]["message"]);
            assert!(result["sets"][0]["rows"].as_array().unwrap().is_empty());
            assert_eq!(
                item["recovery"]["sets"][0]["rows"],
                serde_json::json!([[1]])
            );
        } else if result["sets"][0]["rows"][0][0].is_null() {
            assert_eq!(mode, "TRY_CONVERT");
            assert!(parsed.is_err(), "{}", item["name"]);
        } else {
            let expected = match result["sets"][0]["rows"][0][0].as_str().unwrap() {
                "00112233-4455-6677-8899-AABBCCDDEEFF" => {
                    binary("33221100554477668899aabbccddeeff")
                }
                "00000000-0000-0000-0000-000000000000" => [0; 16],
                "FFFFFFFF-FFFF-FFFF-FFFF-FFFFFFFFFFFF" => [0xff; 16],
                other => panic!("unexpected captured GUID {other}"),
            };
            assert_eq!(parsed.unwrap(), expected, "{}", item["name"]);
        }
    }
}

#[test]
fn invalid_raw_utf16_and_cp1252_units_do_not_become_replacement_characters() {
    let valid: Vec<_> = "00112233-4455-6677-8899-aabbccddeeff"
        .encode_utf16()
        .collect();
    let mut invalid = valid.clone();
    invalid[0] = 0xd800;
    assert!(parse_nvarchar(&invalid).is_err());
    let mut bytes = b"00112233-4455-6677-8899-aabbccddeeff".to_vec();
    bytes[0] = 0xe9;
    assert!(parse_varchar(&bytes).is_err());
    invalid = valid;
    invalid.push(0xd800);
    assert!(parse_nvarchar(&invalid).is_ok());
}
