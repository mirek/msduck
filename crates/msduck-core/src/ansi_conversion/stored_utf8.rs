//! SQL UTF16 projection of explicitly UTF8-tagged stored bytes.
//! Storage decoding is separate from wire/BulkLoad admission and the strict
//! scalar-only `super::project` API. No width truncation or padding is applied.
use super::{ProjectionError, ProjectionLimits, Resource, allocate, check_limit};
use crate::ansi_bytes::{AnsiView, EncodingIdentity};
use std::fmt;

/// Resource/declaration errors remain distinct from SQL stored-byte boundary
/// failure. Root adapters may map `InvalidBoundary` to SQL diagnostic 9833.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoredUtf8Error {
    Projection(ProjectionError),
    InvalidBoundary,
}
impl From<ProjectionError> for StoredUtf8Error {
    fn from(error: ProjectionError) -> Self {
        Self::Projection(error)
    }
}
impl fmt::Display for StoredUtf8Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Projection(error) => error.fmt(f),
            Self::InvalidBoundary => f.write_str("invalid stored UTF8 conversion boundary"),
        }
    }
}
impl std::error::Error for StoredUtf8Error {}

/// Decode stored UTF8 bytes into SQL UTF16 units. A nullable payload retains its
/// explicit declaration; unsupported declarations fail even when it is NULL.
/// Check the complete input and exact output limits before allocating output.
pub fn to_sql_utf16(
    source_encoding: EncodingIdentity,
    value: Option<AnsiView<'_>>,
    limits: ProjectionLimits,
) -> Result<Option<Vec<u16>>, StoredUtf8Error> {
    if source_encoding != EncodingIdentity::Utf8 {
        return Err(ProjectionError::UnsupportedSource(source_encoding).into());
    }
    let Some(value) = value else { return Ok(None) };
    if value.encoding() != source_encoding {
        return Err(ProjectionError::SourceEncodingMismatch {
            declared: source_encoding,
            actual: value.encoding(),
        }
        .into());
    }
    let input = value.bytes();
    check_limit(Resource::Input, input.len(), limits.input_bytes)?;
    let input = fitted_prefix(input)?;
    let count = walk(input, |_| {})?;
    let bytes = count
        .checked_mul(2)
        .ok_or(ProjectionError::LengthOverflow)?;
    check_limit(Resource::Output, bytes, limits.output_bytes)?;
    let mut output = allocate(count, bytes)?;
    walk(input, |unit| output.push(unit))?;
    Ok(Some(output))
}

fn walk(input: &[u8], mut emit: impl FnMut(u16)) -> Result<usize, ProjectionError> {
    let mut offset = 0;
    let mut count = 0usize;
    while offset < input.len() {
        let byte = input[offset];
        let mut pair = None;
        let unit = if byte < 0x80 {
            offset += 1;
            u16::from(byte)
        } else {
            let width = match byte {
                0xc2..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf4 => 4,
                _ => 0,
            };
            if width == 0 {
                offset += 1;
                0xfffd
            } else {
                let mut end = offset + 1;
                while end < input.len()
                    && end - offset < width
                    && (0x80..=0xbf).contains(&input[end])
                {
                    end += 1;
                }
                let forbidden = end > offset + 1
                    && match byte {
                        0xe0 => input[offset + 1] < 0xa0,
                        0xed => input[offset + 1] >= 0xa0,
                        0xf0 => input[offset + 1] < 0x90,
                        0xf4 => input[offset + 1] >= 0x90,
                        _ => false,
                    };
                if forbidden {
                    offset += 2;
                    0xfffd
                } else if end - offset < width {
                    offset = end;
                    0xfffd
                } else {
                    let mut scalar = u32::from(byte & ((1 << (7 - width)) - 1));
                    for &continuation in &input[offset + 1..end] {
                        scalar = (scalar << 6) | u32::from(continuation & 0x3f);
                    }
                    offset = end;
                    if scalar <= 0xffff {
                        scalar as u16
                    } else {
                        let supplementary = scalar - 0x10000;
                        pair = Some(0xdc00 + (supplementary & 0x3ff) as u16);
                        0xd800 + (supplementary >> 10) as u16
                    }
                }
            }
        };
        let added = if pair.is_some() { 2 } else { 1 };
        count = count
            .checked_add(added)
            .ok_or(ProjectionError::LengthOverflow)?;
        emit(unit);
        if let Some(low) = pair {
            emit(low);
        }
    }
    Ok(count)
}

// Nominal byte spans deliberately do not validate continuation bytes. SQL's
// EOF fitting and subsequent replacement decoder are separate operations.
fn nominal_width(byte: u8) -> usize {
    match byte {
        0x00..=0x7f => 1,
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => 0,
    }
}
fn previous_span(input: &[u8]) -> Result<usize, StoredUtf8Error> {
    for distance in 1..=input.len().min(4) {
        let width = nominal_width(input[input.len() - distance]);
        if width == distance {
            return Ok(distance);
        }
        if width != 0 {
            return Err(StoredUtf8Error::InvalidBoundary);
        }
    }
    Err(StoredUtf8Error::InvalidBoundary)
}
// An incomplete selector can make its preceding base unavailable at EOF.
// Complete selectors protect a previous pair from being removed a second time.
// These exact ranges and partial prefixes are grounded in SQL17 endpoint probes.
fn selector_prefix(input: &[u8]) -> bool {
    match input {
        [0xe1]
        | [0xef]
        | [0xf3]
        | [0xe1, 0xa0]
        | [0xef, 0xb8]
        | [0xf3, 0xa0]
        | [0xe1, 0xa0, 0x8b..=0x8d, ..]
        | [0xef, 0xb8, 0x80..=0x8f, ..]
        | [0xf3, 0xa0, 0x84..=0x87] => true,
        // SQL's nominal supplementary-selector recognition masks the final
        // payload bits even when that byte is not a valid continuation. The
        // repair pass still emits replacements for those original bytes.
        [0xf3, 0xa0, 0x84..=0x86, _, ..] => true,
        [0xf3, 0xa0, 0x87, last, ..] => last & 0x3f <= 0x2f,
        _ => false,
    }
}

fn discard_previous(input: &[u8]) -> Result<&[u8], StoredUtf8Error> {
    if input.is_empty() {
        return Ok(input);
    }
    let distance = previous_span(input)?;
    let boundary = input.len() - distance;
    if selector_prefix(&input[boundary..]) {
        Ok(input)
    } else {
        Ok(&input[..boundary])
    }
}
fn fitted_prefix(input: &[u8]) -> Result<&[u8], StoredUtf8Error> {
    let Some(&last) = input.last() else {
        return Ok(input);
    };
    if last < 0x80 {
        return Ok(input);
    }
    for distance in 1..=input.len().min(4) {
        let boundary = input.len() - distance;
        let width = nominal_width(input[boundary]);
        if width == distance {
            return Ok(input);
        }
        if width != 0 {
            let prefix = &input[..boundary];
            if selector_prefix(&input[boundary..]) {
                return discard_previous(prefix);
            }
            if !prefix.is_empty() {
                previous_span(prefix)?;
            }
            return Ok(prefix);
        }
    }
    Err(StoredUtf8Error::InvalidBoundary)
}
