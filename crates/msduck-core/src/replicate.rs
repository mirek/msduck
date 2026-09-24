//! REPLICATE value planning after SQL conversion of the source and count.
//! Allocation limits are explicit caller policy, separate from SQL's bounded
//! 8,000-byte result rule. No work scales with the requested count before limits
//! are checked, and bounded results retain only whole copies of the source.
use crate::character::{CharacterType, Family, Length};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Unrepresentable,
    OutputLimit,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unrepresentable => "VARCHAR value is not representable in Windows-1252",
            Self::OutputLimit => "REPLICATE result exceeds the configured output limit",
        })
    }
}
impl std::error::Error for Error {}

/// A borrowed repetition plan. The source is already converted to its SQL
/// character family (including fixed-width padding or binary conversion).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repeat<'a> {
    source: &'a str,
    whole: usize,
    bytes: usize,
}
impl Repeat<'_> {
    pub fn output_bytes(&self) -> usize {
        self.bytes
    }
    pub fn render(&self) -> String {
        let mut output = String::with_capacity(self.bytes);
        for _ in 0..self.whole {
            output.push_str(self.source);
        }
        output
    }
}

/// Count conversion to INT happens before this function. Negative counts and
/// either NULL input return SQL NULL. Non-MAX sources cap at 8,000 SQL bytes;
/// MAX sources retain requested length, subject to the explicit UTF-8 allocation
/// budget. UTF-8 allocation bytes differ from ANSI bytes and UTF-16 units.
pub fn plan<'a>(
    source: Option<&'a str>,
    count: Option<i32>,
    kind: CharacterType,
    output_limit: usize,
) -> Result<Option<Repeat<'a>>, Error> {
    let (Some(source), Some(count)) = (source, count) else {
        return Ok(None);
    };
    if count < 0 {
        return Ok(None);
    }
    let unicode = matches!(kind.family(), Family::Nchar | Family::Nvarchar);
    let units = if unicode {
        source.encode_utf16().count()
    } else {
        crate::encoding::encode_cp1252(source)
            .map_err(|_| Error::Unrepresentable)?
            .len()
    };
    let count = count as usize;
    if units == 0 || count == 0 {
        return Ok(Some(Repeat {
            source,
            whole: 0,
            bytes: 0,
        }));
    }
    let whole = if kind.length() == Length::Max {
        count
    } else {
        let capacity = if unicode { 4000 } else { 8000 };
        count.min(capacity / units)
    };
    let bytes = source
        .len()
        .checked_mul(whole)
        .filter(|bytes| *bytes <= output_limit)
        .ok_or(Error::OutputLimit)?;
    Ok(Some(Repeat {
        source,
        whole,
        bytes,
    }))
}

/// Result declaration from the converted source declaration and a compile-time
/// INT count, when proven constant. Do not supply a variable's current value.
/// MAX remains MAX even for zero repetitions. Nonpositive constants have the
/// observed minimum descriptor capacity of two bytes (one NVARCHAR unit).
pub fn result_type(input: CharacterType, constant_count: Option<i32>) -> CharacterType {
    let unicode = matches!(input.family(), Family::Nchar | Family::Nvarchar);
    let family = if unicode {
        Family::Nvarchar
    } else {
        Family::Varchar
    };
    let capacity = if unicode { 4000 } else { 8000 };
    let length = match input.length() {
        Length::Max => Length::Max,
        Length::Bounded(width) => Length::Bounded(match constant_count {
            None => capacity,
            Some(count) if count <= 0 => {
                if unicode {
                    1
                } else {
                    2
                }
            }
            Some(count) => u32::from(width)
                .saturating_mul(count as u32)
                .min(u32::from(capacity)) as u16,
        }),
    };
    CharacterType::new(family, length).expect("REPLICATE result declaration is bounded or MAX")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn kind(family: Family, max: bool) -> CharacterType {
        CharacterType::new(family, if max { Length::Max } else { Length::Bounded(3) }).unwrap()
    }
    fn repeat(
        source: Option<&str>,
        count: Option<i32>,
        family: Family,
        max: bool,
    ) -> Option<String> {
        plan(source, count, kind(family, max), 32768)
            .unwrap()
            .map(|plan| plan.render())
    }
    #[test]
    fn live_values_null_counts_fixed_padding_and_binary_conversion() {
        assert_eq!(
            repeat(Some("ab"), Some(3), Family::Varchar, false).as_deref(),
            Some("ababab")
        );
        assert_eq!(
            repeat(Some("雪"), Some(3), Family::Nvarchar, false).as_deref(),
            Some("雪雪雪")
        );
        for family in [Family::Char, Family::Nchar] {
            assert_eq!(
                repeat(Some("x  "), Some(2), family, false).as_deref(),
                Some("x  x  ")
            );
        }
        assert_eq!(
            repeat(Some("x"), Some(0), Family::Varchar, false).as_deref(),
            Some("")
        );
        assert_eq!(repeat(Some("x"), Some(-1), Family::Varchar, false), None);
        assert_eq!(repeat(Some(""), Some(-1), Family::Varchar, false), None);
        assert_eq!(repeat(None, Some(3), Family::Varchar, false), None);
        assert_eq!(repeat(Some("x"), None, Family::Varchar, false), None);
        let binary = crate::encoding::decode_cp1252(&[0, 0x80, 0x41]);
        assert_eq!(
            repeat(Some(&binary), Some(2), Family::Varchar, false).as_deref(),
            Some("\0€A\0€A")
        );
    }
    #[test]
    fn live_caps_preserve_whole_copies_unicode_pairs_and_max_values() {
        assert_eq!(
            repeat(Some("x"), Some(8100), Family::Varchar, false)
                .unwrap()
                .len(),
            8000
        );
        assert_eq!(
            repeat(Some("x"), Some(4100), Family::Nvarchar, false)
                .unwrap()
                .encode_utf16()
                .count(),
            4000
        );
        assert_eq!(
            repeat(Some("abc"), Some(2667), Family::Varchar, false).unwrap(),
            "abc".repeat(2666)
        );
        assert_eq!(
            repeat(Some("abc"), Some(1334), Family::Nvarchar, false).unwrap(),
            "abc".repeat(1333)
        );
        let partial = repeat(Some("🦆x"), Some(1334), Family::Nvarchar, false).unwrap();
        assert_eq!(partial, "🦆x".repeat(1333));
        assert_eq!(partial.encode_utf16().count(), 3999);
        assert_eq!(
            repeat(Some("🦆"), Some(2001), Family::Nvarchar, false).unwrap(),
            "🦆".repeat(2000)
        );
        assert_eq!(
            repeat(Some("x"), Some(8100), Family::Varchar, true)
                .unwrap()
                .len(),
            8100
        );
        assert_eq!(
            repeat(Some("x"), Some(4100), Family::Nvarchar, true)
                .unwrap()
                .encode_utf16()
                .count(),
            4100
        );
    }
    #[test]
    fn live_result_declarations_distinguish_constants_variables_fixed_and_max() {
        for (family, width, count, expected_family, expected_width) in [
            (Family::Varchar, 2, Some(3), Family::Varchar, 6),
            (Family::Nvarchar, 1, Some(3), Family::Nvarchar, 3),
            (Family::Char, 3, Some(2), Family::Varchar, 6),
            (Family::Nchar, 3, Some(2), Family::Nvarchar, 6),
            (Family::Varchar, 4, Some(0), Family::Varchar, 2),
            (Family::Nvarchar, 2, Some(-1), Family::Nvarchar, 1),
            (Family::Varchar, 10, Some(3), Family::Varchar, 30),
            (Family::Varchar, 2, None, Family::Varchar, 8000),
            (Family::Nvarchar, 2, None, Family::Nvarchar, 4000),
            (Family::Varchar, 2, Some(i32::MAX), Family::Varchar, 8000),
        ] {
            let input = CharacterType::new(family, Length::Bounded(width)).unwrap();
            assert_eq!(
                result_type(input, count),
                CharacterType::new(expected_family, Length::Bounded(expected_width)).unwrap()
            );
        }
        for family in [Family::Varchar, Family::Nvarchar] {
            assert_eq!(result_type(kind(family, true), Some(0)), kind(family, true));
        }
    }
    #[test]
    fn limits_are_checked_before_allocating_or_iterating_over_repetitions() {
        assert_eq!(
            plan(
                Some("x"),
                Some(i32::MAX),
                kind(Family::Nvarchar, true),
                1024
            ),
            Err(Error::OutputLimit)
        );
        let bounded = plan(
            Some("€"),
            Some(i32::MAX),
            kind(Family::Varchar, false),
            24000,
        )
        .unwrap()
        .unwrap();
        assert_eq!(bounded.output_bytes(), 24000);
        assert_eq!(bounded.render().chars().count(), 8000);
        assert_eq!(
            plan(Some("€"), Some(8000), kind(Family::Varchar, false), 23999),
            Err(Error::OutputLimit)
        );
        assert_eq!(
            plan(Some("雪"), Some(1), kind(Family::Varchar, false), 100),
            Err(Error::Unrepresentable)
        );
        let empty = plan(Some(""), Some(i32::MAX), kind(Family::Nvarchar, true), 0)
            .unwrap()
            .unwrap();
        assert_eq!(empty.output_bytes(), 0);
        assert_eq!(empty.render(), "");
    }
}
