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
pub fn prefix(text: &[u8]) -> Option<(Kind, usize)> {
    let mut s = Scan { bytes: text, at: 0 };
    s.ws();
    let kind = match s.bytes.get(s.at)? {
        b'{' => Kind::Object,
        b'[' => Kind::Array,
        b'"' => Kind::String,
        b'-' | b'0'..=b'9' => Kind::Number,
        _ => Kind::Literal,
    };
    let mut stack = vec![State::Value];
    while let Some(state) = stack.pop() {
        s.ws();
        match state {
            State::Value => match s.bytes.get(s.at)? {
                b'{' => {
                    s.at += 1;
                    stack.push(State::ObjectFirst);
                }
                b'[' => {
                    s.at += 1;
                    stack.push(State::ArrayFirst);
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
            },
            State::ArrayFirst => {
                if !s.take(b']') {
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
                } else if !s.take(b']') {
                    return None;
                }
            }
            State::ObjectFirst => {
                if !s.take(b'}') {
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
                } else if !s.take(b'}') {
                    return None;
                }
            }
        }
    }
    Some((kind, s.at))
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

#[cfg(test)]
mod tests {
    use super::*;
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
}
