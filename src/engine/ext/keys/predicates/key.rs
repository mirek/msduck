//! Byte keys that order Unicode carriers like msduck's default binary
//! comparison: UTF-16 code units, with the shorter operand padded with
//! spaces (so trailing spaces never distinguish values).
//!
//! Trimming trailing spaces and comparing the rest is not enough: padding
//! means `N'a'` sorts after `N'a' + NCHAR(0)`, because the pad space is
//! greater than U+0000. The key therefore encodes each non-space unit
//! together with the run of spaces before it:
//!
//! - a unit below U+0020 after `k` spaces: `0x01`, `k`, the unit;
//! - a unit above U+0020 after `k` spaces: `0x03`, `k` complemented, the unit;
//! - the end of the value (the padding): `0x02`.
//!
//! Two values agree up to the first differing token. A unit below a space
//! sorts before the padding and a unit above it after; a longer run of
//! spaces before a low unit sorts later and before a high unit earlier,
//! which is exactly what comparing against the other value's units (or its
//! padding) at that position gives. Run lengths use an order-preserving,
//! prefix-free encoding, complemented for high units to reverse the order.
//! Equal keys mean equal values, so the key also serves equality, IN lists,
//! joins and sorting.

/// The order-preserving, prefix-free encoding of a run length.
fn run(out: &mut Vec<u8>, count: u32, reversed: bool) {
    let start = out.len();
    if count < 0xF0 {
        out.push(count as u8);
    } else {
        let bytes = count.to_be_bytes();
        let skip = bytes.iter().take_while(|b| **b == 0).count();
        out.push(0xF0 + (4 - skip) as u8);
        out.extend_from_slice(&bytes[skip..]);
    }
    if reversed {
        for byte in &mut out[start..] {
            *byte = !*byte;
        }
    }
}

/// The sort key of a value's UTF-16 code units.
pub(crate) fn order_key(units: &[u16]) -> Vec<u8> {
    let mut key = Vec::with_capacity(units.len() * 4 + 1);
    let mut spaces = 0u32;
    for &unit in units {
        if unit == 0x20 {
            spaces += 1;
            continue;
        }
        let high = unit > 0x20;
        key.push(if high { 0x03 } else { 0x01 });
        run(&mut key, spaces, high);
        key.extend_from_slice(&unit.to_be_bytes());
        spaces = 0;
    }
    key.push(0x02);
    key
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    fn units(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    #[test]
    fn keys_order_like_space_padded_code_units() {
        let alphabet = [
            0x00u16, 0x09, 0x1F, 0x20, 0x21, 0x41, 0x61, 0xD83E, 0xDD86, 0xE000,
        ];
        let mut values: Vec<Vec<u16>> = vec![vec![]];
        for _ in 0..4 {
            let mut next = values.clone();
            for value in &values {
                if value.len() < 3 {
                    for &unit in &alphabet {
                        let mut extended = value.clone();
                        extended.push(unit);
                        next.push(extended);
                    }
                }
            }
            values = next;
            values.sort();
            values.dedup();
        }
        // Long space runs exercise the multi-byte run encoding.
        for count in [0xEF, 0xF0, 0x1234, 0x12345] {
            let mut value = vec![0x61];
            value.extend(std::iter::repeat_n(0x20, count));
            for tail in [0x00, 0x61] {
                let mut with_tail = value.clone();
                with_tail.push(tail);
                values.push(with_tail);
            }
            values.push(value);
        }
        let keys: Vec<_> = values.iter().map(|v| order_key(v)).collect();
        for (left, left_key) in values.iter().zip(&keys) {
            for (right, right_key) in values.iter().zip(&keys) {
                assert_eq!(
                    left_key.cmp(right_key),
                    msduck_core::bin2::compare(left, right),
                    "{left:04X?} vs {right:04X?}"
                );
            }
        }
    }

    #[test]
    fn trailing_spaces_do_not_distinguish_values() {
        assert_eq!(order_key(&units("abc")), order_key(&units("abc   ")));
        assert_eq!(order_key(&units("")), order_key(&units("  ")));
        assert_ne!(order_key(&units("a b")), order_key(&units("ab")));
        assert_eq!(
            order_key(&units("a")).cmp(&order_key(&units("a\0"))),
            Ordering::Greater
        );
    }
}
