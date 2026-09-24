//! Source-preserving SQL JSON path evaluation, independent of database and AST adapters.
use crate::{
    diagnostic::SqlError,
    json::{Kind, prefix, root},
};

pub const DOCUMENT: &str = "JSON text is not properly formatted.";
pub const PATH: &str = "JSON path is not properly formatted.";
pub const MISSING: &str = "Property cannot be found on the specified JSON path.";
pub const SCALAR: &str = "Scalar value cannot be found in the specified JSON path.";
pub const CONTAINER: &str = "Object or array cannot be found in the specified JSON path.";
pub const WIDTH: &str = "String value in the specified JSON path would be truncated.";

pub const NULL_VALUE_LITERAL: &str =
    "Argument data type NULL is invalid for argument 2 of json_value function.";
pub const NULL_PATHS: [&str; 3] = [
    "Argument data type NULL is invalid for argument 2 of JSON_VALUE function.",
    "Argument data type NULL is invalid for argument 2 of JSON_QUERY function.",
    "Argument data type NULL is invalid for argument 2 of JSON_PATH_EXISTS function.",
];

/// Recognize an exact canonical JSON extraction message.
/// Backend wrappers must be removed by the caller, never by core rules.
pub fn diagnostic(message: &str) -> Option<SqlError> {
    [
        (DOCUMENT, 13609, 1),
        (PATH, 13607, 1),
        (MISSING, 13608, 1),
        (SCALAR, 13623, 2),
        (CONTAINER, 13624, 2),
        (WIDTH, 13625, 1),
        (NULL_VALUE_LITERAL, 8116, 1),
    ]
    .into_iter()
    .chain(NULL_PATHS.into_iter().map(|text| (text, 8116, 8)))
    .find_map(|(text, number, state)| (message == text).then(|| SqlError::new(number, state, text)))
}
/// Trim only JSON whitespace.
pub fn trim(s: &str) -> &str {
    s.trim_matches([' ', '\t', '\r', '\n'])
}
/// Decode a JSON string; isolated UTF-16 surrogates remain unsupported.
pub fn decode(s: &str) -> Result<String, &'static str> {
    serde_json::from_str::<String>(s)
        .map_err(|_| "JSON string with isolated surrogate code units is not yet supported")
}
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    Key(String),
    Index(usize),
    /// Array wildcard, accepted only by the existence predicate.
    All,
}
/// Parse optional lax/strict mode and property/index steps.
pub fn path(text: &str) -> Result<(bool, Vec<Step>), &'static str> {
    parse_path(text, false)
}
fn parse_path(text: &str, wildcard: bool) -> Result<(bool, Vec<Step>), &'static str> {
    let text = trim(text);
    let (strict, mut rest) = if let Some((mode, tail)) = text.split_once(char::is_whitespace) {
        if mode.eq_ignore_ascii_case("strict") {
            (true, trim(tail))
        } else if mode.eq_ignore_ascii_case("lax") {
            (false, trim(tail))
        } else {
            (false, text)
        }
    } else {
        (false, text)
    };
    rest = rest.strip_prefix('$').ok_or(PATH)?;
    let mut steps = Vec::new();
    while !rest.is_empty() {
        if let Some(tail) = rest.strip_prefix('.') {
            if tail.starts_with('"') {
                let (kind, end) = prefix(tail.as_bytes()).ok_or(PATH)?;
                if kind != Kind::String {
                    return Err(PATH);
                }
                steps.push(Step::Key(decode(&tail[..end]).map_err(|_| PATH)?));
                rest = &tail[end..];
            } else {
                let end = tail.find(['.', '[']).unwrap_or(tail.len());
                let key = &tail[..end];
                if key.is_empty()
                    || !key.chars().enumerate().all(|(i, c)| {
                        c == '_' || c == '$' || c.is_alphabetic() || (i > 0 && c.is_ascii_digit())
                    })
                {
                    return Err(PATH);
                }
                steps.push(Step::Key(key.into()));
                rest = &tail[end..];
            }
        } else if let Some(tail) = rest.strip_prefix('[') {
            let end = tail.find(']').ok_or(PATH)?;
            let index = &tail[..end];
            if wildcard && index == "*" {
                steps.push(Step::All);
            } else {
                if index.is_empty() || !index.bytes().all(|c| c.is_ascii_digit()) {
                    return Err(PATH);
                }
                steps.push(Step::Index(index.parse().map_err(|_| PATH)?));
            }
            rest = &tail[end + 1..];
        } else {
            return Err(PATH);
        }
    }
    Ok((strict, steps))
}
/// Locate a matching source suffix, without consuming the matched value.
///
/// The caller must validate the selected value and, when no match exists, the
/// entire document. This permits extraction before unrelated malformed suffixes.
pub fn selected<'a>(source: &'a str, steps: &[Step]) -> Result<Option<&'a str>, &'static str> {
    let mut current = trim(source);
    for step in steps {
        let mut found = None;
        match step {
            Step::All => return Err(PATH),
            Step::Key(wanted) if current.starts_with('{') => {
                let mut rest = trim(&current[1..]);
                while !rest.starts_with('}') {
                    let (kind, end) = prefix(rest.as_bytes()).ok_or(DOCUMENT)?;
                    if kind != Kind::String {
                        return Err(DOCUMENT);
                    }
                    let key = decode(&rest[..end])?;
                    rest = trim(&rest[end..]);
                    rest = trim(rest.strip_prefix(':').ok_or(DOCUMENT)?);
                    if key == *wanted {
                        found = Some(rest);
                        break;
                    }
                    let (_, end) = prefix(rest.as_bytes()).ok_or(DOCUMENT)?;
                    rest = trim(&rest[end..]);
                    if let Some(tail) = rest.strip_prefix(',') {
                        rest = trim(tail);
                    } else {
                        break;
                    }
                }
            }
            Step::Index(wanted) if current.starts_with('[') => {
                let mut rest = trim(&current[1..]);
                let mut index = 0;
                while !rest.starts_with(']') {
                    if index == *wanted {
                        found = Some(rest);
                        break;
                    }
                    let (_, end) = prefix(rest.as_bytes()).ok_or(DOCUMENT)?;
                    index += 1;
                    rest = trim(&rest[end..]);
                    if let Some(tail) = rest.strip_prefix(',') {
                        rest = trim(tail);
                    } else {
                        break;
                    }
                }
            }
            _ => {}
        }
        let Some(value) = found else {
            return Ok(None);
        };
        current = value;
    }
    Ok(Some(current))
}
/// Test whether a property/index/wildcard path selects any JSON value.
/// JSON null and empty containers count as present. Invalid input/path returns
/// false; SQL NULL propagation is the adapter's responsibility.
/// Uses an explicit work stack, preserving lexical values and avoiding recursion.
pub fn exists(source: &str, path_text: &str) -> bool {
    exists_inner(source, path_text).unwrap_or(false)
}
fn exists_inner(source: &str, path_text: &str) -> Option<bool> {
    root(source.as_bytes())?;
    let (_, steps) = parse_path(path_text, true).ok()?;
    let mut pending = vec![(trim(source), 0)];
    while let Some((value, at)) = pending.pop() {
        let Some(step) = steps.get(at) else {
            return Some(true);
        };
        if matches!(step, Step::All) {
            if !value.starts_with('[') {
                continue;
            }
            let (_, end) = prefix(value.as_bytes())?;
            let mut rest = trim(&value[1..end - 1]);
            while !rest.is_empty() {
                let (_, end) = prefix(rest.as_bytes())?;
                pending.push((&rest[..end], at + 1));
                rest = trim(&rest[end..]);
                if let Some(tail) = rest.strip_prefix(',') {
                    rest = trim(tail);
                }
            }
        } else if let Some(tail) = selected(value, std::slice::from_ref(step)).ok()? {
            let (_, end) = prefix(tail.as_bytes())?;
            pending.push((&tail[..end], at + 1));
        }
    }
    Some(false)
}
/// Extract JSON_VALUE (`query = false`) or JSON_QUERY (`query = true`).
/// Preserves lexical numbers and container text; enforces the scalar UTF-16 limit.
pub fn extract(source: &str, path_text: &str, query: bool) -> Result<Option<String>, &'static str> {
    let (strict, steps) = path(path_text)?;
    // Root extraction consumes the document. A missing path must also validate
    // all remaining text; successful descent only validates the required prefix.
    if steps.is_empty() {
        root(source.as_bytes()).ok_or(DOCUMENT)?;
    }
    let Some(tail) = selected(source, &steps)? else {
        root(source.as_bytes()).ok_or(DOCUMENT)?;
        return if strict { Err(MISSING) } else { Ok(None) };
    };
    let (kind, end) = prefix(tail.as_bytes()).ok_or(DOCUMENT)?;
    if tail
        .as_bytes()
        .get(end)
        .is_some_and(|c| !matches!(c, b' ' | b'\t' | b'\r' | b'\n' | b',' | b'}' | b']'))
    {
        return Err(DOCUMENT);
    }
    let value = &tail[..end];
    let container = matches!(kind, Kind::Array | Kind::Object);
    if query {
        return if container {
            Ok(Some(value.into()))
        } else if strict {
            Err(CONTAINER)
        } else {
            Ok(None)
        };
    }
    if container {
        return if strict { Err(SCALAR) } else { Ok(None) };
    }
    if value == "null" {
        return Ok(None);
    }
    let value = if kind == Kind::String {
        decode(value)?
    } else {
        value.into()
    };
    if value.encode_utf16().count() > 4000 {
        return if strict { Err(WIDTH) } else { Ok(None) };
    }
    Ok(Some(value))
}

/// Decode a quoted JSON string without requiring paired UTF-16 surrogates.
pub fn decode_utf16(text: &[u16]) -> Result<Vec<u16>, &'static str> {
    let syntax = json_syntax(text);
    if text.first() != Some(&34)
        || text.last() != Some(&34)
        || prefix(&syntax) != Some((Kind::String, text.len()))
    {
        return Err(DOCUMENT);
    }
    let mut out = Vec::with_capacity(text.len().saturating_sub(2));
    let mut at = 1;
    while at < text.len() - 1 {
        let unit = text[at];
        at += 1;
        if unit != 92 {
            out.push(unit);
            continue;
        }
        let escape = text[at];
        at += 1;
        out.push(match escape {
            34 | 47 | 92 => escape,
            98 => 8,
            102 => 12,
            110 => 10,
            114 => 13,
            116 => 9,
            117 => {
                let mut value = 0;
                for &digit in &text[at..at + 4] {
                    value = value * 16
                        + match digit {
                            48..=57 => digit - 48,
                            65..=70 => digit - 65 + 10,
                            97..=102 => digit - 97 + 10,
                            _ => return Err(DOCUMENT),
                        };
                }
                at += 4;
                value
            }
            _ => return Err(DOCUMENT),
        });
    }
    Ok(out)
}

// One byte per code unit gives the existing ASCII grammar scanner exact UTF-16
// offsets. Non-ASCII content is never reconstructed from this syntax projection.
fn json_syntax(text: &[u16]) -> Vec<u8> {
    text.iter()
        .map(|&u| if u <= 127 { u as u8 } else { 128 })
        .collect()
}
fn skip_ws(syntax: &[u8], mut at: usize, end: usize) -> usize {
    while at < end && matches!(syntax[at], b' ' | b'\t' | b'\r' | b'\n') {
        at += 1;
    }
    at
}
#[derive(Debug)]
enum Utf16Step {
    Key(Vec<u16>),
    Index(usize),
    All,
}
fn path_utf16(text: &[u16], wildcard: bool) -> Result<(bool, Vec<Utf16Step>), &'static str> {
    let syntax = json_syntax(text);
    let mut at = skip_ws(&syntax, 0, syntax.len());
    let mut end = syntax.len();
    while end > at && matches!(syntax[end - 1], b' ' | b'\t' | b'\r' | b'\n') {
        end -= 1;
    }
    let mut strict = false;
    for (mode, value) in [(b"strict".as_slice(), true), (b"lax".as_slice(), false)] {
        if syntax
            .get(at..at + mode.len())
            .is_some_and(|s| s.eq_ignore_ascii_case(mode))
            && syntax
                .get(at + mode.len())
                .is_some_and(u8::is_ascii_whitespace)
        {
            strict = value;
            at = skip_ws(&syntax, at + mode.len(), end);
            break;
        }
    }
    if syntax.get(at) != Some(&b'$') {
        return Err(PATH);
    }
    at += 1;
    let mut steps = Vec::new();
    while at < end {
        match syntax[at] {
            b'.' => {
                at += 1;
                if syntax.get(at) == Some(&b'"') {
                    let (kind, len) = prefix(&syntax[at..end]).ok_or(PATH)?;
                    if kind != Kind::String {
                        return Err(PATH);
                    }
                    steps.push(Utf16Step::Key(
                        decode_utf16(&text[at..at + len]).map_err(|_| PATH)?,
                    ));
                    at += len;
                } else {
                    let start = at;
                    while at < end && !matches!(syntax[at], b'.' | b'[') {
                        at += 1;
                    }
                    let key = String::from_utf16(&text[start..at]).map_err(|_| PATH)?;
                    if key.is_empty()
                        || !key.chars().enumerate().all(|(i, c)| {
                            c == '_'
                                || c == '$'
                                || c.is_alphabetic()
                                || (i > 0 && c.is_ascii_digit())
                        })
                    {
                        return Err(PATH);
                    }
                    steps.push(Utf16Step::Key(text[start..at].to_vec()));
                }
            }
            b'[' => {
                at += 1;
                let start = at;
                while at < end && syntax[at] != b']' {
                    at += 1;
                }
                if at == end {
                    return Err(PATH);
                }
                if wildcard && &syntax[start..at] == b"*" {
                    steps.push(Utf16Step::All);
                } else {
                    if start == at {
                        return Err(PATH);
                    }
                    let mut index = 0usize;
                    for &digit in &syntax[start..at] {
                        if !digit.is_ascii_digit() {
                            return Err(PATH);
                        }
                        index = index
                            .checked_mul(10)
                            .and_then(|n| n.checked_add(usize::from(digit - b'0')))
                            .ok_or(PATH)?;
                    }
                    steps.push(Utf16Step::Index(index));
                }
                at += 1;
            }
            _ => return Err(PATH),
        }
    }
    Ok((strict, steps))
}
struct Utf16Document<'a> {
    units: &'a [u16],
    syntax: Vec<u8>,
}
impl<'a> Utf16Document<'a> {
    fn new(units: &'a [u16]) -> Self {
        Self {
            units,
            syntax: json_syntax(units),
        }
    }
    fn prefix(&self, at: usize, end: usize) -> Result<(Kind, usize), &'static str> {
        prefix(&self.syntax[at..end])
            .map(|(kind, len)| (kind, at + len))
            .ok_or(DOCUMENT)
    }
    fn selected(
        &self,
        start: usize,
        end: usize,
        steps: &[Utf16Step],
    ) -> Result<Option<usize>, &'static str> {
        let mut current = skip_ws(&self.syntax, start, end);
        for step in steps {
            let mut found = None;
            match step {
                Utf16Step::All => return Err(PATH),
                Utf16Step::Key(wanted) if self.syntax.get(current) == Some(&b'{') => {
                    let mut at = skip_ws(&self.syntax, current + 1, end);
                    while self.syntax.get(at) != Some(&b'}') {
                        let (kind, next) = self.prefix(at, end)?;
                        if kind != Kind::String {
                            return Err(DOCUMENT);
                        }
                        let key = decode_utf16(&self.units[at..next])?;
                        at = skip_ws(&self.syntax, next, end);
                        if self.syntax.get(at) != Some(&b':') {
                            return Err(DOCUMENT);
                        }
                        at = skip_ws(&self.syntax, at + 1, end);
                        if &key == wanted {
                            found = Some(at);
                            break;
                        }
                        let (_, next) = self.prefix(at, end)?;
                        at = skip_ws(&self.syntax, next, end);
                        if self.syntax.get(at) != Some(&b',') {
                            break;
                        }
                        at = skip_ws(&self.syntax, at + 1, end);
                    }
                }
                Utf16Step::Index(wanted) if self.syntax.get(current) == Some(&b'[') => {
                    let mut at = skip_ws(&self.syntax, current + 1, end);
                    let mut index = 0;
                    while self.syntax.get(at) != Some(&b']') {
                        if index == *wanted {
                            found = Some(at);
                            break;
                        }
                        let (_, next) = self.prefix(at, end)?;
                        index += 1;
                        at = skip_ws(&self.syntax, next, end);
                        if self.syntax.get(at) != Some(&b',') {
                            break;
                        }
                        at = skip_ws(&self.syntax, at + 1, end);
                    }
                }
                _ => {}
            }
            let Some(at) = found else {
                return Ok(None);
            };
            current = at;
        }
        Ok(Some(current))
    }
}
/// Extract exact UTF-16 scalar units or unchanged container source text.
pub fn extract_utf16(
    source: &[u16],
    path: &[u16],
    query: bool,
) -> Result<Option<Vec<u16>>, &'static str> {
    let (strict, steps) = path_utf16(path, false)?;
    let document = Utf16Document::new(source);
    if steps.is_empty() {
        root(&document.syntax).ok_or(DOCUMENT)?;
    }
    let Some(at) = document.selected(0, source.len(), &steps)? else {
        root(&document.syntax).ok_or(DOCUMENT)?;
        return if strict { Err(MISSING) } else { Ok(None) };
    };
    let (kind, end) = document.prefix(at, source.len())?;
    if document
        .syntax
        .get(end)
        .is_some_and(|c| !matches!(c, b' ' | b'\t' | b'\r' | b'\n' | b',' | b'}' | b']'))
    {
        return Err(DOCUMENT);
    }
    let value = &source[at..end];
    let container = matches!(kind, Kind::Array | Kind::Object);
    if query {
        return if container {
            Ok(Some(value.to_vec()))
        } else if strict {
            Err(CONTAINER)
        } else {
            Ok(None)
        };
    }
    if container {
        return if strict { Err(SCALAR) } else { Ok(None) };
    }
    if &document.syntax[at..end] == b"null" {
        return Ok(None);
    }
    let value = if kind == Kind::String {
        decode_utf16(value)?
    } else {
        value.to_vec()
    };
    if value.len() > 4000 {
        return if strict { Err(WIDTH) } else { Ok(None) };
    }
    Ok(Some(value))
}
/// Existence validates the full document and preserves UTF-16 key identity.
pub fn exists_utf16(source: &[u16], path: &[u16]) -> bool {
    fn evaluate(source: &[u16], path: &[u16]) -> Option<bool> {
        let document = Utf16Document::new(source);
        root(&document.syntax)?;
        let (_, steps) = path_utf16(path, true).ok()?;
        let mut pending = vec![(skip_ws(&document.syntax, 0, source.len()), source.len(), 0)];
        while let Some((at, end, step)) = pending.pop() {
            let Some(next) = steps.get(step) else {
                return Some(true);
            };
            if matches!(next, Utf16Step::All) {
                if document.syntax.get(at) != Some(&b'[') {
                    continue;
                }
                let (_, end) = document.prefix(at, end).ok()?;
                let mut child = skip_ws(&document.syntax, at + 1, end - 1);
                while child < end - 1 {
                    let (_, next) = document.prefix(child, end - 1).ok()?;
                    pending.push((child, next, step + 1));
                    child = skip_ws(&document.syntax, next, end - 1);
                    if document.syntax.get(child) == Some(&b',') {
                        child = skip_ws(&document.syntax, child + 1, end - 1);
                    }
                }
            } else if let Some(selected) = document
                .selected(at, end, std::slice::from_ref(next))
                .ok()?
            {
                let (_, end) = document.prefix(selected, end).ok()?;
                pending.push((selected, end, step + 1));
            }
        }
        Some(false)
    }
    evaluate(source, path).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn existence_distinguishes_json_null_missing_and_scalar_limits() {
        let document =
            r#"{"null":null,"empty":[],"object":{},"text":"","a.b":{"雪":1},"A":1,"n":1e99999}"#;
        for path in [
            "$",
            "$.null",
            "$.empty",
            "$.object",
            "$.text",
            "$.A",
            "$.n",
            "$.\"a.b\".雪",
        ] {
            assert!(exists(document, path), "{path}");
        }
        for path in [
            "$.a",
            "strict $.absent",
            "$.empty[0]",
            "$.null.x",
            "$.text[0]",
        ] {
            assert!(!exists(document, path), "{path}");
        }
        assert!(exists(
            &format!("{{\"s\":\"{}\"}}", "🦆".repeat(5000)),
            "$.s"
        ));
        assert!(exists(r#"{"n":null,"n":1}"#, "$.n"));
    }
    #[test]
    fn existence_wildcards_match_any_branch_without_changing_extraction_paths() {
        for (document, wanted) in [
            (
                r#"{"info":{"address":[{"town":"Paris"},{"town":"London"}]}}"#,
                true,
            ),
            (
                r#"{"info":{"address":[{"town":"Paris"},{"city":"London"}]}}"#,
                true,
            ),
            (
                r#"{"info":{"address":[{"city":"Paris"},{"city":"London"}]}}"#,
                false,
            ),
        ] {
            assert_eq!(exists(document, "$.info.address[*].town"), wanted);
        }
        assert!(exists(r#"[[{}, {"x":null}], []]"#, "$[*][*].x"));
        assert!(!exists("[]", "$[*]"));
        assert!(exists("[null]", "$[*]"));
        assert!(!exists("{}", "$[*]"));
        assert_eq!(path("$[*]"), Err(PATH));
        let depth = 3000;
        let source = "[".repeat(depth) + "null" + &"]".repeat(depth);
        assert!(exists(&source, &("$".to_owned() + &"[0]".repeat(depth))));
    }
    #[test]
    fn existence_malformed_inputs_return_false() {
        for source in ["", "[1,]", "{bad}", "{}[]", r#"{"a":1,"tail":invalid}"#] {
            assert!(!exists(source, "$.a"));
        }
        for path in ["", "nope", "$.", "$[*", "$[x]", "$.two words"] {
            assert!(!exists(r#"{"a":1}"#, path));
        }
        assert!(!exists("[1,]", "strict $"));
    }
    #[test]
    fn source_slices_duplicates_and_utf16_limits() {
        let source =
            r#"{"n":1.20e+2,"n":3,"s":"a\u0062","a": [ 1, { "x": 2 } ],"b":true,"z":null}"#;
        for (path, want) in [
            ("$.n", Some("1.20e+2")),
            ("$.s", Some("ab")),
            ("$.b", Some("true")),
            ("$.z", None),
            ("$.missing", None),
        ] {
            assert_eq!(extract(source, path, false).unwrap().as_deref(), want);
        }
        assert_eq!(
            extract(source, "$.a[1]", true).unwrap().as_deref(),
            Some("{ \"x\": 2 }")
        );
        assert_eq!(extract(source, "strict $.missing", false), Err(MISSING));
        assert_eq!(extract(source, "strict $.a", false), Err(SCALAR));
        assert_eq!(extract(source, "strict $.b", true), Err(CONTAINER));
        for text in ["[1,]", "{bad}", "{}[]"] {
            assert_eq!(extract(text, "$", true), Err(DOCUMENT));
        }
        for path in ["nope", "$.", "$[x]", "$.*", "$.two words"] {
            assert_eq!(extract(source, path, false), Err(PATH));
        }
        let long = format!("{{\"s\":\"{}\"}}", "🦆".repeat(2000));
        assert_eq!(
            extract(&long, "$.s", false)
                .unwrap()
                .unwrap()
                .encode_utf16()
                .count(),
            4000
        );
        let long = format!("{{\"s\":\"{}x\"}}", "🦆".repeat(2000));
        assert_eq!(extract(&long, "$.s", false).unwrap(), None);
        assert_eq!(extract(&long, "strict $.s", false), Err(WIDTH));
    }
    #[test]
    fn early_matches_and_required_validation() {
        let text = r#"{"a":[{"x":1.20e+2,"tail":invalid}],"rest":invalid}"#;
        assert_eq!(
            extract(text, "$.a[0].x", false).unwrap().as_deref(),
            Some("1.20e+2")
        );
        assert_eq!(extract(text, "$", true), Err(DOCUMENT));
        for (text, path, query) in [
            ("{\"a\":1,\"tail\":invalid}", "$.missing", false),
            ("{\"prior\":invalid,\"a\":1}", "$.a", false),
            ("{\"a\":{\"x\":invalid}}", "$.a", true),
        ] {
            assert_eq!(extract(text, path, query), Err(DOCUMENT));
        }
    }
    #[test]
    fn utf16_extraction_matches_reference_units_and_lexical_containers() {
        let u = |s: &str| s.encode_utf16().collect::<Vec<_>>();
        for raw in [
            vec![0xd83e],
            vec![0xdd86],
            vec![0xdd86, 0xd83e],
            vec![0xd83e, 0xdd86],
        ] {
            let mut source = u(r#"{"s":""#);
            source.extend(&raw);
            source.extend(u(r#"","a":[""#));
            source.extend(&raw);
            source.extend(u(r#""]}"#));
            assert_eq!(
                extract_utf16(&source, &u("$.s"), false),
                Ok(Some(raw.clone()))
            );
            let mut array = u(r#"[""#);
            array.extend(&raw);
            array.extend(u(r#""]"#));
            assert_eq!(extract_utf16(&source, &u("$.a"), true), Ok(Some(array)));
            assert!(exists_utf16(&source, &u("$.a[*]")));
            let mut keyed = u(r#"{""#);
            keyed.extend(&raw);
            keyed.extend(u(r#"":7}"#));
            let mut path = u(r#"$.""#);
            path.extend(&raw);
            path.push(34);
            assert_eq!(extract_utf16(&keyed, &path, false), Ok(Some(u("7"))));
            assert!(exists_utf16(&keyed, &path));
        }
        for (escape, units) in [
            (r"\ud800", vec![0xd800]),
            (r"\udc00\ud800", vec![0xdc00, 0xd800]),
            (r"\ud83e\udd86", vec![0xd83e, 0xdd86]),
        ] {
            let source = u(&format!(r#"{{"s":"{escape}","a":["{escape}"]}}"#));
            assert_eq!(extract_utf16(&source, &u("$.s"), false), Ok(Some(units)));
            assert_eq!(
                extract_utf16(&source, &u("$.a"), true),
                Ok(Some(u(&format!(r#"["{escape}"]"#))))
            );
        }
        let source = u(r#"{"s":"ok","a":[1],"bad":invalid}"#);
        assert_eq!(extract_utf16(&source, &u("$.s"), false), Ok(Some(u("ok"))));
        assert_eq!(extract_utf16(&source, &u("$.a"), true), Ok(Some(u("[1]"))));
        assert_eq!(
            extract_utf16(&source, &u("$.missing"), false),
            Err(DOCUMENT)
        );
        assert!(!exists_utf16(&source, &u("$.s")));
    }
    #[test]
    fn utf16_scalar_width_and_legacy_paths_remain_exact() {
        let u = |s: &str| s.encode_utf16().collect::<Vec<_>>();
        for size in [4000, 4001] {
            let mut source = u(r#"{"s":""#);
            source.extend(vec![0xd800; size]);
            source.extend(u(r#""}"#));
            assert_eq!(
                extract_utf16(&source, &u("$.s"), false),
                Ok((size == 4000).then(|| vec![0xd800; size]))
            );
            if size == 4001 {
                assert_eq!(extract_utf16(&source, &u("strict $.s"), false), Err(WIDTH));
            }
        }
        for source in [
            r#"{"雪":{"a":[null,1.20e+2,"🦆"]},"x":"a\n"}"#,
            r#"[{"x":1},{}]"#,
            r#"{"x":1,"x":2}"#,
            r#"{"x":1,"bad":invalid}"#,
            "{}",
            "null",
            "",
            r#"{"x":[1,]}"#,
        ] {
            for path in [
                "$",
                "$.x",
                "strict $.missing",
                "$.雪.a[2]",
                "$.雪.a[1]",
                "$[*].x",
                "strict $[2]",
                "$.[",
                r#"$."雪".a[0]"#,
            ] {
                for query in [false, true] {
                    assert_eq!(
                        extract_utf16(&u(source), &u(path), query),
                        extract(source, path, query).map(|v| v.map(|s| u(&s))),
                        "{source} {path} {query}"
                    );
                }
                assert_eq!(
                    exists_utf16(&u(source), &u(path)),
                    exists(source, path),
                    "{source} {path}"
                );
            }
        }
    }
}
