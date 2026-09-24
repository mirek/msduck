//! Character concatenation declarations and bounded prefix construction.
//! NULL propagation and physical text decoding belong to the caller.
use crate::character::{Family, Length};

/// Result capacities can be zero, unlike storage declarations. Fixed families
/// survive only when both operands are fixed; any Unicode operand wins encoding.
pub fn shape(left: (Family, Length), right: (Family, Length)) -> (Family, Length) {
    let unicode = [left.0, right.0]
        .iter()
        .any(|f| matches!(f, Family::Nchar | Family::Nvarchar));
    let max = matches!(left.1, Length::Max) || matches!(right.1, Length::Max);
    let fixed = !max
        && [left.0, right.0]
            .iter()
            .all(|f| matches!(f, Family::Char | Family::Nchar));
    let family = match (unicode, fixed) {
        (true, true) => Family::Nchar,
        (true, false) => Family::Nvarchar,
        (false, true) => Family::Char,
        (false, false) => Family::Varchar,
    };
    let length = match (left.1, right.1) {
        (Length::Bounded(a), Length::Bounded(b)) => Length::Bounded(
            (u32::from(a) + u32::from(b)).min(if unicode { 4000 } else { 8000 }) as u16,
        ),
        _ => Length::Max,
    };
    (family, length)
}

fn prefix<T: Copy>(left: &[T], right: &[T], length: Length, limit: usize) -> Option<Vec<T>> {
    let total = left.len().checked_add(right.len())?;
    let total = match length {
        Length::Bounded(n) => total.min(usize::from(n)),
        Length::Max => total,
    };
    if total > limit {
        return None;
    }
    let mut result = Vec::with_capacity(total);
    let first = left.len().min(total);
    result.extend_from_slice(&left[..first]);
    result.extend_from_slice(&right[..total - first]);
    Some(result)
}

/// Non-SC concatenation truncates by UTF-16 units, retaining isolated surrogates.
pub fn utf16(left: &[u16], right: &[u16], length: Length, limit: usize) -> Option<Vec<u16>> {
    prefix(left, right, length, limit)
}
/// ANSI concatenation truncates encoded bytes, not UTF-8 bytes.
pub fn ansi(left: &[u8], right: &[u8], length: Length, limit: usize) -> Option<Vec<u8>> {
    prefix(left, right, length, limit)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prefixes_preserve_surrogates_bytes_and_intermediate_truncation() {
        assert_eq!(
            utf16(&[0xd83e], &[0xdd86], Length::Bounded(2), 2),
            Some(vec![0xd83e, 0xdd86])
        );
        assert_eq!(
            utf16(&[0xd83e, 0xdd86], &[120], Length::Bounded(1), 1),
            Some(vec![0xd83e])
        );
        assert_eq!(
            ansi(&[0x80, 0xa0], &[0], Length::Bounded(2), 2),
            Some(vec![0x80, 0xa0])
        );
        assert_eq!(utf16(&[1], &[2], Length::Max, 1), None);
        assert_eq!(utf16(&[1], &[2], Length::Bounded(0), 0), Some(vec![]));
        let first = ansi(&vec![b'a'; 8000], b"b", Length::Bounded(8000), 8000).unwrap();
        let result = ansi(&first, b"c", Length::Max, 8001).unwrap();
        assert_eq!(result.len(), 8001);
        assert_eq!(result[8000], b'c');
        assert!(!result.contains(&b'b'));
    }
}
