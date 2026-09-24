//! Base64 conversion for explicit OPENJSON binary columns.
use super::*;
const ENCODING: &str = "Cannot convert a string value found in the JSON text to binary value because it is not Base64 encoded.";
const WIDTH: &str =
    "Base64 encoded string cannot be converted to binary value. Binary data would be truncated.";
pub(super) fn diagnostic(message: &str) -> Option<SqlError> {
    match message {
        ENCODING => Some(SqlError::new(13612, 1, ENCODING)),
        WIDTH => Some(SqlError::new(13613, 1, WIDTH)),
        _ => None,
    }
}
fn decode64(text: &str) -> Result<Vec<u8>, &'static str> {
    let bytes = text
        .bytes()
        .filter(|b| !matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
        .collect::<Vec<_>>();
    if bytes.len() % 4 != 0 {
        return Err(ENCODING);
    };
    let digit = |b| match b {
        b'A'..=b'Z' => Ok(b - b'A'),
        b'a'..=b'z' => Ok(b - b'a' + 26),
        b'0'..=b'9' => Ok(b - b'0' + 52),
        b'+' => Ok(62),
        b'/' => Ok(63),
        _ => Err(ENCODING),
    };
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for (i, chunk) in bytes.chunks_exact(4).enumerate() {
        let a = digit(chunk[0])?;
        let b = digit(chunk[1])?;
        out.push((a << 2) | (b >> 4));
        if chunk[2] == b'=' {
            if chunk[3] != b'=' || i + 1 != bytes.len() / 4 || b & 15 != 0 {
                return Err(ENCODING);
            };
        } else {
            let c = digit(chunk[2])?;
            out.push((b << 4) | (c >> 2));
            if chunk[3] == b'=' {
                if i + 1 != bytes.len() / 4 || c & 3 != 0 {
                    return Err(ENCODING);
                };
            } else {
                out.push((c << 6) | digit(chunk[3])?);
            }
        }
    }
    Ok(out)
}
/// Convert a selected JSON string from Base64 to binary.
/// Width -1 means MAX; bounded widths are 1..=8000. Fixed widths pad with zeros.
/// Ordinary SQL binary casts have separate semantics.
pub fn convert(
    source: &str,
    path: &str,
    width: i32,
    fixed: bool,
) -> Result<Option<Vec<u8>>, &'static str> {
    let Some(value) = schema::resolve(source, path, false)? else {
        return Ok(None);
    };
    let kind = root(value.as_bytes()).ok_or(DOCUMENT)?;
    if value == "null" {
        return Ok(None);
    };
    if matches!(kind, Kind::Object | Kind::Array) {
        return schema::column(source, path, false).map(|_| None);
    }
    if kind != Kind::String {
        return Err("OPENJSON binary conversion of non-string scalars is not yet supported");
    };
    fit(decode64(&decode(value)?)?, width, fixed).map(Some)
}

fn fit(mut bytes: Vec<u8>, width: i32, fixed: bool) -> Result<Vec<u8>, &'static str> {
    if width != -1 {
        if !(1..=8000).contains(&width) {
            return Err("invalid OPENJSON binary width");
        };
        if bytes.len() > width as usize {
            return Err(WIDTH);
        };
        if fixed {
            bytes.resize(width as usize, 0);
        }
    } else if fixed {
        return Err("invalid OPENJSON binary width");
    };
    Ok(bytes)
}

/// Base64 is ASCII; reject other code units without replacement or struct casts.
pub fn convert_utf16(
    source: &[u16],
    path: &[u16],
    width: i32,
    fixed: bool,
) -> Result<Option<Vec<u8>>, &'static str> {
    let Some(value) = schema::resolve_utf16(source, path, false)? else {
        return Ok(None);
    };
    let syntax = json::json_syntax(value);
    let kind = root(&syntax).ok_or(DOCUMENT)?;
    if syntax == b"null" {
        return Ok(None);
    }
    if matches!(kind, Kind::Object | Kind::Array) {
        return schema::column_utf16(source, path, false).map(|_| None);
    }
    if kind != Kind::String {
        return Err("OPENJSON binary conversion of non-string scalars is not yet supported");
    }
    let units = json::decode_utf16(value).map_err(|_| DOCUMENT)?;
    if units.iter().any(|&unit| unit > 127) {
        return Err(ENCODING);
    }
    let text: String = units.into_iter().map(|unit| unit as u8 as char).collect();
    fit(decode64(&text)?, width, fixed).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn base64_padding_and_widths() {
        for (text, want) in [
            ("", vec![]),
            ("AQ==", vec![1]),
            ("AQI=", vec![1, 2]),
            ("AP8B", vec![0, 255, 1]),
            (" /w==\r\n", vec![255]),
        ] {
            assert_eq!(decode64(text).unwrap(), want);
        }
        for text in [
            "A", "AQ", "AQ=", "=AAA", "AQ==AA==", "AR==", "AQL=", "____", "é", "A==A",
        ] {
            assert_eq!(decode64(text), Err(ENCODING));
        }
        assert_eq!(
            convert("\"AQI=\"", "$", 4, true).unwrap(),
            Some(vec![1, 2, 0, 0])
        );
        assert_eq!(convert("\"AQI=\"", "$", 1, false), Err(WIDTH));
        assert_eq!(convert("null", "$", 1, false).unwrap(), None);
    }
}
