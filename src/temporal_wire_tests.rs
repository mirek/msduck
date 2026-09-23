//! DuckDB value-to-TDS integration stays outside the deterministic core.
use crate::datetime2::DateTime2;
use crate::datetimeoffset::DateTimeOffset;
#[test]
fn timestamp_result_encoding_preserves_range_and_rounds_negative_instants() {
    use duckdb::types::{TimeUnit, Value};
    for (unit, value, expected) in [
        (TimeUnit::Nanosecond, -151, "1969-12-31T23:59:59.9999998"),
        (TimeUnit::Nanosecond, 149, "1970-01-01T00:00:00.0000001"),
        (TimeUnit::Nanosecond, 150, "1970-01-01T00:00:00.0000002"),
        (
            TimeUnit::Second,
            -62_135_596_800,
            "0001-01-01T00:00:00.0000000",
        ),
        (
            TimeUnit::Second,
            253_402_300_799,
            "9999-12-31T23:59:59.0000000",
        ),
    ] {
        let mut bytes = Vec::new();
        crate::engine::encode_value(
            &mut bytes,
            &crate::tds::Type::DateTime,
            &Value::Timestamp(unit, value),
        )
        .unwrap();
        assert_eq!(bytes[0], 8);
        assert_eq!(
            DateTime2::decode(&bytes[1..], 7)
                .unwrap()
                .format_iso(7)
                .unwrap(),
            expected
        );
    }
}
#[cfg(test)]
mod offset {
    use super::*;
    #[test]
    fn datetimeoffset_metadata_and_result_values() {
        let value = DateTimeOffset::parse_iso("2026-07-01T02:30:00.1234567+05:30").unwrap();
        for scale in 0..=7 {
            let kind = crate::tds::Type::DateTimeOffset(scale);
            let mut metadata = vec![];
            crate::tds::metadata(
                &mut metadata,
                &[crate::tds::Column {
                    collation: None,
                    properties: Default::default(),
                    name: String::new(),
                    kind: kind.clone(),
                }],
            )
            .unwrap();
            assert_eq!(metadata, [0x81, 1, 0, 0, 0, 0, 0, 1, 0, 0x2b, scale, 0]);
            let mut result = vec![];
            crate::engine::encode_value(
                &mut result,
                &kind,
                &duckdb::types::Value::Text(value.format_iso(7).unwrap()),
            )
            .unwrap();
            assert_eq!(usize::from(result[0]), result.len() - 1);
            assert_eq!(
                DateTimeOffset::decode(&result[1..], scale).unwrap(),
                value.round(scale).unwrap()
            );
            result.clear();
            crate::engine::encode_value(&mut result, &kind, &duckdb::types::Value::Null).unwrap();
            assert_eq!(result, [0]);
        }
        assert!(
            crate::tds::metadata(
                &mut vec![],
                &[crate::tds::Column {
                    collation: None,
                    properties: Default::default(),
                    name: String::new(),
                    kind: crate::tds::Type::DateTimeOffset(8)
                }]
            )
            .is_err()
        );
    }
}
