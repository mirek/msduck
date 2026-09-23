//! Trim materialized UTF-16 without stringifying the physical carrier.
//! Default ASCII-space trimming preserves raw units. Explicit character lists
//! currently match exact units; linguistic collation equivalence remains a gap.
use crate::unicode_carrier::{CELL_LIMIT, CHUNK_LIMIT, bytes, is_logical, kind, vector_units};
use duckdb::{
    core::{DataChunkHandle, Inserter, LogicalTypeId as Id},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};

struct Trim<const LEFT: bool, const RIGHT: bool>;
impl<const LEFT: bool, const RIGHT: bool> VScalar for Trim<LEFT, RIGHT> {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let source = input.flat_vector(0);
        let chars = input.flat_vector(1);
        let unicode = is_logical(&source.logical_type());
        let source_struct = unicode.then(|| input.struct_vector(0));
        let chars_struct = is_logical(&chars.logical_type()).then(|| input.struct_vector(1));
        let source_data = source_struct.as_ref().map(|s| s.child(0, len));
        let chars_data = chars_struct.as_ref().map(|s| s.child(0, len));
        let mut remaining = CHUNK_LIMIT;
        let mut rows = Vec::with_capacity(len);
        for row in 0..len {
            if source.row_is_null(row as u64) || chars.row_is_null(row as u64) {
                rows.push(None);
                continue;
            }
            let value = if let Some(data) = &source_data {
                vector_units(&source, data, row, len)?.unwrap()
            } else {
                std::str::from_utf8(&bytes(&source, row, len)?)?
                    .encode_utf16()
                    .collect()
            };
            let chars = if let Some(data) = &chars_data {
                vector_units(&chars, data, row, len)?.unwrap()
            } else {
                std::str::from_utf8(&bytes(&chars, row, len)?)?
                    .encode_utf16()
                    .collect()
            };
            // Fixed-size membership avoids quadratic scans of a long character list.
            let mut members = [0u64; 1024];
            for c in chars {
                members[c as usize / 64] |= 1u64 << (c % 64);
            }
            let contains = |c: u16| members[c as usize / 64] & (1u64 << (c % 64)) != 0;
            let mut start = 0;
            let mut end = value.len();
            if LEFT {
                while start < end && contains(value[start]) {
                    start += 1;
                }
            }
            if RIGHT {
                while end > start && contains(value[end - 1]) {
                    end -= 1;
                }
            }
            let encoded = if unicode {
                value[start..end]
                    .iter()
                    .flat_map(|c| c.to_le_bytes())
                    .collect::<Vec<_>>()
            } else {
                String::from_utf16(&value[start..end])?.into_bytes()
            };
            if encoded.len() > CELL_LIMIT || encoded.len() > remaining {
                return Err("trim output exceeds the configured limit".into());
            }
            remaining -= encoded.len();
            rows.push(Some(encoded));
        }
        if unicode {
            let mut result = output.struct_vector();
            let mut data = result.child(0, len);
            for (row, value) in rows.iter().enumerate() {
                if let Some(value) = value {
                    data.insert(row, value.as_slice());
                } else {
                    result.set_null(row);
                    data.set_null(row);
                }
            }
        } else {
            let mut result = output.flat_vector();
            for (row, value) in rows.iter().enumerate() {
                if let Some(value) = value {
                    result.insert(row, std::str::from_utf8(value)?);
                } else {
                    result.set_null(row);
                }
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        let mut signatures = Vec::new();
        for source_unicode in [false, true] {
            for chars_unicode in [false, true] {
                signatures.push(ScalarFunctionSignature::exact(
                    vec![
                        if source_unicode {
                            kind()
                        } else {
                            Id::Varchar.into()
                        },
                        if chars_unicode {
                            kind()
                        } else {
                            Id::Varchar.into()
                        },
                    ],
                    if source_unicode {
                        kind()
                    } else {
                        Id::Varchar.into()
                    },
                ));
            }
        }
        signatures
    }
}

pub fn register(db: &duckdb::Connection) -> duckdb::Result<()> {
    db.register_scalar_function::<Trim<true, false>>("__msduck_ltrim")?;
    db.register_scalar_function::<Trim<false, true>>("__msduck_rtrim")?;
    db.register_scalar_function::<Trim<true, true>>("__msduck_trim")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn database() -> duckdb::Connection {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::unicode_carrier::register(&db).unwrap();
        register(&db).unwrap();
        db
    }
    #[test]
    fn trim_preserves_units_nulls_and_character_sets() {
        let db = database();
        for (name, hex) in [
            ("ltrim", "3ED800202000"),
            ("rtrim", "20003ED80020"),
            ("trim", "3ED80020"),
        ] {
            let value: Vec<u8> = db.query_row(&format!("SELECT (__msduck_{name}(__msduck_unicode_from_le(from_hex('20003ED800202000')),' ')).__msduck_utf16le"), [], |r|r.get(0)).unwrap();
            let expected: Vec<u8> = db
                .query_row("SELECT from_hex(?)", [hex], |r| r.get(0))
                .unwrap();
            assert_eq!(value, expected);
        }
        let nested: Vec<u8> = db.query_row("SELECT (__msduck_ltrim(__msduck_rtrim(__msduck_unicode_from_le(from_hex('20003ED82000')),' '),' ')).__msduck_utf16le",[],|r|r.get(0)).unwrap();
        assert_eq!(nested, [0x3e, 0xd8]);
        let explicit: Vec<u8> = db.query_row("SELECT (__msduck_trim(__msduck_pack_unicode('xyduckxy'),__msduck_pack_unicode('xy'))).__msduck_utf16le",[],|r|r.get(0)).unwrap();
        assert_eq!(
            explicit,
            "duck"
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>()
        );
        for sql in [
            "SELECT __msduck_trim(NULL,' ')",
            "SELECT __msduck_trim('x',NULL)",
            "SELECT __msduck_trim(NULL,NULL)",
            "SELECT __msduck_trim(__msduck_pack_unicode(NULL),' ')",
        ] {
            let value: duckdb::types::Value = db.query_row(sql, [], |r| r.get(0)).unwrap();
            assert_eq!(value, duckdb::types::Value::Null);
        }
        let empty: String = db
            .query_row("SELECT __msduck_trim(' x ','')", [], |r| r.get(0))
            .unwrap();
        assert_eq!(empty, " x ");
        assert!(
            db.query_row(
                "SELECT __msduck_trim(struct_pack(__msduck_utf16le := from_hex('01')),' ')",
                [],
                |r| r.get::<_, duckdb::types::Value>(0)
            )
            .is_err()
        );
    }
    #[test]
    fn trim_vectors_evaluate_each_operand_once() {
        let db = database();
        db.execute_batch("CREATE SEQUENCE trim_source; CREATE SEQUENCE trim_chars; CREATE TABLE trimmed AS SELECT i,__msduck_trim(__msduck_pack_unicode(CASE WHEN i%17=0 THEN NULL ELSE ' '||CAST(nextval('trim_source') AS VARCHAR)||' ' END),CASE WHEN nextval('trim_chars')>0 THEN ' ' END) AS value FROM range(6000) t(i)").unwrap();
        let count: i64 = db
            .query_row("SELECT count(value) FROM trimmed", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 5647);
        let calls: (i64, i64) = db
            .query_row(
                "SELECT currval('trim_source'),currval('trim_chars')",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(calls, (5647, 6000));
        let filtered: i64 = db
            .query_row(
                "SELECT count(__msduck_trim(value,' ')) FROM trimmed WHERE i%3=0",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(filtered, 1882);
    }
}
