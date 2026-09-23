//! Source rows and scalar/fragment selection for explicit schemas.
use super::*;
const COLUMN_MISSING: &str = "OPENJSON: missing column path";
const COLUMN_KIND: &str = "OPENJSON: wrong column kind";
pub(super) fn diagnostic(message: &str) -> Option<SqlError> {
    match message {
        COLUMN_MISSING => Some(SqlError::new(13608, 6, json::MISSING)),
        COLUMN_KIND => Some(SqlError::new(13624, 1, json::CONTAINER)),
        _ => None,
    }
}
pub(super) fn resolve<'a>(
    source: &'a str,
    path_text: &str,
    container: bool,
) -> Result<Option<&'a str>, &'static str> {
    let kind = root(source.as_bytes()).ok_or(DOCUMENT)?;
    if container && !matches!(kind, Kind::Object | Kind::Array) {
        return Err(DOCUMENT);
    }
    let (strict, steps) = path(path_text).map_err(|_| PATH)?;
    let Some(tail) = selected(source, &steps)? else {
        return if strict {
            Err(if container { MISSING } else { COLUMN_MISSING })
        } else {
            Ok(None)
        };
    };
    let (_, end) = prefix(tail.as_bytes()).ok_or(DOCUMENT)?;
    Ok(Some(&tail[..end]))
}
/// Produce source text for explicit-schema rows without coercing column types.
pub fn sources(source: &str, path_text: &str) -> Result<Vec<String>, &'static str> {
    let Some(value) = resolve(source, path_text, true)? else {
        return Ok(vec![]);
    };
    if value.starts_with('{') {
        return Ok(vec![value.into()]);
    }
    if !value.starts_with('[') {
        return Ok(vec![]);
    }
    let mut rest = trim(&value[1..value.len() - 1]);
    let mut rows = Vec::new();
    while !rest.is_empty() {
        let (_, end) = prefix(rest.as_bytes()).ok_or(DOCUMENT)?;
        rows.push(rest[..end].to_owned());
        rest = trim(&rest[end..]);
        if let Some(tail) = rest.strip_prefix(',') {
            rest = trim(tail);
        }
    }
    Ok(rows)
}
/// Select scalar text or an AS JSON fragment; casts belong to the adapter.
pub fn column(
    source: &str,
    path_text: &str,
    fragment: bool,
) -> Result<Option<String>, &'static str> {
    let Some(value) = resolve(source, path_text, false)? else {
        return Ok(None);
    };
    let (strict, _) = path(path_text).map_err(|_| PATH)?;
    let kind = root(value.as_bytes()).ok_or(DOCUMENT)?;
    let structured = matches!(kind, Kind::Object | Kind::Array);
    if structured != fragment {
        return if strict { Err(COLUMN_KIND) } else { Ok(None) };
    };
    if kind == Kind::String {
        return decode(value).map(Some);
    }
    if value == "null" {
        return Ok(None);
    };
    Ok(Some(value.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_slices_and_column_modes() {
        assert_eq!(
            sources(r#"{"items":[ {"A":1,"a":2}, {"A":3} ]}"#, "strict $.items").unwrap(),
            vec![r#"{"A":1,"a":2}"#, r#"{"A":3}"#]
        );
        assert_eq!(
            column(r#"{"A":1,"a":2}"#, "$.A", false).unwrap(),
            Some("1".into())
        );
        assert_eq!(
            column(r#"{"obj": { "x": 1 }}"#, "$.obj", true).unwrap(),
            Some("{ \"x\": 1 }".into())
        );
        assert_eq!(column("{}", "strict $.missing", false), Err(COLUMN_MISSING));
        assert_eq!(
            column("{\"obj\":{}}", "strict $.obj", false),
            Err(COLUMN_KIND)
        );
        assert_eq!(column("1", "strict $", true), Err(COLUMN_KIND));
        assert_eq!(column("null", "$", false).unwrap(), None);
        let long = format!("\"{}\"", "🦆".repeat(3000));
        assert_eq!(
            column(&long, "$", false)
                .unwrap()
                .unwrap()
                .encode_utf16()
                .count(),
            6000
        );
    }
}
