//! LEFT/RIGHT after explicit character and INT conversion. ANSI inputs are
//! encoded bytes; Unicode inputs are UTF-16 units under a non-SC collation.
//! These borrowed slices preserve isolated surrogates without lossy decoding.
use crate::{
    character::{CharacterType, Family, Length},
    diagnostic::SqlError,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

impl Side {
    fn slice<T>(self, source: &[T], count: i32) -> Result<&[T], SqlError> {
        let count = usize::try_from(count)
            .map_err(|_| {
                let name = match self {
                    Self::Left => "left",
                    Self::Right => "right",
                };
                SqlError::new(
                    536,
                    6,
                    format!("Invalid length parameter passed to the {name} function."),
                )
            })?
            .min(source.len());
        Ok(match self {
            Self::Left => &source[..count],
            Self::Right => &source[source.len() - count..],
        })
    }

    /// Already converted single-byte character data (including fixed padding).
    /// Binary SQL inputs must undergo their character conversion first.
    pub fn ansi(self, source: &[u8], count: i32) -> Result<&[u8], SqlError> {
        self.slice(source, count)
    }

    /// Non-SC collation semantics: a surrogate pair counts as two units.
    /// Keep the returned units intact through storage and wire encoding.
    pub fn utf16(self, source: &[u16], count: i32) -> Result<&[u16], SqlError> {
        self.slice(source, count)
    }
}

/// Recover typed identity after a native scalar boundary transmits error text.
/// Backend wrapper removal belongs to the adapter; unrelated text is not guessed.
pub fn diagnostic(message: &str) -> Option<SqlError> {
    matches!(
        message,
        "Invalid length parameter passed to the left function."
            | "Invalid length parameter passed to the right function."
    )
    .then(|| SqlError::new(536, 6, message))
}

/// Result capacity from a source declaration and a proven nonnegative constant
/// INT count. Unknown counts retain source width; current parameter values must
/// not be supplied as constants. Invalid negative constants need diagnostics,
/// whose compilation/execution phase is the caller's responsibility.
pub fn result_type(input: CharacterType, constant_count: Option<u32>) -> CharacterType {
    let family = match input.family() {
        Family::Char | Family::Varchar => Family::Varchar,
        Family::Nchar | Family::Nvarchar => Family::Nvarchar,
    };
    let length = match (input.length(), constant_count) {
        (Length::Bounded(width), Some(count)) => {
            Length::Bounded(u32::from(width).min(count).max(1) as u16)
        }
        (length, _) => length,
    };
    CharacterType::new(family, length).expect("LEFT/RIGHT preserves or narrows a valid declaration")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_ansi_values_padding_binary_conversion_and_extreme_counts() {
        assert_eq!(Side::Left.ansi(b"abcdef", 3).unwrap(), b"abc");
        assert_eq!(Side::Right.ansi(b"abcdef", 3).unwrap(), b"def");
        assert_eq!(Side::Left.ansi(b"x         ", 3).unwrap(), b"x  ");
        assert_eq!(Side::Right.ansi(b"x         ", 3).unwrap(), b"   ");
        let bytes = [0, 0x80, 0x41];
        assert_eq!(
            crate::encoding::decode_cp1252(Side::Left.ansi(&bytes, 2).unwrap()),
            "\0€"
        );
        for side in [Side::Left, Side::Right] {
            assert_eq!(side.ansi(b"abc", 0).unwrap(), b"");
            assert_eq!(side.ansi(b"abc", i32::MAX).unwrap(), b"abc");
            assert_eq!(side.ansi(b"", i32::MAX).unwrap(), b"");
        }
    }

    #[test]
    fn live_non_sc_slicing_preserves_surrogates_and_composes_without_decoding() {
        let left: Vec<_> = "🦆xy".encode_utf16().collect();
        let right: Vec<_> = "x🦆".encode_utf16().collect();
        assert_eq!(Side::Left.utf16(&left, 1).unwrap(), &[0xd83e]);
        assert_eq!(Side::Right.utf16(&right, 1).unwrap(), &[0xdd86]);
        let pair = Side::Left.utf16(&left, 2).unwrap();
        assert_eq!(pair, &[0xd83e, 0xdd86]);
        assert_eq!(Side::Right.utf16(pair, 1).unwrap(), &[0xdd86]);
        assert_eq!(Side::Left.utf16(&[0xd83e], 1).unwrap(), &[0xd83e]);
        assert!(Side::Right.utf16(&right, 0).unwrap().is_empty());
        assert_eq!(Side::Left.utf16(&left, i32::MAX).unwrap(), left);
    }

    #[test]
    fn negative_lengths_have_function_specific_identity() {
        for (side, name) in [(Side::Left, "left"), (Side::Right, "right")] {
            for count in [-1, i32::MIN] {
                let expected = SqlError::new(
                    536,
                    6,
                    format!("Invalid length parameter passed to the {name} function."),
                );
                assert_eq!(diagnostic(&expected.message), Some(expected.clone()));
                assert_eq!(side.ansi(b"abc", count), Err(expected.clone()));
                assert_eq!(side.utf16(&[65], count), Err(expected));
            }
        }
    }

    #[test]
    fn live_declarations_preserve_max_and_dynamic_widths() {
        for (family, variable) in [
            (Family::Char, Family::Varchar),
            (Family::Varchar, Family::Varchar),
            (Family::Nchar, Family::Nvarchar),
            (Family::Nvarchar, Family::Nvarchar),
        ] {
            let input = CharacterType::new(family, Length::Bounded(10)).unwrap();
            for (count, width) in [(Some(3), 3), (Some(0), 1), (Some(20), 10), (None, 10)] {
                assert_eq!(
                    result_type(input, count),
                    CharacterType::new(variable, Length::Bounded(width)).unwrap()
                );
            }
            let max = CharacterType::new(variable, Length::Max).unwrap();
            assert_eq!(result_type(max, Some(2)), max);
        }
    }
}
