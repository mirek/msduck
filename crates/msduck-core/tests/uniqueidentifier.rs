use msduck_core::types::uniqueidentifier::{compare, order_key};
use serde_json::Value;
use std::collections::HashMap;

const REFERENCE: &str = include_str!("../../../reference/guid-conversion-order.json");

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
