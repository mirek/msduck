//! Byte keys that order Unicode carriers like msduck's default comparison
//! (the database's case-insensitive, accent-sensitive collation): UTF-16
//! code units folded to lower case, with the shorter operand padded with
//! spaces (so trailing spaces never distinguish values).
//!
//! Case folding maps each unit through its simple lowercase mapping
//! ([`fold`]); units without a one-unit mapping stay as they are. Units the
//! collation ignores (NUL, surrogates and a few format characters; see
//! [`ignorable`]) are dropped. A first key level orders Latin letters with
//! diacritics next to their base letter ([`base`]); otherwise ordering
//! follows folded code units rather than SQL Server's sort weights (see
//! docs/unicode-collation.md).
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

/// The case-folded form of one UTF-16 unit: its simple lowercase mapping
/// when that is a single BMP unit. U+0130 maps to `i`, as DuckDB's `lower`
/// (and so the session's `nocase` collation) maps it.
pub(crate) fn fold(unit: u16) -> u16 {
    if unit == 0x130 {
        return 0x69;
    }
    let Some(character) = char::from_u32(u32::from(unit)) else {
        return unit;
    };
    let mut lower = character.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(single), None) if (single as u32) < 0x10000 => single as u16,
        _ => unit,
    }
}

pub(crate) use msduck_sql::dialect::ext::keys::value::ignorable;

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

/// The base letter of a Latin-1 or Latin Extended-A letter with a
/// diacritic (`é` -> `e`), which orders it next to that letter; other units
/// are their own base.
pub(crate) fn base(unit: u16) -> u16 {
    const LATIN1: &[u8; 64] =
        b"AAAAAA\0CEEEEIIII\0NOOOOO\0\0UUUUY\0\0aaaaaa\0ceeeeiiii\0nooooo\0\0uuuuy\0y";
    const EXTENDED: &[u8; 128] = b"AaAaAaCcCcCcCcDd\0\0EeEeEeEeEeGgGgGgGgHh\0\0IiIiIiIiI\0\0\0Jj\
Kk\0LlLlLl\0\0\0\0NnNnNn\0\0\0OoOoOo\0\0RrRrRrSsSsSsSsTtTt\0\0UuUuUuUuUuUuWwYyYZzZzZz\0";
    let base = match unit {
        0xC0..=0xFF => LATIN1[usize::from(unit - 0xC0)],
        0x100..=0x17F => EXTENDED[usize::from(unit - 0x100)],
        _ => 0,
    };
    if base == 0 { unit } else { u16::from(base) }
}

/// One level of a sort key: each non-space unit with the run of spaces
/// before it, then the end of the value (see the module documentation).
fn level(key: &mut Vec<u8>, units: impl Iterator<Item = u16>) {
    let mut spaces = 0u32;
    for unit in units {
        if unit == 0x20 {
            spaces += 1;
            continue;
        }
        let high = unit > 0x20;
        key.push(if high { 0x03 } else { 0x01 });
        run(key, spaces, high);
        key.extend_from_slice(&unit.to_be_bytes());
        spaces = 0;
    }
    key.push(0x02);
}

/// The case-insensitive sort key of a value's UTF-16 code units. Units the
/// default collation ignores are dropped. The first level orders letters
/// with diacritics next to their base letter; the second, the case-folded
/// units, decides equality and orders the rest.
pub(crate) fn order_key(units: &[u16]) -> Vec<u8> {
    let folded: Vec<u16> = units
        .iter()
        .filter(|unit| !ignorable(**unit))
        .map(|&unit| fold(unit))
        .collect();
    let mut key = Vec::with_capacity(folded.len() * 8 + 2);
    level(&mut key, folded.iter().map(|&unit| fold(base(unit))));
    level(&mut key, folded.iter().copied());
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
    fn keys_order_like_space_padded_folded_units() {
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
        // Without letters that have diacritics both key levels agree, so the
        // key orders like the padded comparison of the folded units that
        // remain once ignorable units are dropped.
        let folded: Vec<Vec<u16>> = values
            .iter()
            .map(|v| {
                v.iter()
                    .filter(|u| !ignorable(**u))
                    .map(|&u| fold(u))
                    .collect()
            })
            .collect();
        for ((left, left_key), left_folded) in values.iter().zip(&keys).zip(&folded) {
            for ((right, right_key), right_folded) in values.iter().zip(&keys).zip(&folded) {
                assert_eq!(
                    left_key.cmp(right_key),
                    msduck_core::bin2::compare(left_folded, right_folded),
                    "{left:04X?} vs {right:04X?}"
                );
            }
        }
    }

    #[test]
    fn case_does_not_distinguish_values_but_accents_do() {
        assert_eq!(order_key(&units("Foo")), order_key(&units("fOO  ")));
        assert_eq!(order_key(&units("ÄÖÜ")), order_key(&units("äöü")));
        assert_eq!(order_key(&units("ΣΑ")), order_key(&units("σα")));
        assert_ne!(order_key(&units("a")), order_key(&units("á")));
        assert!(order_key(&units("a")) < order_key(&units("B")));
        assert_eq!(fold(0x130), 0x69);
        assert_eq!(fold(0xD83E), 0xD83E);
        assert_eq!(base(0xE9), u16::from(b'e'));
        assert_eq!(base(0x100), u16::from(b'A'));
        assert_eq!(base(0xFF), u16::from(b'y'));
        assert_eq!(base(0x17D), u16::from(b'Z'));
        assert_eq!(base(0x142), 0x142);
        assert_eq!(base(0xC6), 0xC6);
    }

    #[test]
    fn ignorable_units_vanish_and_diacritics_sort_by_base_letter() {
        assert_eq!(order_key(&units("a\u{0}b")), order_key(&units("ab")));
        assert_eq!(order_key(&units("\u{1F986}")), order_key(&units("")));
        assert_eq!(order_key(&units("a \u{0}")), order_key(&units("a")));
        assert_ne!(order_key(&units("a\u{1}")), order_key(&units("a")));
        // a < á < ā < ab < b < z, as reference/default-collation.json orders.
        let ordered = ["a", "á", "\u{101}", "ab", "b", "c", "ç", "d", "z"];
        for pair in ordered.windows(2) {
            assert!(
                order_key(&units(pair[0])) < order_key(&units(pair[1])),
                "{pair:?}"
            );
        }
    }

    #[test]
    fn trailing_spaces_do_not_distinguish_values() {
        assert_eq!(order_key(&units("abc")), order_key(&units("abc   ")));
        assert_eq!(order_key(&units("")), order_key(&units("  ")));
        assert_ne!(order_key(&units("a b")), order_key(&units("ab")));
        // Padding: a unit below the space sorts before the shorter value.
        assert_eq!(
            order_key(&units("a")).cmp(&order_key(&units("a\u{1}"))),
            Ordering::Greater
        );
    }
}
