use msduck_tds::order::{MAX_ORDINALS, decode, encode};
use serde_json::Value;

fn captured_vectors(value: &Value, count: &mut usize) {
    match value {
        Value::Object(record) => {
            if record.get("kind").and_then(Value::as_str) == Some("ORDER") {
                let hex = record["hex"].as_str().unwrap();
                let bytes: Vec<_> = hex
                    .as_bytes()
                    .chunks_exact(2)
                    .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
                    .collect();
                let ordinals: Vec<_> = record["ordinals"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|ordinal| u16::try_from(ordinal.as_u64().unwrap()).unwrap())
                    .collect();
                let mut output = vec![0x81];
                encode(&mut output, &ordinals).unwrap();
                assert_eq!(&output[1..], bytes);
                assert_eq!(decode(&bytes).unwrap(), ordinals);
                assert_eq!(record["length"].as_u64().unwrap() as usize, bytes.len() - 3);
                *count += 1;
            }
            for child in record.values() {
                captured_vectors(child, count);
            }
        }
        Value::Array(array) => {
            for child in array {
                captured_vectors(child, count);
            }
        }
        _ => {}
    }
}

#[test]
fn every_retained_order_token_matches_exact_sql_server_bytes() {
    let reference: Value =
        serde_json::from_str(include_str!("../../../reference/order-token.json")).unwrap();
    let mut count = 0;
    captured_vectors(&reference, &mut count);
    assert_eq!(count, 124, "reference ORDER coverage changed");
}

#[test]
fn checked_byte_length_preserves_zero_duplicates_and_maximum_words() {
    let mut bytes = Vec::new();
    encode(&mut bytes, &[0, 2, 2, u16::MAX]).unwrap();
    assert_eq!(bytes, [0xa9, 8, 0, 0, 0, 2, 0, 2, 0, 255, 255]);
    assert_eq!(decode(&bytes).unwrap(), [0, 2, 2, u16::MAX]);
    let ordinals = vec![u16::MAX; MAX_ORDINALS];
    let mut maximum = Vec::new();
    encode(&mut maximum, &ordinals).unwrap();
    assert_eq!(maximum.len(), 65537);
    assert_eq!(&maximum[..3], &[0xa9, 0xfe, 0xff]);
    assert_eq!(decode(&maximum).unwrap(), ordinals);
    let mut unchanged = vec![1, 2, 3];
    assert!(encode(&mut unchanged, &vec![0; MAX_ORDINALS + 1]).is_err());
    assert_eq!(unchanged, [1, 2, 3]);
    let mut empty = Vec::new();
    encode(&mut empty, &[]).unwrap();
    assert_eq!(empty, [0xa9, 0, 0]);
    assert!(decode(&empty).unwrap().is_empty());
}

#[test]
fn rejects_truncation_odd_lengths_wrong_tags_and_trailing_bytes() {
    let valid = [0xa9, 6, 0, 0, 0, 2, 0, 1, 0];
    for end in 0..valid.len() {
        assert!(decode(&valid[..end]).is_err(), "prefix {end}");
    }
    assert!(decode(&[0xa9, 1, 0, 0]).is_err());
    assert!(decode(&[0xa8, 0, 0]).is_err());
    let mut trailing = valid.to_vec();
    trailing.push(0);
    assert!(decode(&trailing).is_err());
    assert!(decode(&[0xa9, 0xfe, 0xff]).is_err());
    let mut odd_maximum = vec![0; 65538];
    odd_maximum[..3].copy_from_slice(&[0xa9, 0xff, 0xff]);
    assert!(decode(&odd_maximum).is_err());
}
