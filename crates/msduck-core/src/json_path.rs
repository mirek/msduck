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
    ]
    .into_iter()
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
}
