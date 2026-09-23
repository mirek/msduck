//! Exact DATETIME2 casts using a tagged one-field DuckDB STRUCT.
use crate::datetime2::{DateTime2, Parts};
use duckdb::{
    core::{DataChunkHandle, LogicalTypeHandle, LogicalTypeId as Id},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};

pub const CONVERSION: &str =
    "Conversion failed when converting date and/or time from character string.";
pub use msduck_sql::datetime2_cast::*;

pub fn arrow_scale(kind: &duckdb::arrow::datatypes::DataType) -> Option<u8> {
    use duckdb::arrow::datatypes::DataType as A;
    if let A::Struct(fields) = kind
        && fields.len() == 1
        && fields[0].data_type() == &A::Int64
    {
        return field_scale(fields[0].name());
    }
    None
}
struct Cast<const SCALE: u8, const TRY: bool>;
impl<const SCALE: u8, const TRY: bool> VScalar for Cast<SCALE, TRY> {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let source = input.flat_vector(0);
        let logical = source.logical_type();
        let kind = logical.id();
        let tagged = kind == Id::Struct
            && logical.num_children() == 1
            && field_scale(&logical.child_name(0)).is_some()
            && logical.child(0).id() == Id::Bigint;
        let offset = kind == Id::Struct
            && logical.num_children() == 2
            && logical.child(0).id() == Id::Bigint
            && (0..=7).any(|s| logical.child_name(0) == format!("__msduck_datetimeoffset_{s}"))
            && logical.child_name(1) == "__msduck_offset_minutes"
            && logical.child(1).id() == Id::Smallint;
        if !(tagged
            || offset
            || matches!(
                kind,
                Id::Varchar
                    | Id::Date
                    | Id::Timestamp
                    | Id::TimestampS
                    | Id::TimestampMs
                    | Id::TimestampNs
                    | Id::Time
                    | Id::TimeNs
                    | Id::SqlNull
            ))
        {
            return Err("unsupported input type for DATETIME2 conversion".into());
        }
        let struct_source = (tagged || offset).then(|| input.struct_vector(0));
        let child = struct_source.as_ref().map(|s| s.child(0, len));
        let offsets = offset.then(|| struct_source.as_ref().unwrap().child(1, len));
        let mut result = output.struct_vector();
        let mut ticks = result.child(0, len);
        for row in 0..len {
            if source.row_is_null(row as u64) {
                result.set_null(row);
                ticks.set_null(row);
                continue;
            }
            let converted = (|| -> anyhow::Result<DateTime2> {
                let value = unsafe {
                    if let Some(child) = &child {
                        anyhow::ensure!(!child.row_is_null(row as u64), "invalid DATETIME2 ticks");
                        let value =
                            DateTime2::from_ticks(child.as_slice_with_len::<i64>(len)[row])?;
                        if let Some(offsets) = &offsets {
                            anyhow::ensure!(
                                !offsets.row_is_null(row as u64),
                                "invalid DATETIMEOFFSET offset"
                            );
                            crate::datetimeoffset::DateTimeOffset::from_utc(
                                value,
                                offsets.as_slice_with_len::<i16>(len)[row],
                            )?
                            .local()
                        } else {
                            value
                        }
                    } else if kind == Id::Varchar {
                        let mut text =
                            source.as_slice_with_len::<duckdb::ffi::duckdb_string_t>(len)[row];
                        let length = duckdb::ffi::duckdb_string_t_length(text) as usize;
                        let bytes = std::slice::from_raw_parts(
                            duckdb::ffi::duckdb_string_t_data(&mut text).cast::<u8>(),
                            length,
                        );
                        crate::datetimeoffset::DateTimeOffset::parse_local(std::str::from_utf8(
                            bytes,
                        )?)?
                        .0
                    } else if kind == Id::Date {
                        let days = source.as_slice_with_len::<i32>(len)[row];
                        DateTime2::from_unix_nanos(i128::from(days) * 86_400_000_000_000)?
                    } else if matches!(kind, Id::Time | Id::TimeNs) {
                        let time = source.as_slice_with_len::<i64>(len)[row];
                        let base = DateTime2::from_parts(Parts {
                            year: 1900,
                            month: 1,
                            day: 1,
                            hour: 0,
                            minute: 0,
                            second: 0,
                            fraction: 0,
                        })?;
                        let nanos = i128::from(time) * if kind == Id::Time { 1000 } else { 1 };
                        anyhow::ensure!((0..86_400_000_000_000).contains(&nanos), "invalid time");
                        DateTime2::from_ticks(base.ticks() + ((nanos + 50) / 100) as i64)?
                    } else {
                        let value = source.as_slice_with_len::<i64>(len)[row];
                        let factor = match kind {
                            Id::TimestampS => 1_000_000_000,
                            Id::TimestampMs => 1_000_000,
                            Id::Timestamp => 1000,
                            Id::TimestampNs => 1,
                            _ => anyhow::bail!("invalid DATETIME2 input"),
                        };
                        DateTime2::from_unix_nanos(i128::from(value) * factor)?
                    }
                };
                value.round(SCALE)
            })();
            match converted {
                Ok(value) => unsafe {
                    ticks.as_mut_slice_with_len::<i64>(len)[row] = value.ticks();
                },
                Err(_) if TRY => {
                    result.set_null(row);
                    ticks.set_null(row);
                }
                Err(_) => return Err(CONVERSION.into()),
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![Id::Any.into()],
            LogicalTypeHandle::struct_type(&[(&field(SCALE), Id::Bigint.into())]),
        )]
    }
}
pub fn register(db: &duckdb::Connection) -> duckdb::Result<()> {
    macro_rules! add {($($s:literal),*)=>{$(
        db.register_scalar_function::<Cast<$s,false>>(&format!("{PREFIX}cast_{}",$s))?;
        db.register_scalar_function::<Cast<$s,true>>(&format!("{PREFIX}try_{}",$s))?;
    )*};}
    add!(0, 1, 2, 3, 4, 5, 6, 7);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn offset_text_preserves_local_ticks_nulls_and_single_evaluation() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        register(&db).unwrap();
        let local = DateTime2::parse_iso("2024-01-01T00:15:30.1234567").unwrap();
        for scale in 0..=7 {
            let ticks = local.round(scale).unwrap().ticks();
            let wrong:i64=db.query_row(&format!("SELECT count(*) FROM range(6000) r(i) WHERE (__msduck_datetime2_try_{scale}(CASE WHEN i%17=0 THEN NULL WHEN i%19=0 THEN '2024-01-01T00:15:30+14:01' WHEN i%2=0 THEN '2024-01-01T00:15:30.1234567+14:00' ELSE '2024-01-01T00:15:30.1234567-14:00' END)).__msduck_datetime2_{scale} IS DISTINCT FROM CASE WHEN i%17=0 OR i%19=0 THEN NULL ELSE {ticks} END"),[],|r|r.get(0)).unwrap();
            assert_eq!(wrong, 0);
        }
        db.execute_batch("CREATE SEQUENCE local_text_calls START 1")
            .unwrap();
        let count:i64=db.query_row("SELECT count(__msduck_datetime2_cast_7(printf('2024-01-01T00:00:%02d.1234567+14:00',nextval('local_text_calls')%60))) FROM range(6000)",[],|r|r.get(0)).unwrap();
        assert_eq!(count, 6000);
        assert_eq!(
            db.query_row("SELECT currval('local_text_calls')", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            6000
        );
    }

    #[test]
    fn offset_conversion_retains_local_ticks_across_scales_and_chunks() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        let local = DateTime2::parse_iso("2024-01-01T00:15:30.1234567").unwrap();
        for source in 0..=7 {
            for target in 0..=7 {
                let expected = local.round(source).unwrap().round(target).unwrap().ticks();
                let wrong: i64 = db.query_row(&format!("SELECT count(*) FROM range(6000) r(i) WHERE (__msduck_datetime2_cast_{target}(__msduck_datetimeoffset_cast_{source}(CASE WHEN i%17=0 THEN NULL WHEN i%2=0 THEN '2024-01-01T00:15:30.1234567+14:00' ELSE '2024-01-01T00:15:30.1234567-14:00' END))).__msduck_datetime2_{target} IS DISTINCT FROM CASE WHEN i%17=0 THEN NULL ELSE {expected} END"), [], |r| r.get(0)).unwrap();
                assert_eq!(wrong, 0, "{source} -> {target}");
            }
        }
        db.execute_batch("CREATE SEQUENCE local_cast_calls START 1")
            .unwrap();
        let count: i64 = db.query_row("SELECT count(__msduck_datetime2_cast_3(__msduck_datetimeoffset_cast_7(CASE WHEN nextval('local_cast_calls')%3=0 THEN NULL ELSE '2024-01-01T00:15:30.1234567+14:00' END))) FROM range(6000)", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 4000);
        let calls: i64 = db
            .query_row("SELECT currval('local_cast_calls')", [], |r| r.get(0))
            .unwrap();
        assert_eq!(calls, 6000);
    }

    #[test]
    fn tagged_vectors_preserve_ticks_nulls_and_nested_rounding_across_chunks() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        register(&db).unwrap();
        let expected = DateTime2::parse_iso("2024-02-29T12:34:56.1234567").unwrap();
        for scale in 0..=7 {
            let sql = format!(
                "SELECT (__msduck_datetime2_cast_{scale}(__msduck_datetime2_try_7(CASE WHEN i%17=0 THEN NULL WHEN i%19=0 THEN 'bad' ELSE '2024-02-29T12:34:56.1234567' END))).__msduck_datetime2_{scale} FROM range(6000) r(i)"
            );
            let mut statement = db.prepare(&sql).unwrap();
            let values = statement
                .query_map([], |r| r.get::<_, Option<i64>>(0))
                .unwrap()
                .collect::<duckdb::Result<Vec<_>>>()
                .unwrap();
            assert_eq!(values.len(), 6000);
            for (i, value) in values.into_iter().enumerate() {
                assert_eq!(
                    value,
                    if i % 17 == 0 || i % 19 == 0 {
                        None
                    } else {
                        Some(expected.round(scale).unwrap().ticks())
                    }
                );
            }
        }
        for (value, expected) in [
            ("DATE '0001-01-01'", "0001-01-01T00:00:00"),
            (
                "TIMESTAMP '9999-12-31 23:59:59.999999'",
                "9999-12-31T23:59:59.999999",
            ),
            ("TIME '12:34:56.123456'", "1900-01-01T12:34:56.123456"),
        ] {
            let sql = format!("SELECT (__msduck_datetime2_cast_7({value})).__msduck_datetime2_7");
            assert_eq!(
                db.query_row(&sql, [], |r| r.get::<_, i64>(0)).unwrap(),
                DateTime2::parse_iso(expected).unwrap().ticks()
            );
        }
    }
}
