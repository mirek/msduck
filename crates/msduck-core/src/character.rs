//! Validated SQL character types and deterministic storage / CAST rules.
use crate::encoding::{decode_cp1252, encode_cp1252};
use std::{borrow::Cow, fmt};

pub const TRUNCATED: &str = "String or binary data would be truncated.";
pub const INVALID_LENGTH: &str = "invalid character column length";
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    Varchar,
    Char,
    Nvarchar,
    Nchar,
}
impl Family {
    fn unicode(self) -> bool {
        matches!(self, Self::Nvarchar | Self::Nchar)
    }
    fn fixed(self) -> bool {
        matches!(self, Self::Char | Self::Nchar)
    }
    fn name(self) -> &'static str {
        match self {
            Self::Varchar => "varchar",
            Self::Char => "char",
            Self::Nvarchar => "nvarchar",
            Self::Nchar => "nchar",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Length {
    Bounded(u16),
    Max,
}
/// Source category after its value has been formatted as text by an adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CastInput {
    Text,
    SmallInteger,
    OtherNumeric,
    Currency,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidLength,
    Truncated,
    NumericOverflow(Family),
    Unrepresentable,
    SplitSurrogate,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLength => f.write_str(INVALID_LENGTH),
            Self::Truncated => f.write_str(TRUNCATED),
            Self::NumericOverflow(family) => write!(
                f,
                "Arithmetic overflow error converting expression to data type {}.",
                family.name()
            ),
            Self::Unrepresentable => {
                f.write_str("VARCHAR value is not representable in Windows-1252")
            }
            Self::SplitSurrogate => {
                f.write_str("NVARCHAR truncation inside a surrogate pair is not yet supported")
            }
        }
    }
}
impl std::error::Error for Error {}
/// LEN under a non-SC Unicode collation: count UTF-16 units, excluding trailing
/// U+0020 spaces only. Isolated surrogates and other whitespace remain units.
pub fn len_utf16(units: &[u16]) -> usize {
    units
        .iter()
        .rposition(|&unit| unit != 32)
        .map_or(0, |last| last + 1)
}

/// Text conversion that can retain isolated UTF-16 code units.
#[derive(Debug, PartialEq, Eq)]
pub enum ConvertedUnicode {
    Unicode(Vec<u16>),
    Ansi(String),
}

/// Constructed only after validating SQL's family-specific length limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CharacterType {
    family: Family,
    length: Length,
}
impl CharacterType {
    pub const fn family(self) -> Family {
        self.family
    }
    pub const fn length(self) -> Length {
        self.length
    }
    pub fn new(family: Family, length: Length) -> Result<Self, Error> {
        match length {
            Length::Max if !family.fixed() => {}
            Length::Bounded(n) if (1..=if family.unicode() { 4000 } else { 8000 }).contains(&n) => {
            }
            _ => return Err(Error::InvalidLength),
        }
        Ok(Self { family, length })
    }
    /// Explicit character conversion under the current non-SC collation.
    /// Surrogates convert individually to '?' for ANSI targets. Other best-fit
    /// code-page mappings remain unsupported rather than silently substituted.
    pub fn cast_utf16(self, source: &[u16]) -> Result<ConvertedUnicode, Error> {
        let width = match self.length {
            Length::Max => source.len(),
            Length::Bounded(n) => usize::from(n),
        };
        let source = &source[..source.len().min(width)];
        if self.family.unicode() {
            let mut units = source.to_vec();
            if self.family.fixed() {
                units.resize(width, 32);
            }
            return Ok(ConvertedUnicode::Unicode(units));
        }
        let mut bytes = Vec::with_capacity(if self.family.fixed() {
            width
        } else {
            source.len()
        });
        for &unit in source {
            if (0xd800..=0xdfff).contains(&unit) {
                bytes.push(b'?');
            } else {
                let ch = char::from_u32(u32::from(unit)).expect("BMP non-surrogate");
                let mut utf8 = [0; 4];
                bytes.extend(
                    encode_cp1252(ch.encode_utf8(&mut utf8)).map_err(|_| Error::Unrepresentable)?,
                );
            }
        }
        if self.family.fixed() {
            bytes.resize(width, b' ');
        }
        Ok(ConvertedUnicode::Ansi(decode_cp1252(&bytes)))
    }

    /// Storage counts UTF-16 units without requiring well-formed scalar values.
    /// Unlike CAST, it rejects non-space overflow instead of truncating it.
    pub fn store_utf16(self, source: &[u16]) -> Result<ConvertedUnicode, Error> {
        if let Length::Bounded(width) = self.length
            && len_utf16(source) > usize::from(width)
        {
            return Err(Error::Truncated);
        }
        self.cast_utf16(source)
    }

    /// Storage under ANSI_WARNINGS ON: excess trailing spaces are discardable,
    /// excess non-space text is an error, and fixed-width targets are padded.
    pub fn store(self, text: &str) -> Result<String, Error> {
        if self.family.unicode() {
            let Length::Bounded(width) = self.length else {
                return Ok(text.into());
            };
            let width = usize::from(width);
            if text.trim_end_matches(' ').encode_utf16().count() > width {
                return Err(Error::Truncated);
            }
            self.unicode_prefix(text, width).map(Cow::into_owned)
        } else {
            let mut bytes = encode_cp1252(text).map_err(|_| Error::Unrepresentable)?;
            if let Length::Bounded(width) = self.length {
                let width = usize::from(width);
                if bytes
                    .iter()
                    .rposition(|b| *b != b' ')
                    .is_some_and(|last| last >= width)
                {
                    return Err(Error::Truncated);
                }
                bytes.truncate(width);
                if self.family.fixed() {
                    bytes.resize(width, b' ');
                }
            }
            Ok(decode_cp1252(&bytes))
        }
    }
    /// CAST's text truncation differs from storage. Numeric display overflow is
    /// an asterisk for single-byte small integers and an error otherwise.
    /// Currency alone is right-aligned in fixed-width character targets.
    pub fn cast<'a>(self, text: &'a str, source: CastInput) -> Result<Cow<'a, str>, Error> {
        if self.family.unicode() {
            let Length::Bounded(width) = self.length else {
                return Ok(Cow::Borrowed(text));
            };
            let width = usize::from(width);
            if source != CastInput::Text && text.encode_utf16().count() > width {
                return Err(Error::NumericOverflow(self.family));
            }
            if source == CastInput::Currency && self.family.fixed() {
                let mut result = " ".repeat(width - text.encode_utf16().count());
                result.push_str(text);
                return Ok(Cow::Owned(result));
            }
            self.unicode_prefix(text, width)
        } else {
            let mut bytes = encode_cp1252(text).map_err(|_| Error::Unrepresentable)?;
            if let Length::Bounded(width) = self.length {
                let width = usize::from(width);
                if bytes.len() > width {
                    match source {
                        CastInput::SmallInteger => bytes = vec![b'*'],
                        CastInput::OtherNumeric | CastInput::Currency => {
                            return Err(Error::NumericOverflow(self.family));
                        }
                        CastInput::Text => bytes.truncate(width),
                    }
                }
                if self.family.fixed() {
                    let padding = width - bytes.len();
                    bytes.resize(width, b' ');
                    if source == CastInput::Currency {
                        bytes.rotate_right(padding);
                    }
                }
            }
            Ok(Cow::Owned(decode_cp1252(&bytes)))
        }
    }
    fn unicode_prefix<'a>(self, text: &'a str, width: usize) -> Result<Cow<'a, str>, Error> {
        let mut units = 0;
        let mut end = text.len();
        for (offset, ch) in text.char_indices() {
            if units == width {
                end = offset;
                break;
            }
            if units + ch.len_utf16() > width {
                return Err(Error::SplitSurrogate);
            }
            units += ch.len_utf16();
        }
        if self.family.fixed() {
            let mut result = text[..end].to_owned();
            result.extend(std::iter::repeat_n(' ', width - units));
            Ok(Cow::Owned(result))
        } else {
            Ok(Cow::Borrowed(&text[..end]))
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn storage_retains_raw_units_and_only_discards_excess_spaces() {
        use ConvertedUnicode::{Ansi, Unicode};
        let variable = CharacterType::new(Family::Nvarchar, Length::Bounded(2)).unwrap();
        let fixed = CharacterType::new(Family::Nchar, Length::Bounded(2)).unwrap();
        assert_eq!(variable.store_utf16(&[0xd83e]), Ok(Unicode(vec![0xd83e])));
        assert_eq!(fixed.store_utf16(&[0xdd86]), Ok(Unicode(vec![0xdd86, 32])));
        assert_eq!(
            variable.store_utf16(&[0xd83e, 32, 32]),
            Ok(Unicode(vec![0xd83e, 32]))
        );
        assert_eq!(
            variable.store_utf16(&[0xd83e, 0xdd86, 32]),
            Ok(Unicode(vec![0xd83e, 0xdd86]))
        );
        assert_eq!(
            variable.store_utf16(&[0xd83e, 0xdd86, 9]),
            Err(Error::Truncated)
        );
        let one = CharacterType::new(Family::Nvarchar, Length::Bounded(1)).unwrap();
        assert_eq!(one.store_utf16(&[0xd83e, 0xdd86]), Err(Error::Truncated));
        assert_eq!(one.cast_utf16(&[0xd83e, 0xdd86]), Ok(Unicode(vec![0xd83e])));
        let max = CharacterType::new(Family::Nvarchar, Length::Max).unwrap();
        assert_eq!(
            max.store_utf16(&[0, 0xdc00, 0xd800]),
            Ok(Unicode(vec![0, 0xdc00, 0xd800]))
        );
        let ansi = CharacterType::new(Family::Char, Length::Bounded(2)).unwrap();
        assert_eq!(ansi.store_utf16(&[0xd83e]), Ok(Ansi("? ".into())));
        assert_eq!(
            ansi.store_utf16(&[0xd83e, 0xdd86, 65]),
            Err(Error::Truncated)
        );
    }
    #[test]
    fn widths_spaces_and_unicode_units() {
        for family in [
            Family::Varchar,
            Family::Char,
            Family::Nvarchar,
            Family::Nchar,
        ] {
            let s = CharacterType::new(family, Length::Bounded(3)).unwrap();
            assert_eq!(
                s.store("a").unwrap(),
                if family.fixed() { "a  " } else { "a" }
            );
            assert_eq!(s.store("abc  ").unwrap(), "abc");
            assert_eq!(s.store("abcd"), Err(Error::Truncated));
            assert_eq!(s.store("abc\t"), Err(Error::Truncated));
        }
        let s = CharacterType::new(Family::Nchar, Length::Bounded(3)).unwrap();
        assert_eq!(s.store("🦆").unwrap(), "🦆 ");
        assert_eq!(s.store("🦆🦆"), Err(Error::Truncated));
    }
    #[test]
    fn declaration_limits_max_and_empty_padding() {
        for family in [
            Family::Varchar,
            Family::Char,
            Family::Nvarchar,
            Family::Nchar,
        ] {
            assert_eq!(
                CharacterType::new(family, Length::Bounded(0)),
                Err(Error::InvalidLength)
            );
            let limit = if family.unicode() { 4000 } else { 8000 };
            assert!(CharacterType::new(family, Length::Bounded(limit)).is_ok());
            assert_eq!(
                CharacterType::new(family, Length::Bounded(limit + 1)),
                Err(Error::InvalidLength)
            );
            let max = CharacterType::new(family, Length::Max);
            assert_eq!(max.is_err(), family.fixed());
            if let Ok(max) = max {
                let text = "é ".repeat(9000);
                assert_eq!(max.store(&text).unwrap(), text);
            }
            let s = CharacterType::new(family, Length::Bounded(2)).unwrap();
            assert_eq!(s.store("").unwrap(), if family.fixed() { "  " } else { "" });
        }
    }
    #[test]
    fn currency_cast_alignment_does_not_change_other_numbers_or_storage() {
        for family in [
            Family::Char,
            Family::Nchar,
            Family::Varchar,
            Family::Nvarchar,
        ] {
            let target = CharacterType::new(family, Length::Bounded(8)).unwrap();
            assert_eq!(
                target.cast("-1.25", CastInput::Currency).unwrap(),
                if family.fixed() { "   -1.25" } else { "-1.25" }
            );
            for source in [
                CastInput::Text,
                CastInput::SmallInteger,
                CastInput::OtherNumeric,
            ] {
                assert_eq!(
                    target.cast("-1.25", source).unwrap(),
                    if family.fixed() { "-1.25   " } else { "-1.25" }
                );
            }
            assert_eq!(
                target.store("-1.25").unwrap(),
                if family.fixed() { "-1.25   " } else { "-1.25" }
            );
            assert_eq!(
                target.cast("12345678", CastInput::Currency).unwrap(),
                "12345678"
            );
            assert_eq!(
                target.cast("123456789", CastInput::Currency),
                Err(Error::NumericOverflow(family))
            );
        }
    }
    #[test]
    fn casts_distinguish_text_numeric_and_storage_overflow() {
        let v = CharacterType::new(Family::Varchar, Length::Bounded(2)).unwrap();
        assert_eq!(v.cast("abc", CastInput::Text).unwrap(), "ab");
        assert_eq!(v.store("abc"), Err(Error::Truncated));
        assert_eq!(v.cast("123", CastInput::SmallInteger).unwrap(), "*");
        assert_eq!(
            v.cast("123", CastInput::OtherNumeric),
            Err(Error::NumericOverflow(Family::Varchar))
        );
        let c = CharacterType::new(Family::Char, Length::Bounded(2)).unwrap();
        assert_eq!(c.cast("123", CastInput::SmallInteger).unwrap(), "* ");
        assert_eq!(v.cast("🦆", CastInput::Text), Err(Error::Unrepresentable));
        let n = CharacterType::new(Family::Nvarchar, Length::Bounded(2)).unwrap();
        assert_eq!(n.cast("🦆x", CastInput::Text).unwrap(), "🦆");
        assert_eq!(
            n.cast("123", CastInput::SmallInteger),
            Err(Error::NumericOverflow(Family::Nvarchar))
        );
        let short = CharacterType::new(Family::Nvarchar, Length::Bounded(1)).unwrap();
        assert_eq!(
            short.cast("🦆", CastInput::Text),
            Err(Error::SplitSurrogate)
        );
        assert_eq!(short.store("🦆"), Err(Error::Truncated));
    }
}

#[cfg(test)]
mod unicode_cast_tests {
    use super::*;
    #[test]
    fn unicode_length_counts_units_and_trims_only_spaces() {
        for (units, expected) in [
            (&[0xd83e][..], 1),
            (&[0xd83e, 0xdd86], 2),
            (&[0xd83e, 32, 32], 1),
            (&[97, 32, 9], 3),
            (&[0, 32], 1),
            (&[32, 32], 0),
            (&[], 0),
        ] {
            assert_eq!(len_utf16(units), expected);
        }
    }
    #[test]
    fn reference_surrogate_casts_preserve_units_or_convert_each_to_question_mark() {
        let target = |family, width| CharacterType::new(family, Length::Bounded(width)).unwrap();
        assert_eq!(
            target(Family::Nvarchar, 3).cast_utf16(&[0xd83e]).unwrap(),
            ConvertedUnicode::Unicode(vec![0xd83e])
        );
        assert_eq!(
            target(Family::Nchar, 3).cast_utf16(&[0xd83e]).unwrap(),
            ConvertedUnicode::Unicode(vec![0xd83e, 32, 32])
        );
        assert_eq!(
            target(Family::Nvarchar, 1)
                .cast_utf16(&[0xd83e, 0xdd86])
                .unwrap(),
            ConvertedUnicode::Unicode(vec![0xd83e])
        );
        assert_eq!(
            target(Family::Varchar, 3)
                .cast_utf16(&[0xd83e, 0xdd86])
                .unwrap(),
            ConvertedUnicode::Ansi("??".into())
        );
        assert_eq!(
            target(Family::Char, 3).cast_utf16(&[97, 98]).unwrap(),
            ConvertedUnicode::Ansi("ab ".into())
        );
    }
}
