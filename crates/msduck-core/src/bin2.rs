//! Unicode Latin1_General_100_BIN2 comparison over explicit UTF-16 units.
//! This is not a linguistic collation or a LIKE-pattern comparison.
use std::cmp::Ordering;

/// SQL comparisons space-pad the shorter operand. Trimming followed by ordinary
/// lexicographic comparison gives the wrong order for suffixes below U+0020.
pub fn compare(left: &[u16], right: &[u16]) -> Ordering {
    padded(left, right, 32)
}

/// ANSI BIN2 orders encoded code-page bytes, not their Unicode code points.
pub fn compare_bytes(left: &[u8], right: &[u8]) -> Ordering {
    padded(left, right, b' ')
}

fn padded<T: Copy + Ord>(left: &[T], right: &[T], space: T) -> Ordering {
    for index in 0..left.len().max(right.len()) {
        let left = left.get(index).copied().unwrap_or(space);
        let right = right.get(index).copied().unwrap_or(space);
        let order = left.cmp(&right);
        if order != Ordering::Equal {
            return order;
        }
    }
    Ordering::Equal
}

/// Equality-only key, suitable for hashing and uniqueness. Byte ordering is
/// deliberately not a SQL sort key. NULL policy belongs to the caller.
pub fn equality_key(units: &[u16]) -> Vec<u8> {
    units[..crate::character::len_utf16(units)]
        .iter()
        .flat_map(|unit| unit.to_be_bytes())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ansi_byte_order_matches_live_cp1252_comparisons() {
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../../../reference/bin2-ansi-comparisons.json"
        ))
        .unwrap();
        for (name, left, right) in [
            ("euro nbsp", vec![0x80], vec![0xa0]),
            ("oe nbsp", vec![0x8c], vec![0xa0]),
            ("y dia nbsp", vec![0x9f], vec![0xa0]),
            ("padding", b"a".to_vec(), b"a   ".to_vec()),
            ("nul suffix", b"a\0".to_vec(), b"a".to_vec()),
            ("surrogate cast", b"?".to_vec(), b"?".to_vec()),
        ] {
            let case = reference["results"]
                .as_array()
                .unwrap()
                .iter()
                .find(|case| case["name"] == name)
                .unwrap();
            let values = &case["reference"]["sets"][0]["rows"][0];
            let expected = if values[0] == 1 {
                Ordering::Equal
            } else if values[2] == 1 {
                Ordering::Less
            } else {
                Ordering::Greater
            };
            assert_eq!(compare_bytes(&left, &right), expected, "{name}");
            assert_eq!(compare_bytes(&right, &left), expected.reverse(), "{name}");
        }
        assert_eq!(compare(&[0x20ac], &[0xa0]), Ordering::Greater);
        assert_eq!(compare_bytes(&[0x80], &[0xa0]), Ordering::Less);
    }

    #[test]
    fn comparisons_and_equality_match_live_sql_server_vectors() {
        let reference: serde_json::Value =
            serde_json::from_str(include_str!("../../../reference/unicode-collation.json"))
                .unwrap();
        let units = |value: &serde_json::Value| {
            value
                .as_array()
                .unwrap()
                .iter()
                .map(|unit| u16::try_from(unit.as_u64().unwrap()).unwrap())
                .collect::<Vec<_>>()
        };
        let mut count = 0;
        for case in reference["results"].as_array().unwrap() {
            if case["collation"] != "Latin1_General_100_BIN2" {
                continue;
            }
            count += 1;
            assert!(case["reference"]["errors"].as_array().unwrap().is_empty());
            let left = units(&case["left"]);
            let right = units(&case["right"]);
            let expected = case["reference"]["sets"][0]["rows"][0][0]
                .as_i64()
                .unwrap()
                .cmp(&0);
            assert_eq!(compare(&left, &right), expected, "{}", case["query"]);
            assert_eq!(compare(&right, &left), expected.reverse());
            assert_eq!(
                equality_key(&left) == equality_key(&right),
                expected == Ordering::Equal
            );
        }
        assert_eq!(count, 35);
    }

    #[test]
    fn equality_keys_preserve_units_but_must_not_be_used_for_sorting() {
        assert_eq!(equality_key(&[0xd83e, 32]), [0xd8, 0x3e]);
        assert_eq!(equality_key(&[0, 0xdc00]), [0, 0, 0xdc, 0]);
        assert_eq!(equality_key(&[32, 32]), equality_key(&[]));
        assert_ne!(equality_key(&[0xd83e]), equality_key(&[0xdd86]));
        // The shorter byte key sorts first, while SQL space padding sorts last.
        assert!(equality_key(&[97]) < equality_key(&[97, 32, 0]));
        assert_eq!(compare(&[97], &[97, 32, 0]), Ordering::Greater);
        assert_eq!(compare(&[0xd83e, 0xdd86], &[0xe000]), Ordering::Less);
    }
}
