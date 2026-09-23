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
}
