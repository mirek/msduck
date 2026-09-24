//! Deterministic OPENJSON row and explicit-schema rules.
use crate::{
    diagnostic::SqlError,
    json::{Kind, prefix, root},
    json_path::{self as json, decode, path, selected, trim},
};
pub mod binary;
pub mod schema;
const DOCUMENT: &str = "OPENJSON: invalid document";
const PATH: &str = "OPENJSON: invalid path";
const MISSING: &str = "OPENJSON: missing path";
pub fn diagnostic(message: &str) -> Option<SqlError> {
    match message {
        DOCUMENT => Some(SqlError::new(13609, 4, json::DOCUMENT)),
        PATH => Some(SqlError::new(13607, 22, json::PATH)),
        MISSING => Some(SqlError::new(13608, 3, json::MISSING)),
        _ => schema::diagnostic(message).or_else(|| binary::diagnostic(message)),
    }
}
/// Default OPENJSON row; `kind` is the SQL type code (0 through 5).
#[derive(Debug, PartialEq)]
pub struct Row {
    pub key: String,
    pub value: Option<String>,
    pub kind: i32,
}
/// Expand an object or array, preserving duplicate keys and lexical values.
/// Validates the entire input document before selecting a path.
pub fn rows(source: &str, path_text: &str) -> Result<Vec<Row>, &'static str> {
    if !matches!(root(source.as_bytes()), Some(Kind::Object | Kind::Array)) {
        return Err(DOCUMENT);
    }
    let (strict, steps) = path(path_text).map_err(|_| PATH)?;
    let Some(tail) = selected(source, &steps).map_err(|error| {
        if error == json::DOCUMENT {
            DOCUMENT
        } else {
            error
        }
    })?
    else {
        return if strict { Err(MISSING) } else { Ok(vec![]) };
    };
    let (kind, end) = prefix(tail.as_bytes()).ok_or(DOCUMENT)?;
    if !matches!(kind, Kind::Object | Kind::Array) {
        return Ok(vec![]);
    }
    let mut rest = trim(&tail[1..end - 1]);
    let mut output = Vec::new();
    while !rest.is_empty() {
        let key = if kind == Kind::Object {
            let (_, end) = prefix(rest.as_bytes()).ok_or(DOCUMENT)?;
            let key = decode(&rest[..end])?;
            rest = trim(&rest[end..]);
            rest = trim(rest.strip_prefix(':').ok_or(DOCUMENT)?);
            key
        } else {
            output.len().to_string()
        };
        let (kind, end) = prefix(rest.as_bytes()).ok_or(DOCUMENT)?;
        let text = &rest[..end];
        let (value, code) = match kind {
            Kind::String => (Some(decode(text)?), 1),
            Kind::Number => (Some(text.to_owned()), 2),
            Kind::Literal if text == "null" => (None, 0),
            Kind::Literal => (Some(text.to_owned()), 3),
            Kind::Array => (Some(text.to_owned()), 4),
            Kind::Object => (Some(text.to_owned()), 5),
        };
        output.push(Row {
            key,
            value,
            kind: code,
        });
        rest = trim(&rest[end..]);
        if let Some(tail) = rest.strip_prefix(',') {
            rest = trim(tail);
        }
    }
    Ok(output)
}

/// Default rows with exact UTF-16 keys and values, including unpaired units.
#[derive(Debug, PartialEq, Eq)]
pub struct Utf16Row {
    pub key: Vec<u16>,
    pub value: Option<Vec<u16>>,
    pub kind: i32,
}

/// Validate the complete document before expanding the selected object or array.
pub fn rows_utf16(source: &[u16], path: &[u16]) -> Result<Vec<Utf16Row>, &'static str> {
    rows_utf16_with_limit(source, path, usize::MAX)
}

pub const OUTPUT_LIMIT: &str = "OPENJSON result exceeds the configured output limit";

/// Bound retained row structures and UTF-16 payloads while expanding the document.
pub fn rows_utf16_with_limit(
    source: &[u16],
    path: &[u16],
    mut remaining: usize,
) -> Result<Vec<Utf16Row>, &'static str> {
    let Some(value) = schema::resolve_utf16(source, path, true)? else {
        return Ok(vec![]);
    };
    let syntax = json::json_syntax(value);
    let kind = root(&syntax).ok_or(DOCUMENT)?;
    if !matches!(kind, Kind::Object | Kind::Array) {
        return Ok(vec![]);
    }
    let document = json::Utf16Document::new(value);
    let end = value.len() - 1;
    let mut at = json::skip_ws(&syntax, 1, end);
    let mut rows = Vec::new();
    while at < end {
        let key = if kind == Kind::Object {
            let (_, next) = document.prefix(at, end).map_err(|_| DOCUMENT)?;
            let key = json::decode_utf16(&value[at..next]).map_err(|_| DOCUMENT)?;
            at = json::skip_ws(&syntax, next, end);
            if syntax.get(at) != Some(&b':') {
                return Err(DOCUMENT);
            }
            at = json::skip_ws(&syntax, at + 1, end);
            key
        } else {
            rows.len().to_string().encode_utf16().collect()
        };
        let (kind, next) = document.prefix(at, end).map_err(|_| DOCUMENT)?;
        let text = &value[at..next];
        let (value, kind) = match kind {
            Kind::String => (Some(json::decode_utf16(text).map_err(|_| DOCUMENT)?), 1),
            Kind::Number => (Some(text.to_vec()), 2),
            Kind::Literal if syntax[at..next] == *b"null" => (None, 0),
            Kind::Literal => (Some(text.to_vec()), 3),
            Kind::Array => (Some(text.to_vec()), 4),
            Kind::Object => (Some(text.to_vec()), 5),
        };
        let size = key
            .len()
            .checked_add(value.as_ref().map_or(0, Vec::len))
            .and_then(|n| n.checked_mul(2))
            .and_then(|n| n.checked_add(std::mem::size_of::<Utf16Row>()))
            .ok_or(OUTPUT_LIMIT)?;
        remaining = remaining.checked_sub(size).ok_or(OUTPUT_LIMIT)?;
        rows.push(Utf16Row { key, value, kind });
        at = json::skip_ws(&syntax, next, end);
        if syntax.get(at) == Some(&b',') {
            at = json::skip_ws(&syntax, at + 1, end);
        }
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn u(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }
    #[test]
    fn utf16_expansion_enforces_retained_output_budget() {
        let size = std::mem::size_of::<Utf16Row>() + 2;
        assert_eq!(
            rows_utf16_with_limit(&u("[null]"), &u("$"), size)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            rows_utf16_with_limit(&u("[null]"), &u("$"), size - 1),
            Err(OUTPUT_LIMIT)
        );
        assert_eq!(
            rows_utf16_with_limit(&u("[null,null]"), &u("$"), size),
            Err(OUTPUT_LIMIT)
        );
        let size = std::mem::size_of::<Vec<u16>>() + 4;
        assert!(schema::sources_utf16_with_limit(&u("{}"), &u("$"), size).is_ok());
        assert_eq!(
            schema::sources_utf16_with_limit(&u("{}"), &u("$"), size - 1),
            Err(OUTPUT_LIMIT)
        );
        assert_eq!(
            schema::sources_utf16_with_limit(&u("[{},{}]"), &u("$"), size),
            Err(OUTPUT_LIMIT)
        );
    }
    #[test]
    fn utf16_rows_preserve_duplicates_units_and_lexical_values() {
        let mut source = u("{\"");
        source.extend([0xd800]);
        source.extend(u("\":\""));
        source.extend([0xdc00, 0xd800]);
        source.extend(u(
            "\",\"n\":1.20e+2,\"n\":3,\"z\":null,\"b\":true,\"a\":[ \"\\ud800\" ]}",
        ));
        let rows = rows_utf16(&source, &u("$")).unwrap();
        assert_eq!(
            rows[0],
            Utf16Row {
                key: vec![0xd800],
                value: Some(vec![0xdc00, 0xd800]),
                kind: 1
            }
        );
        assert_eq!(rows[1].value, Some(u("1.20e+2")));
        assert_eq!(rows[1].key, rows[2].key);
        assert_eq!(rows[3].value, None);
        assert_eq!(rows[3].kind, 0);
        assert_eq!(rows[4].kind, 3);
        assert_eq!(rows[5].value, Some(u("[ \"\\ud800\" ]")));
        assert_eq!(rows[5].kind, 4);
        assert_eq!(
            rows_utf16(&u("[\"\\ud800\",\"\\udc00\",{}]"), &u("$")).unwrap()[0].value,
            Some(vec![0xd800])
        );
        assert_eq!(
            rows_utf16(&u("{\"a\":[],\"bad\":invalid}"), &u("$.a")),
            Err(DOCUMENT)
        );
        assert_eq!(rows_utf16(&u("{}"), &u("strict $.missing")), Err(MISSING));
        assert_eq!(rows_utf16(&u("{}"), &u("$[*]")), Err(PATH));
        let mut path = u("$.\"");
        path.push(0xd800);
        path.push(b'"' as u16);
        assert!(rows_utf16(&source, &path).unwrap().is_empty());
        assert_eq!(
            schema::column_utf16(&source, &path, false).unwrap(),
            Some(vec![0xdc00, 0xd800])
        );
    }

    #[test]
    fn utf16_explicit_schema_retains_long_strings_and_fragment_spelling() {
        let mut text = vec![b'"' as u16];
        text.extend(vec![0xd800; 4001]);
        text.push(b'"' as u16);
        assert_eq!(
            schema::column_utf16(&text, &u("$"), false).unwrap(),
            Some(vec![0xd800; 4001])
        );
        let source = u("{\"a\":[ {\"x\":\"\\ud800\"}, null, 1.20e+2 ]}");
        assert_eq!(
            schema::sources_utf16(&source, &u("$.a")).unwrap(),
            vec![u("{\"x\":\"\\ud800\"}"), u("null"), u("1.20e+2")]
        );
        assert_eq!(
            schema::column_utf16(&source, &u("$.a"), true).unwrap(),
            Some(u("[ {\"x\":\"\\ud800\"}, null, 1.20e+2 ]"))
        );
        assert_eq!(
            binary::convert_utf16(&u("\"AP8B\""), &u("$"), 4, true).unwrap(),
            Some(vec![0, 255, 1, 0])
        );
        for unit in [0x80, 0xd800, 0xdc00, 0xffff] {
            assert!(binary::convert_utf16(&[34, unit, 34], &u("$"), -1, false).is_err());
        }
    }

    #[test]
    fn utf16_ordinary_documents_match_existing_rules() {
        for source in [
            "{}",
            "[]",
            "[null,true,3.14,\"雪🦆\",[],{}]",
            r#"{"a":{"x":1},"a":2}"#,
            r#"{"a":[1,2],"s":"a\u0062"}"#,
            "{\"a\":1,}",
            "null",
        ] {
            for path in ["$", "$.a", "strict $.missing", "$.a[0]", "wrong"] {
                let old = rows(source, path).map(|rows| {
                    rows.into_iter()
                        .map(|row| Utf16Row {
                            key: u(&row.key),
                            value: row.value.map(|text| u(&text)),
                            kind: row.kind,
                        })
                        .collect::<Vec<_>>()
                });
                assert_eq!(rows_utf16(&u(source), &u(path)), old, "{source} {path}");
                let old = schema::sources(source, path)
                    .map(|rows| rows.iter().map(|row| u(row)).collect::<Vec<_>>());
                assert_eq!(
                    schema::sources_utf16(&u(source), &u(path)),
                    old,
                    "{source} {path}"
                );
                for fragment in [false, true] {
                    let old = schema::column(source, path, fragment)
                        .map(|value| value.map(|text| u(&text)));
                    assert_eq!(
                        schema::column_utf16(&u(source), &u(path), fragment),
                        old,
                        "{source} {path} {fragment}"
                    );
                }
            }
        }
    }
    #[test]
    fn lexical_rows_and_paths() {
        assert_eq!(
            rows(r#"{"n":1.20e+2,"n":3,"s":"a\u0062","z":null}"#, "$").unwrap(),
            vec![
                Row {
                    key: "n".into(),
                    value: Some("1.20e+2".into()),
                    kind: 2
                },
                Row {
                    key: "n".into(),
                    value: Some("3".into()),
                    kind: 2
                },
                Row {
                    key: "s".into(),
                    value: Some("ab".into()),
                    kind: 1
                },
                Row {
                    key: "z".into(),
                    value: None,
                    kind: 0
                },
            ]
        );
        assert_eq!(rows("{}", "strict $.missing"), Err(MISSING));
        assert_eq!(rows("1", "$"), Err(DOCUMENT));
        assert_eq!(rows("{\"a\":[],\"tail\":invalid}", "$.a"), Err(DOCUMENT));
        assert_eq!(rows("{}", "nope"), Err(PATH));
        assert_eq!(rows("{\"a\":2}", "$.a").unwrap(), vec![]);
    }
}
