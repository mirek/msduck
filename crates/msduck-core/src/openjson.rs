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

#[cfg(test)]
mod tests {
    use super::*;
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
