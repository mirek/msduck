//! Native functions over Unicode carriers: sort keys, LIKE and text.
use super::{key, like::Pattern};
use crate::unicode_carrier::{CELL_LIMIT, CHUNK_LIMIT, bytes, is_logical, kind, vector_units};
use duckdb::{
    core::{DataChunkHandle, Inserter, LogicalTypeId as Id},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};

/// `value -> carrier` for a carrier, VARCHAR text or untyped NULL. Unlike
/// `__msduck_carrier_input` it binds for every type (including other
/// STRUCTs), so it can sit in a `typeof` dispatch branch that DuckDB binds
/// but does not run. Other types give NULL, or with `STRICT` (operands
/// known to be compared as Unicode text) an error.
pub(super) struct Input<const STRICT: bool>;
impl<const STRICT: bool> VScalar for Input<STRICT> {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let source = input.flat_vector(0);
        let kind = source.logical_type();
        let carrier = is_logical(&kind);
        let text = matches!(kind.id(), Id::Varchar | Id::StringLiteral);
        if !carrier && !text && kind.id() != Id::SqlNull {
            if STRICT {
                return Err(format!(
                    "Comparing nvarchar with a value of DuckDB type {:?} is not supported.",
                    kind.id()
                )
                .into());
            }
            let mut result = output.struct_vector();
            for row in 0..len {
                result.set_null(row);
                result.child(0, len).set_null(row);
            }
            return Ok(());
        }
        let data = carrier.then(|| input.struct_vector(0).child(0, len));
        let mut result = output.struct_vector();
        let mut payload = result.child(0, len);
        let mut remaining = CHUNK_LIMIT;
        for row in 0..len {
            let encoded = if let Some(data) = &data {
                vector_units(&source, data, row, len)?.map(|units| {
                    units
                        .iter()
                        .flat_map(|u| u.to_le_bytes())
                        .collect::<Vec<u8>>()
                })
            } else if text && !source.row_is_null(row as u64) {
                let utf8 = bytes(&source, row, len)?;
                Some(
                    std::str::from_utf8(&utf8)?
                        .encode_utf16()
                        .flat_map(u16::to_le_bytes)
                        .collect(),
                )
            } else {
                None
            };
            match encoded {
                Some(encoded) => {
                    if encoded.len() > CELL_LIMIT || encoded.len() > remaining {
                        return Err("Unicode input exceeds the configured limit".into());
                    }
                    remaining -= encoded.len();
                    payload.insert(row, encoded.as_slice());
                }
                None => {
                    result.set_null(row);
                    payload.set_null(row);
                }
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(vec![Id::Any.into()], kind())]
    }
}

/// `__msduck_unicode_order_key(carrier) -> BLOB`: see [`key`].
pub(super) struct OrderKey;
impl VScalar for OrderKey {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let source = input.flat_vector(0);
        let data = input.struct_vector(0).child(0, len);
        let mut result = output.flat_vector();
        let mut remaining = CHUNK_LIMIT * 4;
        for row in 0..len {
            let Some(units) = vector_units(&source, &data, row, len)? else {
                result.set_null(row);
                continue;
            };
            let key = key::order_key(&units);
            if key.len() > remaining {
                return Err("Unicode sort keys exceed the configured chunk limit".into());
            }
            remaining -= key.len();
            result.insert(row, key.as_slice());
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![kind()],
            Id::Blob.into(),
        )]
    }
}

/// `__msduck_unicode_text(carrier) -> VARCHAR`. Isolated surrogates, which
/// UTF-8 cannot hold, become U+FFFD.
pub(super) struct Text;
impl VScalar for Text {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let source = input.flat_vector(0);
        let data = input.struct_vector(0).child(0, len);
        let mut result = output.flat_vector();
        let mut remaining = CHUNK_LIMIT * 2;
        for row in 0..len {
            let Some(units) = vector_units(&source, &data, row, len)? else {
                result.set_null(row);
                continue;
            };
            let text = String::from_utf16_lossy(&units);
            if text.len() > remaining {
                return Err("Unicode text exceeds the configured chunk limit".into());
            }
            remaining -= text.len();
            result.insert(row, text.as_str());
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![kind()],
            Id::Varchar.into(),
        )]
    }
}

/// SQL Server's error for an escape that is not exactly one character.
pub(super) fn invalid_escape(escape: &str) -> String {
    format!("The invalid escape character \"{escape}\" was specified in a LIKE predicate.")
}

/// `__msduck_unicode_like(value, pattern[, escape]) -> BOOLEAN`, all
/// carriers. A NULL escape means no escape character, as in SQL Server.
pub(super) struct Like;
impl VScalar for Like {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let escaped = input.num_columns() == 3;
        let values = input.flat_vector(0);
        let value_data = input.struct_vector(0).child(0, len);
        let patterns = input.flat_vector(1);
        let pattern_data = input.struct_vector(1).child(0, len);
        let escapes = escaped.then(|| input.flat_vector(2));
        let escape_data = escaped.then(|| input.struct_vector(2).child(0, len));
        let mut result = output.flat_vector();
        // Patterns are usually constant: parse once per distinct pattern.
        let mut parsed: Option<(Vec<u16>, Option<u16>, Pattern)> = None;
        for row in 0..len {
            let (Some(value), Some(pattern)) = (
                vector_units(&values, &value_data, row, len)?,
                vector_units(&patterns, &pattern_data, row, len)?,
            ) else {
                result.set_null(row);
                continue;
            };
            let escape = match (&escapes, &escape_data) {
                (Some(escapes), Some(data)) => match vector_units(escapes, data, row, len)? {
                    Some(units) if units.len() == 1 => Some(units[0]),
                    Some(units) => {
                        return Err(invalid_escape(&String::from_utf16_lossy(&units)).into());
                    }
                    None => None,
                },
                _ => None,
            };
            if !parsed
                .as_ref()
                .is_some_and(|(p, e, _)| *p == pattern && *e == escape)
            {
                let compiled = Pattern::parse(&pattern, escape);
                parsed = Some((pattern, escape, compiled));
            }
            let matched = parsed.as_ref().is_some_and(|(_, _, p)| p.matches(&value));
            unsafe {
                result.as_mut_slice_with_len::<bool>(len)[row] = matched;
            }
        }
        Ok(())
    }
    // A NULL escape means no escape character.
    fn special_null_handling() -> bool {
        true
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![
            ScalarFunctionSignature::exact(vec![kind(), kind()], Id::Boolean.into()),
            ScalarFunctionSignature::exact(vec![kind(), kind(), kind()], Id::Boolean.into()),
        ]
    }
}

#[cfg(test)]
mod tests {
    fn db() -> (crate::server::Server, crate::server::Connection) {
        let server = crate::server::Server::open(":memory:").unwrap();
        let connection = server.connection().unwrap();
        (server, connection)
    }

    fn text(db: &duckdb::Connection, sql: &str) -> Option<String> {
        db.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn input_accepts_carriers_text_and_null_only() {
        let (_server, db) = db();
        let units = "SELECT hex(struct_extract(__msduck_unicode_input(x), '__msduck_utf16le')) FROM (SELECT {} AS x)";
        assert_eq!(
            text(&db, &units.replace("{}", "'a\u{1F986}'")).as_deref(),
            Some("61003ED886DD")
        );
        assert_eq!(
            text(
                &db,
                &units.replace("{}", "__msduck_unicode_from_le(from_hex('3DD8'))")
            )
            .as_deref(),
            Some("3DD8")
        );
        assert_eq!(text(&db, &units.replace("{}", "NULL")), None);
        assert_eq!(text(&db, &units.replace("{}", "42")), None);
        let strict = db.query_row("SELECT __msduck_unicode_operand(42)", [], |r| {
            r.get::<_, Option<String>>(0)
        });
        assert!(
            strict
                .unwrap_err()
                .to_string()
                .contains("Comparing nvarchar with a value")
        );
    }

    #[test]
    fn order_keys_and_text_over_vectors() {
        let (_server, db) = db();
        // 3000 rows span several vectors; constant and varying inputs.
        let ordered: String = db
            .query_row(
                "SELECT string_agg(v, ',' ORDER BY __msduck_unicode_order_key(__msduck_unicode_input(v)), v)
                 FROM (SELECT CASE i % 5 WHEN 0 THEN 'b' WHEN 1 THEN 'a ' WHEN 2 THEN 'a' || chr(0) WHEN 3 THEN chr(256) ELSE 'B' END AS v
                       FROM range(3000) r(i))",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let mut seen: Vec<&str> = ordered.split(',').collect();
        seen.dedup();
        assert_eq!(seen, ["B", "a\0", "a ", "b", "\u{100}"]);
        assert_eq!(
            text(
                &db,
                "SELECT __msduck_unicode_text(__msduck_unicode_from_le(from_hex('61003DD86200')))"
            )
            .as_deref(),
            Some("a\u{FFFD}b")
        );
        let equal: bool = db
            .query_row(
                "SELECT __msduck_unicode_order_key(__msduck_unicode_input('x')) = __msduck_unicode_order_key(__msduck_unicode_input('x   '))",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(equal);
    }

    #[test]
    fn like_handles_escapes_nulls_and_varying_patterns() {
        let (_server, db) = db();
        let like = |value: &str, pattern: &str, escape: &str| -> Option<bool> {
            db.query_row(
                &format!(
                    "SELECT __msduck_unicode_like(__msduck_unicode_input({value}), __msduck_unicode_input({pattern}){escape})"
                ),
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(
            like("'a%'", "'a!%'", ", __msduck_unicode_input('!')"),
            Some(true)
        );
        assert_eq!(
            like("'a'", "'a'", ", __msduck_unicode_input(NULL)"),
            Some(true)
        );
        assert_eq!(like("NULL", "'%'", ""), None);
        assert_eq!(like("'a'", "NULL", ""), None);
        let error = db
            .query_row(
                "SELECT __msduck_unicode_like(__msduck_unicode_input('a'), __msduck_unicode_input('a'), __msduck_unicode_input('ab'))",
                [],
                |r| r.get::<_, bool>(0),
            )
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("The invalid escape character \"ab\"")
        );
        let matches: i64 = db
            .query_row(
                "SELECT count(*) FROM range(3000) r(i)
                 WHERE __msduck_unicode_like(__msduck_unicode_input('x' || (i % 7)), __msduck_unicode_input(CASE WHEN i % 2 = 0 THEN 'x[0-3]' ELSE '%5' END))",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let expected = (0..3000)
            .filter(|i| if i % 2 == 0 { i % 7 <= 3 } else { i % 7 == 5 })
            .count();
        assert_eq!(matches, expected as i64);
    }
}
