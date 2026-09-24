//! SQL JSON string escaping without database or parser dependencies.
use std::borrow::Cow;

pub const INVALID_FORMAT: &str = "An invalid value was specified for argument 2.";

/// Escape string contents, without adding JSON quotation marks.
/// SQL NULL handling belongs to the adapter. Only JSON escaping is supported.
pub fn escape<'a>(source: &'a str, format: &str) -> Result<Cow<'a, str>, &'static str> {
    if !format.eq_ignore_ascii_case("json") {
        return Err(INVALID_FORMAT);
    }
    if !source
        .bytes()
        .any(|c| c < 32 || matches!(c, b'"' | b'\\' | b'/'))
    {
        return Ok(Cow::Borrowed(source));
    }
    let mut output = String::with_capacity(source.len());
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for c in source.chars() {
        match c {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '/' => output.push_str("\\/"),
            '\u{8}' => output.push_str("\\b"),
            '\u{c}' => output.push_str("\\f"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            c if c < ' ' => {
                output.push_str("\\u00");
                output.push(char::from(HEX[c as usize / 16]));
                output.push(char::from(HEX[c as usize % 16]));
            }
            c => output.push(c),
        }
    }
    Ok(Cow::Owned(output))
}

/// Escape UTF-16 contents without replacing, pairing or escaping surrogate
/// units. SQL Server leaves all non-control Unicode units unchanged, including
/// isolated high/low surrogates (see the Unicode JSON reference capture).
pub fn escape_utf16<'a>(source: &'a [u16], format: &[u16]) -> Result<Cow<'a, [u16]>, &'static str> {
    if format.len() != 4
        || !format.iter().zip(b"json").all(|(&unit, &ascii)| {
            unit == u16::from(ascii) || unit == u16::from(ascii.to_ascii_uppercase())
        })
    {
        return Err(INVALID_FORMAT);
    }
    if !source
        .iter()
        .any(|&unit| unit < 32 || matches!(unit, 34 | 47 | 92))
    {
        return Ok(Cow::Borrowed(source));
    }
    let mut output = Vec::with_capacity(source.len());
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for &unit in source {
        let short = match unit {
            34 => Some(b'"'),
            92 => Some(b'\\'),
            47 => Some(b'/'),
            8 => Some(b'b'),
            12 => Some(b'f'),
            10 => Some(b'n'),
            13 => Some(b'r'),
            9 => Some(b't'),
            _ => None,
        };
        if let Some(escaped) = short {
            output.extend([u16::from(b'\\'), u16::from(escaped)]);
        } else if unit < 32 {
            output.extend(b"\\u00".iter().copied().map(u16::from));
            output.extend([
                u16::from(HEX[usize::from(unit / 16)]),
                u16::from(HEX[usize::from(unit % 16)]),
            ]);
        } else {
            output.push(unit);
        }
    }
    Ok(Cow::Owned(output))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sql_json_special_characters_and_unicode() {
        assert_eq!(
            escape("\"\\/\u{8}\u{c}\n\r\t", "json").unwrap(),
            r#"\"\\\/\b\f\n\r\t"#
        );
        assert_eq!(
            escape("\0\u{1}\u{b}\u{e}\u{1f}", "json").unwrap(),
            r"\u0000\u0001\u000b\u000e\u001f"
        );
        for text in ["", "plain text", "é雪🦆\u{7f}\u{2028}\u{2029}"] {
            assert_eq!(escape(text, "json").unwrap(), text);
        }
        let all = (0..=31).map(char::from).collect::<String>() + "\"/\\雪🦆";
        let escaped = escape(&all, "json").unwrap();
        let decoded: String = serde_json::from_str(&format!("\"{escaped}\"")).unwrap();
        assert_eq!(decoded, all);
    }
    #[test]
    fn json_format_and_unbounded_expansion() {
        assert_eq!(escape("/", "JSON").unwrap(), r"\/");
        for format in ["", "xml", "url", "json ", " json"] {
            assert_eq!(escape("x", format), Err(INVALID_FORMAT));
        }
        let input = "\0🦆/".repeat(5000);
        assert_eq!(escape(&input, "json").unwrap(), "\\u0000🦆\\/".repeat(5000));
    }
    #[test]
    fn utf16_escape_keeps_raw_units_and_matches_existing_unicode_rules() {
        let format: Vec<_> = "JSON".encode_utf16().collect();
        for source in [
            vec![0xd83e],
            vec![0xdd86],
            vec![0xdd86, 0xd83e],
            vec![0xd83e, 0xdd86],
            vec![0xd83e, 120],
        ] {
            assert_eq!(escape_utf16(&source, &format).unwrap().as_ref(), &source);
        }
        let source = [0xd83e, 0, 47, 0xdd86];
        assert_eq!(
            escape_utf16(&source, &format).unwrap().as_ref(),
            &[0xd83e, 92, 117, 48, 48, 48, 48, 92, 47, 0xdd86]
        );
        let source = (0..=31).map(char::from).collect::<String>() + "\"/\\雪🦆";
        let units: Vec<_> = source.encode_utf16().collect();
        assert_eq!(
            escape_utf16(&units, &format).unwrap().as_ref(),
            escape(&source, "json")
                .unwrap()
                .encode_utf16()
                .collect::<Vec<_>>()
        );
        for format in [
            vec![],
            vec![106, 115, 111, 110, 32],
            vec![106, 115, 111, 0xd800],
        ] {
            assert_eq!(escape_utf16(&[], &format), Err(INVALID_FORMAT));
        }
    }
}
