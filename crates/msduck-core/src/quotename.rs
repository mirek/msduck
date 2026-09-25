//! Deterministic SQL Server `QUOTENAME` over UTF-16 code units.
//!
//! A caller must supply the already-converted Unicode input and distinguish an
//! omitted delimiter from a SQL NULL delimiter. SQL binding and wire metadata
//! remain outside this module.

/// SQL's optional quote character after Unicode conversion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delimiter<'a> {
    Default,
    Explicit(&'a [u16]),
    Null,
}

/// Return quoted UTF-16, or `None` for SQL NULL and invalid inputs.
///
/// The input limit is 128 UTF-16 units. The largest result is 258 units: two
/// wrappers plus a doubled closing delimiter for each input unit.
pub fn quote_units(input: Option<&[u16]>, delimiter: Delimiter<'_>) -> Option<Vec<u16>> {
    let input = input?;
    if input.len() > 128 {
        return None;
    }
    let unit = match delimiter {
        Delimiter::Default | Delimiter::Explicit([]) => b']' as u16,
        Delimiter::Explicit(units) => *units.first()?,
        Delimiter::Null => return None,
    };
    // SQL Server's explicit NUL delimiter is a special pass-through. It still
    // applies the sysname length limit, but returns NULL for empty input.
    if unit == 0 {
        return (!input.is_empty()).then(|| input.to_vec());
    }
    // Other delimiters reject an embedded NUL, even though isolated surrogate
    // units remain valid input and are copied without Unicode normalization.
    if input.contains(&0) {
        return None;
    }
    let (open, close) = match unit {
        0x5b | 0x5d => (b'[' as u16, b']' as u16),
        0x28 | 0x29 => (b'(' as u16, b')' as u16),
        0x3c | 0x3e => (b'<' as u16, b'>' as u16),
        0x7b | 0x7d => (b'{' as u16, b'}' as u16),
        0x27 => (b'\'' as u16, b'\'' as u16),
        0x22 => (b'"' as u16, b'"' as u16),
        0x60 => (b'`' as u16, b'`' as u16),
        _ => return None,
    };
    let capacity = input.len().checked_mul(2)?.checked_add(2)?;
    if capacity > 258 {
        return None;
    }
    let mut output = Vec::with_capacity(capacity);
    output.push(open);
    for &character in input {
        output.push(character);
        if character == close {
            output.push(close);
        }
    }
    output.push(close);
    Some(output)
}
