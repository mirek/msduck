//! SQL Server ordering for `uniqueidentifier` values.
//!
//! The input is the 16-byte representation produced by SQL Server's
//! `CONVERT(binary(16), guid)` and TDS GUID values: the first 4, 2 and 2-byte
//! fields are little-endian; the remaining 8 bytes keep their displayed order.
//! A canonical UUID string's raw hex bytes are *not* this representation.

use crate::diagnostic::SqlError;
use std::cmp::Ordering;

const CHARACTER_CONVERSION_ERROR: &str =
    "Conversion failed when converting from a character string to uniqueidentifier.";

/// Convert a CP1252-encoded `varchar` value to SQL Server's mixed-endian GUID
/// bytes. ASCII GUID syntax is required; non-ASCII bytes in the accepted prefix
/// fail even if they are valid CP1252 text.
///
/// Callers handle SQL NULL before invoking this function. For `TRY_CONVERT`,
/// turn an error into a typed NULL while preserving the result descriptor.
pub fn parse_varchar(bytes: &[u8]) -> Result<[u8; 16], SqlError> {
    parse_character(bytes.len(), |index| u16::from(bytes[index]))
}

/// Convert raw UTF-16 `nvarchar` units without replacing isolated surrogates.
pub fn parse_nvarchar(units: &[u16]) -> Result<[u8; 16], SqlError> {
    parse_character(units.len(), |index| units[index])
}

fn parse_character(length: usize, unit: impl Fn(usize) -> u16) -> Result<[u8; 16], SqlError> {
    let invalid = || SqlError::new(8169, 2, CHARACTER_CONVERSION_ERROR);
    if length == 0 {
        return Err(invalid());
    }
    let braced = unit(0) == u16::from(b'{');
    let start = usize::from(braced);
    if length < 36 + 2 * start || (braced && unit(37) != u16::from(b'}')) {
        return Err(invalid());
    }

    let mut bytes = [0; 16];
    let mut high = None;
    let mut byte_index = 0;
    for index in 0..36 {
        let value = unit(start + index);
        if matches!(index, 8 | 13 | 18 | 23) {
            if value != u16::from(b'-') {
                return Err(invalid());
            }
            continue;
        }
        let digit = match value {
            48..=57 => (value - 48) as u8,
            65..=70 => (value - 65 + 10) as u8,
            97..=102 => (value - 97 + 10) as u8,
            _ => return Err(invalid()),
        };
        if let Some(first) = high.take() {
            bytes[byte_index] = first << 4 | digit;
            byte_index += 1;
        } else {
            high = Some(digit);
        }
    }
    debug_assert_eq!(byte_index, 16);
    bytes[..4].reverse();
    bytes[4..6].reverse();
    bytes[6..8].reverse();
    Ok(bytes)
}

/// Form the bytewise sort key observed for SQL Server `uniqueidentifier`.
///
/// SQL Server gives the final six bytes highest precedence, followed by the
/// preceding two-byte group, then the third and second groups, and finally
/// the initial four-byte group. Bytes within each group keep their input order.
/// The returned key is suitable for an ordinary unsigned lexicographic sort.
#[must_use]
pub fn order_key(bytes: &[u8; 16]) -> [u8; 16] {
    let mut key = [0; 16];
    key[..6].copy_from_slice(&bytes[10..16]);
    key[6..8].copy_from_slice(&bytes[8..10]);
    key[8..10].copy_from_slice(&bytes[6..8]);
    key[10..12].copy_from_slice(&bytes[4..6]);
    key[12..16].copy_from_slice(&bytes[..4]);
    key
}

/// Compare two mixed-endian GUID byte arrays using SQL Server value order.
#[must_use]
pub fn compare(left: &[u8; 16], right: &[u8; 16]) -> Ordering {
    order_key(left).cmp(&order_key(right))
}
