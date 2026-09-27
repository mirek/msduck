//! Deterministic JSON lexical grammar, independent of AST and database adapters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Object,
    Array,
    String,
    Number,
    Literal,
}
#[derive(Clone, Copy)]
enum State {
    Value,
    ArrayFirst,
    ArrayValue,
    ArrayComma,
    ObjectFirst,
    ObjectKey,
    ObjectColon,
    ObjectComma,
}
struct Scan<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl Scan<'_> {
    fn ws(&mut self) {
        while self
            .bytes
            .get(self.at)
            .is_some_and(|c| matches!(c, b' ' | b'\t' | b'\r' | b'\n'))
        {
            self.at += 1;
        }
    }
    fn take(&mut self, c: u8) -> bool {
        if self.bytes.get(self.at) == Some(&c) {
            self.at += 1;
            true
        } else {
            false
        }
    }
    fn string(&mut self) -> Option<()> {
        if !self.take(b'"') {
            return None;
        }
        loop {
            let c = *self.bytes.get(self.at)?;
            self.at += 1;
            match c {
                b'"' => return Some(()),
                0..=31 => return None,
                b'\\' => {
                    let escape = *self.bytes.get(self.at)?;
                    self.at += 1;
                    match escape {
                        b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => {}
                        b'u' => {
                            for _ in 0..4 {
                                if !self.bytes.get(self.at)?.is_ascii_hexdigit() {
                                    return None;
                                }
                                self.at += 1;
                            }
                        }
                        _ => return None,
                    }
                }
                _ => {}
            }
        }
    }
    fn digits(&mut self) -> bool {
        let start = self.at;
        while self.bytes.get(self.at).is_some_and(u8::is_ascii_digit) {
            self.at += 1;
        }
        self.at > start
    }
    fn number(&mut self) -> Option<()> {
        self.take(b'-');
        if !self.take(b'0') && !self.digits() {
            return None;
        }
        if self.take(b'.') && !self.digits() {
            return None;
        }
        if self.take(b'e') || self.take(b'E') {
            if !self.take(b'+') {
                self.take(b'-');
            }
            if !self.digits() {
                return None;
            }
        }
        Some(())
    }
}
/// ISJSON reports a distinct error when a valid value exceeds the engine's
/// nesting limit. Syntax errors remain ordinary invalid JSON.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DepthLimit;

const ISJSON_MAX_DEPTH: usize = 128;

fn scan_prefix(text: &[u8], max_depth: Option<usize>) -> Result<Option<(Kind, usize)>, DepthLimit> {
    let mut s = Scan { bytes: text, at: 0 };
    let mut overflow = false;
    let result = (|| {
        s.ws();
        let kind = match s.bytes.get(s.at)? {
            b'{' => Kind::Object,
            b'[' => Kind::Array,
            b'"' => Kind::String,
            b'-' | b'0'..=b'9' => Kind::Number,
            _ => Kind::Literal,
        };
        let mut depth = 0;
        let mut stack = vec![State::Value];
        while let Some(state) = stack.pop() {
            s.ws();
            match state {
                State::Value => {
                    match s.bytes.get(s.at)? {
                        b'{' => {
                            s.at += 1;
                            depth += 1;
                            stack.push(State::ObjectFirst);
                            continue;
                        }
                        b'[' => {
                            s.at += 1;
                            depth += 1;
                            stack.push(State::ArrayFirst);
                            continue;
                        }
                        b'"' => s.string()?,
                        b't' | b'f' | b'n' => {
                            let literal: &[u8] = match s.bytes[s.at] {
                                b't' => b"true",
                                b'f' => b"false",
                                _ => b"null",
                            };
                            if !s.bytes[s.at..].starts_with(literal) {
                                return None;
                            }
                            s.at += literal.len();
                        }
                        b'-' | b'0'..=b'9' => s.number()?,
                        _ => return None,
                    }
                    if max_depth.is_some_and(|limit| depth > limit) {
                        overflow = true;
                        return None;
                    }
                }
                State::ArrayFirst => {
                    if s.take(b']') {
                        if max_depth.is_some_and(|limit| depth > limit + 1) {
                            overflow = true;
                            return None;
                        }
                        depth -= 1;
                    } else {
                        stack.push(State::ArrayComma);
                        stack.push(State::Value);
                    }
                }
                State::ArrayValue => {
                    stack.push(State::ArrayComma);
                    stack.push(State::Value);
                }
                State::ArrayComma => {
                    if s.take(b',') {
                        stack.push(State::ArrayValue);
                    } else if s.take(b']') {
                        depth -= 1;
                    } else {
                        return None;
                    }
                }
                State::ObjectFirst => {
                    if s.take(b'}') {
                        if max_depth.is_some_and(|limit| depth > limit + 1) {
                            overflow = true;
                            return None;
                        }
                        depth -= 1;
                    } else {
                        s.string()?;
                        stack.push(State::ObjectColon);
                    }
                }
                State::ObjectKey => {
                    s.string()?;
                    stack.push(State::ObjectColon);
                }
                State::ObjectColon => {
                    if !s.take(b':') {
                        return None;
                    }
                    stack.push(State::ObjectComma);
                    stack.push(State::Value);
                }
                State::ObjectComma => {
                    if s.take(b',') {
                        stack.push(State::ObjectKey);
                    } else if s.take(b'}') {
                        depth -= 1;
                    } else {
                        return None;
                    }
                }
            }
        }
        Some((kind, s.at))
    })();
    if overflow {
        Err(DepthLimit)
    } else {
        Ok(result)
    }
}

pub fn prefix(text: &[u8]) -> Option<(Kind, usize)> {
    scan_prefix(text, None).ok().flatten()
}
pub fn root(text: &[u8]) -> Option<Kind> {
    let (kind, end) = prefix(text)?;
    text[end..]
        .iter()
        .all(|c| matches!(c, b' ' | b'\t' | b'\r' | b'\n'))
        .then_some(kind)
}
/// Validate JSON with SQL ISJSON modes: 0 object/array, 1 any JSON value,
/// 2 array, 3 object, 4 string/number. Unknown modes return false.
pub fn valid(text: &[u8], mode: u8) -> bool {
    let Some(kind) = root(text) else {
        return false;
    };
    match mode {
        0 => matches!(kind, Kind::Object | Kind::Array),
        1 => true,
        2 => kind == Kind::Array,
        3 => kind == Kind::Object,
        4 => matches!(kind, Kind::Number | Kind::String),
        _ => false,
    }
}

/// Validate ISJSON input while retaining SQL Server's depth error separately
/// from malformed JSON. Other JSON consumers continue to use the unrestricted
/// lexical prefix until their own depth/error rules are captured.
pub fn isjson(text: &[u8], mode: u8) -> Result<bool, DepthLimit> {
    let Some((kind, end)) = scan_prefix(text, Some(ISJSON_MAX_DEPTH))? else {
        return Ok(false);
    };
    if !text[end..]
        .iter()
        .all(|c| matches!(c, b' ' | b'\t' | b'\r' | b'\n'))
    {
        return Ok(false);
    }
    Ok(match mode {
        0 => matches!(kind, Kind::Object | Kind::Array),
        1 => true,
        2 => kind == Kind::Array,
        3 => kind == Kind::Object,
        4 => matches!(kind, Kind::Number | Kind::String),
        _ => false,
    })
}

/// Validate SQL Server JSON stored as UTF-16 code units. Structural syntax is
/// ASCII; every non-ASCII code unit is ordinary string content and invalid
/// outside a quoted string. In particular, SQL Server accepts isolated surrogate
/// units in strings. Map only for grammar recognition, never for returned text.
pub fn valid_utf16(text: &[u16], mode: u8) -> bool {
    let syntax: Vec<u8> = text
        .iter()
        .map(|&unit| if unit <= 0x7f { unit as u8 } else { 0x80 })
        .collect();
    valid(&syntax, mode)
}

pub fn isjson_utf16(text: &[u16], mode: u8) -> Result<bool, DepthLimit> {
    let syntax: Vec<u8> = text
        .iter()
        .map(|&unit| if unit <= 0x7f { unit as u8 } else { 0x80 })
        .collect();
    isjson(&syntax, mode)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isjson_depth_and_unicode_match_retained_sql_server_capture() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../../reference/isjson-depth.json")).unwrap();
        let runs = fixture["containers"][0]["runs"].as_array().unwrap();
        assert_eq!(runs[0], runs[1]);
        let entries = runs[0].as_array().unwrap();
        assert_eq!(entries.len(), 110);
        for entry in entries {
            let input = &entry["input"];
            let depth = input["depth"].as_u64().unwrap_or(0) as usize;
            let text = match input["form"].as_str().unwrap() {
                "array" => "[".repeat(depth) + "0" + &"]".repeat(depth),
                "object" => "{\"v\":".repeat(depth) + "0" + &"}".repeat(depth),
                "empty-array" | "empty-at-depth" => "[".repeat(depth) + &"]".repeat(depth),
                "empty-object" => "{\"v\":".repeat(depth - 1) + "{}" + &"}".repeat(depth - 1),
                "invalid-before-depth" => {
                    "[x".to_owned() + &"[".repeat(depth - 1) + "0" + &"]".repeat(depth)
                }
                "invalid-after-depth" => "[".repeat(depth) + "x" + &"]".repeat(depth),
                "unclosed-after-depth" => "[".repeat(depth) + "0" + &"]".repeat(depth - 1),
                "incomplete-after-depth" => "[".repeat(depth) + "1e" + &"]".repeat(depth),
                "openings-only" => "[".repeat(depth),
                "trailing-after-depth" => "[".repeat(depth) + "0" + &"]".repeat(depth) + "x",
                "units" => String::new(),
                other => panic!("unrecognized fixture input {other}"),
            };
            let units: Vec<u16> = if input["form"] == "units" {
                input["units"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|unit| unit.as_u64().unwrap() as u16)
                    .collect()
            } else {
                text.encode_utf16().collect()
            };
            let mode = if entry["mode"] == "default" { 0 } else { 1 };
            let observed = isjson_utf16(&units, mode);
            let result = &entry["result"];
            let expected = if result["errors"].as_array().unwrap().is_empty() {
                Ok(result["sets"][0]["rows"][0][0].as_i64().unwrap() != 0)
            } else {
                assert_eq!(result["errors"][0]["number"], 13606);
                Err(DepthLimit)
            };
            assert_eq!(observed, expected, "{} / {}", entry["name"], entry["mode"]);
        }
        assert_eq!(
            isjson(&("[".repeat(129) + "0" + &"]".repeat(129)).into_bytes(), 0),
            Err(DepthLimit)
        );
    }

    #[test]
    fn lexical_grammar_and_deep_nesting() {
        for text in [
            r#"{"a":1,"a":2}"#,
            r#"[1,true,null,{"x":"a\u0062"}]"#,
            r#"[1e99999999999999999999]"#,
            r#"["\ud800"]"#,
        ] {
            assert!(valid(text.as_bytes(), 0), "{text}");
        }
        for text in [
            "",
            "01",
            "-01",
            "+1",
            "1.",
            "1e",
            "NaN",
            "Infinity",
            "[1,]",
            "{\"a\":1,}",
            "{1:2}",
            "[true false]",
            "{}[]",
            "[\"a\n\"]",
            "[\"\\x\"]",
            "[\"\\u123\"]",
            "\u{feff}{}",
        ] {
            assert!(!valid(text.as_bytes(), 1), "{text}");
        }
        let text = "[".repeat(20000) + "0" + &"]".repeat(20000);
        assert!(valid(text.as_bytes(), 0));
        assert!(!valid(&text.as_bytes()[..text.len() - 1], 0));
    }
    #[test]
    fn utf16_grammar_preserves_sql_server_surrogate_acceptance() {
        // reference/unicode-json-storage.json: both raw and escaped isolated
        // units are valid JSON; non-ASCII code units are not JSON whitespace.
        for unit in 0x80..=u16::MAX {
            assert!(valid_utf16(
                &[b'[' as u16, b'"' as u16, unit, b'"' as u16, b']' as u16],
                0
            ));
            assert!(!valid_utf16(&[unit], 1));
        }
        for text in [
            r#"{"s":"\ud800"}"#,
            r#"{"s":"\udc00\ud800"}"#,
            "[true,false,null,1e20]",
            "[\"雪🦆\"]",
        ] {
            let units: Vec<_> = text.encode_utf16().collect();
            for mode in 0..=4 {
                assert_eq!(valid_utf16(&units, mode), valid(text.as_bytes(), mode));
            }
        }
        for unit in 0..32 {
            assert!(!valid_utf16(&[b'"' as u16, unit, b'"' as u16], 4));
        }
        assert!(!valid_utf16(
            &"{\"s\":\"ok\",\"bad\":invalid}"
                .encode_utf16()
                .collect::<Vec<_>>(),
            0
        ));
    }
}
