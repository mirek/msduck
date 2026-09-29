//! SQL Server ordering for `uniqueidentifier` values.
//!
//! The input is the 16-byte representation produced by SQL Server's
//! `CONVERT(binary(16), guid)` and TDS GUID values: the first 4, 2 and 2-byte
//! fields are little-endian; the remaining 8 bytes keep their displayed order.
//! A canonical UUID string's raw hex bytes are *not* this representation.

use std::cmp::Ordering;

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
