//! SQL binary-to-Unicode conversion, preserving raw UTF-16 code units.
use crate::character::{CharacterType, Family, Length};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidTarget,
    UnsupportedStyle(i32),
    OutputLimit,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidTarget => f.write_str("binary conversion requires a Unicode target"),
            Self::UnsupportedStyle(style) => {
                write!(f, "unsupported binary-to-Unicode style {style}")
            }
            Self::OutputLimit => {
                f.write_str("binary-to-Unicode output exceeds the configured limit")
            }
        }
    }
}
impl std::error::Error for Error {}

/// Convert a non-NULL input. The caller supplies the output allocation bound in
/// UTF-16 units. NULL and TRY policy belong to the caller, never to this byte rule.
pub fn convert(
    source: &[u8],
    target: CharacterType,
    style: i32,
    max_units: usize,
) -> Result<Vec<u16>, Error> {
    if !matches!(target.family(), Family::Nvarchar | Family::Nchar) {
        return Err(Error::InvalidTarget);
    }
    if !(0..=2).contains(&style) {
        return Err(Error::UnsupportedStyle(style));
    }
    let width = match target.length() {
        Length::Bounded(n) => usize::from(n),
        Length::Max => usize::MAX,
    };
    let count = if style == 0 {
        source.len().div_ceil(2).min(width)
    } else {
        let prefix = usize::from(style == 1) * 2;
        source
            .len()
            .checked_mul(2)
            .and_then(|n| n.checked_add(prefix))
            .ok_or(Error::OutputLimit)?
            .min(width / 2 * 2)
    };
    let size = if target.family() == Family::Nchar {
        width
    } else {
        count
    };
    if size > max_units {
        return Err(Error::OutputLimit);
    }
    let mut units = Vec::with_capacity(size);
    if style == 0 {
        units.extend(
            source
                .chunks(2)
                .take(count)
                .map(|b| u16::from_le_bytes([b[0], *b.get(1).unwrap_or(&0)])),
        );
    } else {
        if style == 1 && count >= 2 {
            units.extend([u16::from(b'0'), u16::from(b'x')]);
        }
        let bytes = (count - units.len()) / 2;
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        for &b in source.iter().take(bytes) {
            units.extend([
                u16::from(HEX[usize::from(b >> 4)]),
                u16::from(HEX[usize::from(b & 15)]),
            ]);
        }
    }
    units.resize(size, u16::from(b' '));
    Ok(units)
}
