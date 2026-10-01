//! CONVERT binary styles 0, 1 and 2 between binary and character values.

/// Why a conversion failed.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Error {
    /// 9809: the style does not apply to binary conversions.
    Style,
    /// 8114: the text is not hexadecimal in the style's shape.
    Syntax,
}

/// Binary to character text. Style 0 reinterprets the bytes as code page
/// 1252 characters (or UTF-16LE for Unicode targets); 1 and 2 give
/// uppercase hexadecimal with and without the `0x` prefix.
pub(super) fn to_text(bytes: &[u8], style: i32, unicode: bool) -> Result<String, Error> {
    let hex = || bytes.iter().map(|b| format!("{b:02X}")).collect::<String>();
    match style {
        0 if unicode => {
            let units: Vec<u16> = bytes
                .chunks(2)
                .map(|pair| u16::from_le_bytes([pair[0], *pair.get(1).unwrap_or(&0)]))
                .collect();
            Ok(String::from_utf16_lossy(&units))
        }
        0 => Ok(msduck_core::encoding::decode_cp1252(bytes)),
        1 => Ok(format!("0x{}", hex())),
        2 => Ok(hex()),
        _ => Err(Error::Style),
    }
}

/// Character text to binary. Style 1 requires the `0x` prefix and style 2
/// forbids it; both need an even number of hexadecimal digits.
pub(super) fn from_text(text: &str, style: i32, utf16: Option<&[u16]>) -> Result<Vec<u8>, Error> {
    let digits = match style {
        0 => {
            return Ok(match utf16 {
                Some(units) => units.iter().flat_map(|u| u.to_le_bytes()).collect(),
                None => msduck_core::encoding::encode_cp1252(text).unwrap_or_else(|_| {
                    text.chars()
                        .map(|c| if (c as u32) < 256 { c as u8 } else { b'?' })
                        .collect()
                }),
            });
        }
        1 => {
            let lower = text.get(..2).map(str::to_ascii_lowercase);
            if lower.as_deref() != Some("0x") {
                return Err(Error::Syntax);
            }
            &text[2..]
        }
        2 => text,
        _ => return Err(Error::Style),
    };
    if digits.len() % 2 != 0 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::Syntax);
    }
    Ok((0..digits.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&digits[at..at + 2], 16).expect("validated hex digits"))
        .collect())
}

/// Apply a binary(n) or varbinary(n) length: truncate, and pad binary with
/// zeros. `width` -1 is MAX.
pub(super) fn fit(mut bytes: Vec<u8>, width: i32, fixed: bool) -> Vec<u8> {
    if width >= 0 {
        let width = width as usize;
        bytes.truncate(width);
        if fixed {
            bytes.resize(width, 0);
        }
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styles_match_sql_server() {
        assert_eq!(to_text(&[1, 2, 3], 2, false).unwrap(), "010203");
        assert_eq!(to_text(&[0x0a, 0x0b], 1, false).unwrap(), "0x0A0B");
        assert_eq!(to_text(b"ABC", 0, false).unwrap(), "ABC");
        assert_eq!(to_text(&[], 1, false).unwrap(), "0x");
        assert_eq!(to_text(&[], 2, false).unwrap(), "");
        assert_eq!(to_text(&[1], 3, false), Err(Error::Style));
        assert_eq!(from_text("0x0102", 1, None).unwrap(), vec![1, 2]);
        assert_eq!(from_text("0x0a0B", 1, None).unwrap(), vec![10, 11]);
        assert_eq!(from_text("0102", 2, None).unwrap(), vec![1, 2]);
        assert_eq!(from_text("abc", 0, None).unwrap(), b"abc".to_vec());
        assert_eq!(from_text("0x", 1, None).unwrap(), Vec::<u8>::new());
        assert_eq!(from_text("", 2, None).unwrap(), Vec::<u8>::new());
        for (text, style) in [
            ("0x123", 1),
            ("123", 2),
            ("0102", 1),
            ("0x0102", 2),
            ("0xZZ", 1),
            ("ZZ", 2),
            ("", 1),
        ] {
            assert_eq!(
                from_text(text, style, None),
                Err(Error::Syntax),
                "{text} {style}"
            );
        }
        assert_eq!(from_text("abc", 3, None), Err(Error::Style));
        assert_eq!(fit(vec![1, 2], 4, true), vec![1, 2, 0, 0]);
        assert_eq!(fit(vec![1, 2, 3], 2, false), vec![1, 2]);
        assert_eq!(fit(vec![1, 2, 3], -1, false), vec![1, 2, 3]);
    }
}
