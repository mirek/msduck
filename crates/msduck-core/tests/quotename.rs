#[path = "../src/quotename.rs"]
mod quotename;

use quotename::{Delimiter, quote_units};
use serde_json::{Value, json};

const REFERENCE: &str = include_str!("../../../reference/quotename.json");

fn units(value: &Value) -> Vec<u16> {
    let hex = value["value"].as_str().unwrap();
    assert_eq!(hex.len() % 4, 0);
    hex.as_bytes()
        .chunks_exact(4)
        .map(|chunk| {
            let lo = u8::from_str_radix(std::str::from_utf8(&chunk[..2]).unwrap(), 16).unwrap();
            let hi = u8::from_str_radix(std::str::from_utf8(&chunk[2..]).unwrap(), 16).unwrap();
            u16::from_le_bytes([lo, hi])
        })
        .collect()
}

#[test]
fn both_fresh_sql_server_runs_match_pure_utf16_results_and_descriptors() {
    let reference: Value = serde_json::from_str(REFERENCE).unwrap();
    let runs = reference["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0], runs[1]);
    let expected_descriptor = json!({
        "name": "quoted", "type": "NVarChar", "length": 516,
        "precision": null, "scale": null, "flags": 33,
        "collation": {"buffer": {"kind": "missing"}, "lcid": 1033,
          "flags": 13, "version": 0, "sortId": 52, "codepage": "CP1252"}
    });
    for (run_index, run) in runs.iter().enumerate() {
        let cases = run.as_array().unwrap();
        assert_eq!(cases.len(), 47);
        for case in cases {
            let name = case["name"].as_str().unwrap();
            let result = &case["result"];
            assert_eq!(result["errors"], json!([]), "{name}");
            assert_eq!(result["sets"].as_array().unwrap().len(), 1, "{name}");
            let set = &result["sets"][0];
            assert_eq!(set["columns"][0], expected_descriptor, "{name}");
            let rows = set["rows"].as_array().unwrap();
            if name == "empty projection" || name == "empty column input" {
                assert!(rows.is_empty(), "{name}");
                continue;
            }
            assert_eq!(rows.len(), 1, "{name}");
            let row = &rows[0];
            let source = (!row[2].is_null()).then(|| units(&row[2]));
            let quote_character = (!row[3].is_null()).then(|| units(&row[3]));
            let delimiter = if case["defaultDelimiter"].as_bool().unwrap() {
                Delimiter::Default
            } else if let Some(ref units) = quote_character {
                Delimiter::Explicit(units)
            } else {
                Delimiter::Null
            };
            let expected = (!row[1].is_null()).then(|| units(&row[1]));
            let actual = quote_units(source.as_deref(), delimiter);
            assert_eq!(actual, expected, "run {run_index}, {name}");
            if let Some(ref output) = actual {
                assert!(output.len() <= 258, "{name}");
                assert_eq!(row[0]["kind"], "utf16", "{name}");
                assert_eq!(row[0]["value"], row[1]["value"], "{name}");
            } else {
                assert!(row[0].is_null(), "{name}");
            }
        }
    }
}

#[test]
fn all_surrogate_units_and_maximum_escaping_remain_bounded() {
    for unit in 0xd800..=0xdfff {
        assert_eq!(
            quote_units(Some(&[unit]), Delimiter::Default),
            Some(vec![b'[' as u16, unit, b']' as u16])
        );
    }
    let closing = [b']' as u16; 128];
    let result = quote_units(Some(&closing), Delimiter::Default).unwrap();
    assert_eq!(result.len(), 258);
    assert_eq!(result[0], b'[' as u16);
    assert!(result[1..].iter().all(|unit| *unit == b']' as u16));
    assert!(quote_units(Some(&[b'a' as u16; 129]), Delimiter::Default).is_none());
}
