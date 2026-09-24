//! DATETIMEOFFSET casts retain UTC ticks, offset and declared scale.
use crate::{
    datetime2::{DateTime2, Parts},
    datetimeoffset::DateTimeOffset,
};
use duckdb::{
    core::{DataChunkHandle, LogicalTypeHandle, LogicalTypeId as Id},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};

pub const CONVERSION: &str =
    "Conversion failed when converting date and/or time from character string.";
pub use msduck_sql::datetimeoffset_cast::*;

pub fn arrow_scale(kind: &duckdb::arrow::datatypes::DataType) -> Option<u8> {
    use duckdb::arrow::datatypes::DataType as A;
    if let A::Struct(fields) = kind
        && fields.len() == 2
        && fields[0].data_type() == &A::Int64
        && fields[1].name() == OFFSET
        && fields[1].data_type() == &A::Int16
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
        let offset_tag = kind == Id::Struct
            && logical.num_children() == 2
            && field_scale(&logical.child_name(0)).is_some()
            && logical.child(0).id() == Id::Bigint
            && logical.child_name(1) == OFFSET
            && logical.child(1).id() == Id::Smallint;
        let datetime_tag = kind == Id::Struct
            && logical.num_children() == 1
            && logical.child(0).id() == Id::Bigint
            && (0..=7).any(|s| logical.child_name(0) == format!("__msduck_datetime2_{s}"));
        if !offset_tag
            && !datetime_tag
            && !matches!(
                kind,
                Id::Varchar
                    | Id::Date
                    | Id::Time
                    | Id::TimeNs
                    | Id::Timestamp
                    | Id::TimestampS
                    | Id::TimestampMs
                    | Id::TimestampNs
                    | Id::SqlNull
            )
        {
            return Err("unsupported input type for DATETIMEOFFSET conversion".into());
        }
        let structure = (offset_tag || datetime_tag).then(|| input.struct_vector(0));
        let source_ticks = structure.as_ref().map(|s| s.child(0, len));
        let source_offset = if offset_tag {
            structure.as_ref().map(|s| s.child(1, len))
        } else {
            None
        };
        let mut result = output.struct_vector();
        let mut ticks = result.child(0, len);
        let mut offsets = result.child(1, len);
        for row in 0..len {
            if source.row_is_null(row as u64) {
                result.set_null(row);
                ticks.set_null(row);
                offsets.set_null(row);
                continue;
            }
            let converted = (|| -> anyhow::Result<DateTimeOffset> {
                // Each type/field is validated before reading bounded live slots.
                let value = unsafe {
                    if let Some(child) = &source_ticks {
                        anyhow::ensure!(
                            !child.row_is_null(row as u64),
                            "invalid DATETIMEOFFSET ticks"
                        );
                        let value =
                            DateTime2::from_ticks(child.as_slice_with_len::<i64>(len)[row])?;
                        if let Some(offset) = &source_offset {
                            anyhow::ensure!(
                                !offset.row_is_null(row as u64),
                                "invalid DATETIMEOFFSET offset"
                            );
                            DateTimeOffset::from_utc(
                                value,
                                offset.as_slice_with_len::<i16>(len)[row],
                            )?
                        } else {
                            DateTimeOffset::from_local(value, 0)?
                        }
                    } else if kind == Id::Varchar {
                        let mut text =
                            source.as_slice_with_len::<duckdb::ffi::duckdb_string_t>(len)[row];
                        let length = duckdb::ffi::duckdb_string_t_length(text) as usize;
                        let bytes = std::slice::from_raw_parts(
                            duckdb::ffi::duckdb_string_t_data(&mut text).cast::<u8>(),
                            length,
                        );
                        DateTimeOffset::parse_iso(std::str::from_utf8(bytes)?)?
                    } else {
                        let local = if kind == Id::Date {
                            DateTime2::from_unix_nanos(
                                i128::from(source.as_slice_with_len::<i32>(len)[row])
                                    * 86_400_000_000_000,
                            )?
                        } else if matches!(kind, Id::Time | Id::TimeNs) {
                            let time = i128::from(source.as_slice_with_len::<i64>(len)[row])
                                * if kind == Id::Time { 1000 } else { 1 };
                            anyhow::ensure!(
                                (0..86_400_000_000_000).contains(&time),
                                "invalid time"
                            );
                            let base = DateTime2::from_parts(Parts {
                                year: 1900,
                                month: 1,
                                day: 1,
                                hour: 0,
                                minute: 0,
                                second: 0,
                                fraction: 0,
                            })?;
                            DateTime2::from_ticks(base.ticks() + ((time + 50) / 100) as i64)?
                        } else {
                            let factor = match kind {
                                Id::TimestampS => 1_000_000_000,
                                Id::TimestampMs => 1_000_000,
                                Id::Timestamp => 1000,
                                Id::TimestampNs => 1,
                                _ => anyhow::bail!("invalid DATETIMEOFFSET input"),
                            };
                            DateTime2::from_unix_nanos(
                                i128::from(source.as_slice_with_len::<i64>(len)[row]) * factor,
                            )?
                        };
                        DateTimeOffset::from_local(local, 0)?
                    }
                };
                value.round(SCALE)
            })();
            match converted {
                Ok(value) => unsafe {
                    ticks.as_mut_slice_with_len::<i64>(len)[row] = value.utc().ticks();
                    offsets.as_mut_slice_with_len::<i16>(len)[row] = value.offset_minutes();
                },
                Err(_) if TRY => {
                    result.set_null(row);
                    ticks.set_null(row);
                    offsets.set_null(row);
                }
                Err(_) => return Err(CONVERSION.into()),
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![Id::Any.into()],
            LogicalTypeHandle::struct_type(&[
                (&field(SCALE), Id::Bigint.into()),
                (OFFSET, Id::Smallint.into()),
            ]),
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
    fn datetimeoffset_storage_and_defaults_survive_restart() {
        let path = std::env::temp_dir().join(format!(
            "msduck-offset-{}-{}.duckdb",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        {
            let server = crate::server::Server::open(path.to_str().unwrap()).unwrap();
            let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
            assert!(session.batch_response("CREATE TABLE dbo.offset_values(d DATETIMEOFFSET(7) DEFAULT CAST('2024-01-01T12:00:00.1234567+05:30' AS DATETIMEOFFSET(7))); INSERT INTO dbo.offset_values DEFAULT VALUES",&Default::default(),false,None).1);
        }
        {
            let server = crate::server::Server::open(path.to_str().unwrap()).unwrap();
            let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
            assert!(
                session
                    .batch_response(
                        "INSERT INTO dbo.offset_values DEFAULT VALUES",
                        &Default::default(),
                        false,
                        None
                    )
                    .1
            );
            let mut query=session.db.prepare("SELECT d.__msduck_datetimeoffset_7,d.__msduck_offset_minutes FROM dbo.offset_values").unwrap();
            let rows = query
                .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i16>(1)?)))
                .unwrap()
                .collect::<duckdb::Result<Vec<_>>>()
                .unwrap();
            let expected = DateTimeOffset::parse_iso("2024-01-01T12:00:00.1234567+05:30").unwrap();
            assert_eq!(rows, vec![(expected.utc().ticks(), 330); 2]);
            drop(query);
            assert!(
                session
                    .batch_response(
                        "ALTER TABLE dbo.offset_values ALTER COLUMN d DATETIMEOFFSET(3)",
                        &Default::default(),
                        false,
                        None
                    )
                    .1
            );
            assert!(
                session
                    .batch_response(
                        "INSERT INTO dbo.offset_values DEFAULT VALUES",
                        &Default::default(),
                        false,
                        None
                    )
                    .1
            );
            let rows:i64=session.db.query_row("SELECT count(*) FROM dbo.offset_values WHERE d.__msduck_offset_minutes=330 AND d.__msduck_datetimeoffset_3=?",[expected.round(3).unwrap().utc().ticks()],|r|r.get(0)).unwrap();
            assert_eq!(rows, 3);
        }
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn datetimeoffset_vectors_retain_offsets_scales_and_nulls() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        register(&db).unwrap();
        let expected = DateTimeOffset::parse_iso("2024-02-29T12:34:56.1234567-05:30").unwrap();
        for scale in 0..=7 {
            let sql = format!(
                "SELECT v.__msduck_datetimeoffset_{scale}, v.__msduck_offset_minutes FROM (SELECT __msduck_datetimeoffset_cast_{scale}(__msduck_datetimeoffset_try_7(CASE WHEN i%17=0 THEN NULL WHEN i%19=0 THEN 'bad' ELSE '2024-02-29T12:34:56.1234567-05:30' END)) v FROM range(6000) r(i))"
            );
            let mut statement = db.prepare(&sql).unwrap();
            let rows = statement
                .query_map([], |r| {
                    Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, Option<i16>>(1)?))
                })
                .unwrap()
                .collect::<duckdb::Result<Vec<_>>>()
                .unwrap();
            assert_eq!(rows.len(), 6000);
            for (i, row) in rows.into_iter().enumerate() {
                assert_eq!(
                    row,
                    if i % 17 == 0 || i % 19 == 0 {
                        (None, None)
                    } else {
                        (
                            Some(expected.round(scale).unwrap().utc().ticks()),
                            Some(-330),
                        )
                    }
                );
            }
        }
        db.execute_batch("CREATE SEQUENCE dto_calls START 1")
            .unwrap();
        let count:i64=db.query_row("SELECT count(__msduck_datetimeoffset_cast_7(printf('2024-01-01T00:00:%02d+05:30',nextval('dto_calls')%60))) FROM range(6000)",[],|r|r.get(0)).unwrap();
        assert_eq!(count, 6000);
        assert_eq!(
            db.query_row("SELECT currval('dto_calls')", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            6000
        );
    }
}
