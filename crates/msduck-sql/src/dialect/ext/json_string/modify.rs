//! JSON_MODIFY document editing, from reference/json-constructors.json and
//! reference/gaps-json_string.json.
//!
//! The document is edited in place on its UTF-16 text: untouched text,
//! including whitespace and duplicate keys, is kept exactly. Values arrive
//! already serialized as JSON text (or SQL NULL); typing and string escaping
//! belong to the caller.
use super::error;

/// A parsed `[append] [lax | strict] $...` path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Path {
    pub append: bool,
    pub strict: bool,
    pub steps: Vec<Step>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Step {
    Key(Vec<u16>),
    Index(usize),
}

const PATH_FORMAT: &str = "JSON path is not properly formatted.";
const DOCUMENT_FORMAT: &str = "JSON text is not properly formatted.";
const MISSING: &str = "Property cannot be found on the specified JSON path.";
const NOT_ARRAY: &str = "Array cannot be found in the specified JSON path.";
const ROOT: &str = "Unsupported JSON path found in argument 2 of JSON_MODIFY.";
const ADVANCED: &str = "JsonModify not yet supported for advanced JSON array accessors.";

fn unexpected(prefix: &str, units: &[u16], at: usize) -> String {
    let character = units
        .get(at)
        .map(|&unit| char::from_u32(u32::from(unit)).unwrap_or(char::REPLACEMENT_CHARACTER))
        .unwrap_or('.');
    format!("{prefix} Unexpected character '{character}' is found at position {at}.")
}

fn path_error(state: u8, units: &[u16], at: usize) -> String {
    error(13607, state, 16, &unexpected(PATH_FORMAT, units, at))
}

fn is_space(unit: u16) -> bool {
    matches!(unit, 0x20 | 0x09 | 0x0a | 0x0d)
}

fn starts_with_ignore_case(units: &[u16], at: usize, word: &str) -> bool {
    word.len() <= units.len().saturating_sub(at)
        && word
            .bytes()
            .zip(&units[at..])
            .all(|(byte, &unit)| unit < 0x80 && (unit as u8).eq_ignore_ascii_case(&byte))
}

/// Parse a JSON_MODIFY path. Errors are json_string markers.
pub fn path(units: &[u16]) -> Result<Path, String> {
    let mut at = 0;
    let end = units.len()
        - units
            .iter()
            .rev()
            .take_while(|&&unit| is_space(unit))
            .count();
    let skip = |mut at: usize| {
        while at < end && is_space(units[at]) {
            at += 1;
        }
        at
    };
    at = skip(at);
    if at == end {
        return Err(path_error(14, units, at));
    }
    let mut append = false;
    if starts_with_ignore_case(units, at, "append")
        && units.get(at + 6).is_some_and(|&unit| is_space(unit))
    {
        append = true;
        at = skip(at + 6);
    }
    let mut strict = false;
    for (word, value) in [("strict", true), ("lax", false)] {
        if starts_with_ignore_case(units, at, word)
            && units
                .get(at + word.len())
                .is_some_and(|&unit| is_space(unit) || unit == u16::from(b'$'))
        {
            strict = value;
            at = skip(at + word.len());
            break;
        }
    }
    if units.get(at) != Some(&u16::from(b'$')) || at >= end {
        return Err(path_error(22, units, at));
    }
    at += 1;
    let mut steps = Vec::new();
    while at < end {
        match units[at] {
            0x2e => {
                // '.'
                at += 1;
                if at == end {
                    return Err(path_error(14, units, at));
                }
                if units[at] == u16::from(b'*') {
                    return Err(error(13660, 4, 16, ADVANCED));
                }
                if units[at] == u16::from(b'"') {
                    let (key, next) =
                        string(units, at, end).map_err(|_| error(13607, 1, 16, PATH_FORMAT))?;
                    steps.push(Step::Key(key));
                    at = next;
                } else {
                    let start = at;
                    while at < end && !matches!(units[at], 0x2e | 0x5b) {
                        at += 1;
                    }
                    let key = &units[start..at];
                    let valid = String::from_utf16(key).is_ok_and(|key| {
                        key.chars().enumerate().all(|(i, c)| {
                            c == '_'
                                || c == '$'
                                || c.is_alphabetic()
                                || (i > 0 && c.is_ascii_digit())
                        })
                    });
                    if !valid {
                        return Err(error(13607, 1, 16, PATH_FORMAT));
                    }
                    steps.push(Step::Key(key.to_vec()));
                }
            }
            0x5b => {
                // '['
                at += 1;
                let start = at;
                while at < end && units[at] != u16::from(b']') {
                    at += 1;
                }
                if at == end {
                    return Err(error(13607, 1, 16, PATH_FORMAT));
                }
                let selector = &units[start..at];
                if selector == [u16::from(b'*')] || selector.contains(&u16::from(b' ')) {
                    return Err(error(13660, 4, 16, ADVANCED));
                }
                if selector.is_empty() {
                    return Err(path_error(21, units, start));
                }
                let mut index = 0usize;
                for (offset, &unit) in selector.iter().enumerate() {
                    if !(0x30..=0x39).contains(&unit) {
                        return Err(path_error(21, units, start + offset));
                    }
                    index = index
                        .checked_mul(10)
                        .and_then(|n| n.checked_add(usize::from(unit - 0x30)))
                        .ok_or_else(|| path_error(21, units, start + offset))?;
                }
                steps.push(Step::Index(index));
                at += 1;
            }
            _ => return Err(path_error(21, units, at)),
        }
    }
    if steps.is_empty() && !append {
        return Err(error(13619, 1, 16, ROOT));
    }
    Ok(Path {
        append,
        strict,
        steps,
    })
}

/// A parsed JSON value with UTF-16 spans `[start, end)`.
#[derive(Debug)]
enum Node {
    Object {
        start: usize,
        end: usize,
        members: Vec<Member>,
    },
    Array {
        start: usize,
        end: usize,
        items: Vec<Node>,
    },
    Scalar {
        start: usize,
        end: usize,
    },
}

#[derive(Debug)]
struct Member {
    key_start: usize,
    key: Vec<u16>,
    value: Node,
    /// Index of the comma that follows this member's value, if any.
    comma: Option<usize>,
}

impl Node {
    fn span(&self) -> (usize, usize) {
        match self {
            Node::Object { start, end, .. }
            | Node::Array { start, end, .. }
            | Node::Scalar { start, end } => (*start, *end),
        }
    }
}

const MAX_DEPTH: usize = 512;

/// Decode a JSON string token starting at `at`; returns the decoded units
/// and the index after the closing quote, or the offending position.
fn string(units: &[u16], mut at: usize, end: usize) -> Result<(Vec<u16>, usize), usize> {
    if units.get(at) != Some(&u16::from(b'"')) {
        return Err(at);
    }
    at += 1;
    let mut out = Vec::new();
    loop {
        if at >= end {
            return Err(at);
        }
        let unit = units[at];
        match unit {
            0x22 => return Ok((out, at + 1)),
            0x5c => {
                let escape = *units.get(at + 1).filter(|_| at + 1 < end).ok_or(at + 1)?;
                at += 2;
                out.push(match escape {
                    0x22 | 0x5c | 0x2f => escape,
                    0x62 => 0x08,
                    0x66 => 0x0c,
                    0x6e => 0x0a,
                    0x72 => 0x0d,
                    0x74 => 0x09,
                    0x75 => {
                        let mut value = 0u16;
                        for offset in 0..4 {
                            let digit = units
                                .get(at + offset)
                                .filter(|_| at + offset < end)
                                .and_then(|&d| char::from_u32(u32::from(d))?.to_digit(16))
                                .ok_or(at + offset)?;
                            value = value * 16 + digit as u16;
                        }
                        at += 4;
                        value
                    }
                    _ => return Err(at - 1),
                });
            }
            0x00..=0x1f => return Err(at),
            _ => {
                out.push(unit);
                at += 1;
            }
        }
    }
}

struct Parser<'a> {
    units: &'a [u16],
    at: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.at < self.units.len() && is_space(self.units[self.at]) {
            self.at += 1;
        }
    }
    fn take(&mut self, byte: u8) -> bool {
        if self.units.get(self.at) == Some(&u16::from(byte)) {
            self.at += 1;
            true
        } else {
            false
        }
    }
    fn digits(&mut self) -> usize {
        let start = self.at;
        while self
            .units
            .get(self.at)
            .is_some_and(|unit| (0x30..=0x39).contains(unit))
        {
            self.at += 1;
        }
        self.at - start
    }
    fn value(&mut self, depth: usize) -> Result<Node, usize> {
        if depth > MAX_DEPTH {
            return Err(self.at);
        }
        self.ws();
        let start = self.at;
        match self.units.get(self.at).copied().ok_or(self.at)? {
            0x7b => {
                self.at += 1;
                let mut members: Vec<Member> = Vec::new();
                self.ws();
                if self.take(b'}') {
                    return Ok(Node::Object {
                        start,
                        end: self.at,
                        members,
                    });
                }
                loop {
                    self.ws();
                    let key_start = self.at;
                    let (key, next) = string(self.units, self.at, self.units.len())?;
                    self.at = next;
                    self.ws();
                    if !self.take(b':') {
                        return Err(self.at);
                    }
                    let value = self.value(depth + 1)?;
                    self.ws();
                    if self.units.get(self.at) == Some(&u16::from(b',')) {
                        members.push(Member {
                            key_start,
                            key,
                            value,
                            comma: Some(self.at),
                        });
                        self.at += 1;
                    } else if self.take(b'}') {
                        members.push(Member {
                            key_start,
                            key,
                            value,
                            comma: None,
                        });
                        return Ok(Node::Object {
                            start,
                            end: self.at,
                            members,
                        });
                    } else {
                        return Err(self.at);
                    }
                }
            }
            0x5b => {
                self.at += 1;
                let mut items = Vec::new();
                self.ws();
                if self.take(b']') {
                    return Ok(Node::Array {
                        start,
                        end: self.at,
                        items,
                    });
                }
                loop {
                    items.push(self.value(depth + 1)?);
                    self.ws();
                    if self.take(b',') {
                        continue;
                    }
                    if self.take(b']') {
                        return Ok(Node::Array {
                            start,
                            end: self.at,
                            items,
                        });
                    }
                    return Err(self.at);
                }
            }
            0x22 => {
                let (_, next) = string(self.units, self.at, self.units.len())?;
                self.at = next;
                Ok(Node::Scalar {
                    start,
                    end: self.at,
                })
            }
            0x74 | 0x66 | 0x6e => {
                let word: &[u8] = match self.units[self.at] {
                    0x74 => b"true",
                    0x66 => b"false",
                    _ => b"null",
                };
                for &byte in word {
                    if !self.take(byte) {
                        return Err(self.at);
                    }
                }
                Ok(Node::Scalar {
                    start,
                    end: self.at,
                })
            }
            0x2d | 0x30..=0x39 => {
                self.take(b'-');
                if self.take(b'0') {
                } else if self.digits() == 0 {
                    return Err(self.at);
                }
                if self.take(b'.') && self.digits() == 0 {
                    return Err(self.at);
                }
                if self.take(b'e') || self.take(b'E') {
                    if !self.take(b'+') {
                        self.take(b'-');
                    }
                    if self.digits() == 0 {
                        return Err(self.at);
                    }
                }
                Ok(Node::Scalar {
                    start,
                    end: self.at,
                })
            }
            _ => Err(self.at),
        }
    }
}

/// Parse and validate a document whose root must be an object or array.
fn document(units: &[u16]) -> Result<Node, String> {
    let mut parser = Parser { units, at: 0 };
    parser.ws();
    let start = parser.at;
    let invalid = |at: usize| error(13609, 7, 16, &unexpected(DOCUMENT_FORMAT, units, at));
    if !matches!(units.get(start), Some(0x7b | 0x5b)) {
        return Err(invalid(start));
    }
    let root = parser.value(0).map_err(invalid)?;
    parser.ws();
    if parser.at != units.len() {
        return Err(invalid(parser.at));
    }
    Ok(root)
}

/// Apply JSON_MODIFY. `value` is the serialized JSON text of the new value,
/// or None for SQL NULL (delete in lax mode, `null` in strict mode).
pub fn modify(doc: &[u16], path: &Path, value: Option<&[u16]>) -> Result<Vec<u16>, String> {
    let root = document(doc)?;
    let missing = || {
        if path.strict {
            Err(error(13608, 2, 16, MISSING))
        } else {
            Ok(doc.to_vec())
        }
    };
    let null: Vec<u16> = "null".encode_utf16().collect();
    let written = value.unwrap_or(&null);
    let splice = |start: usize, end: usize, text: &[u16]| {
        let mut out = Vec::with_capacity(doc.len() + text.len());
        out.extend_from_slice(&doc[..start]);
        out.extend_from_slice(text);
        out.extend_from_slice(&doc[end..]);
        out
    };
    let append_to = |node: &Node| -> Option<Vec<u16>> {
        let Node::Array { end, items, .. } = node else {
            return None;
        };
        let mut text = Vec::new();
        if !items.is_empty() {
            text.push(u16::from(b','));
        }
        text.extend_from_slice(written);
        Some(splice(end - 1, end - 1, &text))
    };
    let not_array = || {
        if path.strict {
            Err(error(13621, 1, 16, NOT_ARRAY))
        } else {
            Ok(doc.to_vec())
        }
    };
    // Walk to the parent of the last step.
    let Some((last, parents)) = path.steps.split_last() else {
        // `append $`
        return append_to(&root).map_or_else(not_array, Ok);
    };
    let mut node = &root;
    for step in parents {
        let next = match (step, node) {
            (Step::Key(key), Node::Object { members, .. }) => members
                .iter()
                .find(|member| &member.key == key)
                .map(|member| &member.value),
            (Step::Index(index), Node::Array { items, .. }) => items.get(*index),
            _ => None,
        };
        let Some(next) = next else {
            return missing();
        };
        node = next;
    }
    match (last, node) {
        (Step::Key(key), Node::Object { end, members, .. }) => {
            let found = members.iter().position(|member| &member.key == key);
            match found {
                Some(index) => {
                    let member = &members[index];
                    let (start, end) = member.value.span();
                    if path.append {
                        return append_to(&member.value).map_or_else(not_array, Ok);
                    }
                    if value.is_none() && !path.strict {
                        // Delete the member and one adjacent comma.
                        let (from, to) = if let Some(comma) = member.comma {
                            (member.key_start, comma + 1)
                        } else if index > 0 {
                            (members[index - 1].comma.expect("separating comma"), end)
                        } else {
                            (member.key_start, end)
                        };
                        return Ok(splice(from, to, &[]));
                    }
                    Ok(splice(start, end, written))
                }
                None if path.strict => Err(error(13608, 2, 16, MISSING)),
                None if value.is_none() && !path.append => Ok(doc.to_vec()),
                None => {
                    let mut text = Vec::new();
                    if !members.is_empty() {
                        text.push(u16::from(b','));
                    }
                    text.extend(super::auto::quoted(&String::from_utf16_lossy(key)));
                    text.push(u16::from(b':'));
                    if path.append {
                        text.push(u16::from(b'['));
                        text.extend_from_slice(written);
                        text.push(u16::from(b']'));
                    } else {
                        text.extend_from_slice(written);
                    }
                    Ok(splice(end - 1, end - 1, &text))
                }
            }
        }
        (Step::Index(index), Node::Array { items, .. }) => match items.get(*index) {
            Some(item) => {
                if path.append {
                    return append_to(item).map_or_else(not_array, Ok);
                }
                let (start, end) = item.span();
                Ok(splice(start, end, written))
            }
            None => missing(),
        },
        _ => missing(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn units(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    fn run(doc: &str, path_text: &str, value: Option<&str>) -> Result<String, String> {
        let path = path(&units(path_text))?;
        let value = value.map(units);
        modify(&units(doc), &path, value.as_deref()).map(|out| String::from_utf16(&out).unwrap())
    }

    fn code(result: Result<String, String>) -> (i32, u8, String) {
        let error = result.unwrap_err();
        let mut fields = error
            .strip_prefix(super::super::MARKER)
            .unwrap()
            .splitn(4, ':');
        (
            fields.next().unwrap().parse().unwrap(),
            fields.next().unwrap().parse().unwrap(),
            {
                fields.next();
                fields.next().unwrap().to_owned()
            },
        )
    }

    #[test]
    fn captured_documents() {
        let ok = |doc, path, value| run(doc, path, value).unwrap();
        assert_eq!(ok(r#"{"a":1}"#, "$.a", Some("2")), r#"{"a":2}"#);
        assert_eq!(ok(r#"{"a":1}"#, "$.b", Some("2")), r#"{"a":1,"b":2}"#);
        assert_eq!(ok(r#"{"a":1}"#, "lax $.b", Some("2")), r#"{"a":1,"b":2}"#);
        assert_eq!(ok(r#"{"a":1}"#, "strict $.a", Some("2")), r#"{"a":2}"#);
        assert_eq!(ok(r#"{"a":1,"b":2}"#, "$.a", None), r#"{"b":2}"#);
        assert_eq!(ok(r#"{"a":1}"#, "$.z", None), r#"{"a":1}"#);
        assert_eq!(ok(r#"{"a":1}"#, "strict $.a", None), r#"{"a":null}"#);
        assert_eq!(
            ok(r#"{"arr":[1,2]}"#, "append $.arr", Some("3")),
            r#"{"arr":[1,2,3]}"#
        );
        assert_eq!(
            ok(r#"{"arr":[]}"#, "append $.arr", Some(r#""x""#)),
            r#"{"arr":["x"]}"#
        );
        assert_eq!(
            ok(r#"{"a":1}"#, "append $.arr", Some("3")),
            r#"{"a":1,"arr":[3]}"#
        );
        assert_eq!(
            ok(r#"{"a":1}"#, "append lax $.arr", Some("3")),
            r#"{"a":1,"arr":[3]}"#
        );
        assert_eq!(ok(r#"{"a":1}"#, "append $.a", Some("3")), r#"{"a":1}"#);
        assert_eq!(
            ok(r#"{"arr":[1]}"#, "append $.arr", None),
            r#"{"arr":[1,null]}"#
        );
        assert_eq!(
            ok(r#"{"arr":[1,2,3]}"#, "$.arr[1]", Some("9")),
            r#"{"arr":[1,9,3]}"#
        );
        assert_eq!(
            ok(r#"{"arr":[1]}"#, "$.arr[5]", Some("9")),
            r#"{"arr":[1]}"#
        );
        assert_eq!(
            ok(r#"{"arr":[1,2,3]}"#, "$.arr[1]", None),
            r#"{"arr":[1,null,3]}"#
        );
        assert_eq!(ok("[1,2]", "$[0]", Some(r#""x""#)), r#"["x",2]"#);
        assert_eq!(
            ok(r#"{"o":{"x":1}}"#, "$.o.x", Some("2")),
            r#"{"o":{"x":2}}"#
        );
        assert_eq!(ok(r#"{"a":1}"#, "$.o.x", Some("2")), r#"{"a":1}"#);
        assert_eq!(ok(r#"{"a b":1}"#, r#"$."a b""#, Some("2")), r#"{"a b":2}"#);
        assert_eq!(ok("{}", r#"$."a\"b""#, Some("2")), r#"{"a\"b":2}"#);
        assert_eq!(ok(r#"{"a":1,"a":2}"#, "$.a", Some("3")), r#"{"a":3,"a":2}"#);
        assert_eq!(ok(r#"{"a":1,"a":2}"#, "$.a", None), r#"{"a":2}"#);
        assert_eq!(
            ok(r#"{ "a" : 1 ,  "b" : [ 1 ] }"#, "$.a", Some("2")),
            r#"{ "a" : 2 ,  "b" : [ 1 ] }"#
        );
        assert_eq!(ok(r#"{"a":1}"#, "$.a.b", Some("2")), r#"{"a":1}"#);
        // Whitespace around inserted, deleted and appended members.
        assert_eq!(
            ok(r#"{ "a" : 1 }"#, "$.b", Some("2")),
            r#"{ "a" : 1 ,"b":2}"#
        );
        assert_eq!(ok("{ }", "$.b", Some("2")), r#"{ "b":2}"#);
        assert_eq!(ok(r#" {"a":1} "#, "$.b", Some("2")), r#" {"a":1,"b":2} "#);
        assert_eq!(
            ok(r#"{ "a" : 1 , "b" : 2 }"#, "$.a", None),
            r#"{  "b" : 2 }"#
        );
        assert_eq!(
            ok(r#"{ "a" : 1 , "b" : 2 }"#, "$.b", None),
            r#"{ "a" : 1  }"#
        );
        assert_eq!(ok(r#"{ "a" : 1 }"#, "$.a", None), "{  }");
        assert_eq!(
            ok(r#"{"a":1,"b":2,"c":3}"#, "$.b", None),
            r#"{"a":1,"c":3}"#
        );
        assert_eq!(
            ok(r#"{"arr":[ 1 , 2 ]}"#, "append $.arr", Some("3")),
            r#"{"arr":[ 1 , 2 ,3]}"#
        );
        assert_eq!(
            ok(r#"{"arr":[ ]}"#, "append $.arr", Some("3")),
            r#"{"arr":[ 3]}"#
        );
        assert_eq!(ok("[1]", "append $", Some("3")), "[1,3]");
        assert_eq!(
            ok(r#"{"o":{"x":1}}"#, "$.o.y", Some("2")),
            r#"{"o":{"x":1,"y":2}}"#
        );
        assert_eq!(
            ok(r#"{"a":1}"#, "append $.b", Some("2")),
            r#"{"a":1,"b":[2]}"#
        );
        assert_eq!(ok(r#"{"a":1}"#, "strict$.a", Some("2")), r#"{"a":2}"#);
        assert_eq!(ok(r#"{"a":1}"#, " $.a ", Some("2")), r#"{"a":2}"#);
        assert_eq!(ok("[[1]]", "$[0][0]", Some("2")), "[[2]]");
        assert_eq!(ok(r#"{"a":[1]}"#, "$.a[0].b", Some("2")), r#"{"a":[1]}"#);
        assert_eq!(
            ok(r#"{"a":{"b":"}"},"c":1}"#, "$.a", Some("2")),
            r#"{"a":2,"c":1}"#
        );
        assert_eq!(
            ok(r#"{"a":{"b":"\"}"},"c":1}"#, "$.a.b", Some("2")),
            r#"{"a":{"b":2},"c":1}"#
        );
    }

    #[test]
    fn captured_errors() {
        let strict_missing = (13608, 2, MISSING.to_owned());
        assert_eq!(
            code(run(r#"{"a":1}"#, "strict $.b", Some("2"))),
            strict_missing
        );
        assert_eq!(code(run(r#"{"a":1}"#, "strict $.z", None)), strict_missing);
        assert_eq!(
            code(run(r#"{"a":1}"#, "append strict $.arr", Some("3"))),
            strict_missing
        );
        assert_eq!(
            code(run(r#"{"arr":[1]}"#, "strict $.arr[5]", Some("9"))),
            strict_missing
        );
        assert_eq!(
            code(run(r#"{"a":1}"#, "strict $.o.x", Some("2"))),
            strict_missing
        );
        assert_eq!(
            code(run(r#"{"a":1}"#, "strict $.a.b", Some("2"))),
            strict_missing
        );
        assert_eq!(
            code(run(r#"{"a":1}"#, "append strict $.a", Some("3"))),
            (13621, 1, NOT_ARRAY.to_owned())
        );
        assert_eq!(
            code(run(r#"{"a":1}"#, "$", Some("1"))),
            (13619, 1, ROOT.to_owned())
        );
        let document = |doc: &str, character: char, at: usize| {
            assert_eq!(
                code(run(doc, "$.a", Some("1"))),
                (
                    13609,
                    7,
                    format!(
                        "{DOCUMENT_FORMAT} Unexpected character '{character}' is found at position {at}."
                    )
                )
            )
        };
        document("not json", 'n', 0);
        document("", '.', 0);
        document("1", '1', 0);
        document(r#"{"a":1"#, '.', 6);
        let path_error = |path_text: &str, state: u8, character: char, at: usize| {
            assert_eq!(
                code(run(r#"{"a":[1]}"#, path_text, Some("2"))),
                (
                    13607,
                    state,
                    format!(
                        "{PATH_FORMAT} Unexpected character '{character}' is found at position {at}."
                    )
                )
            )
        };
        path_error("a", 22, 'a', 0);
        path_error("", 14, '.', 0);
        path_error("$.", 14, '.', 2);
        path_error("loose $.a", 22, 'l', 0);
        path_error("$.a[x]", 21, 'x', 4);
        path_error("$.a[-1]", 21, '-', 4);
        assert_eq!(
            code(run(r#"{"a":1}"#, "$.*", Some("2"))),
            (13660, 4, ADVANCED.to_owned())
        );
    }
}
