//! Attach or change fixed offsets using exact temporal ticks.
use duckdb::{
    core::{DataChunkHandle, LogicalTypeHandle, LogicalTypeId as Id},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use sqlparser::ast::*;
pub const INVALID: &str = "The timezone provided to builtin function switchoffset is invalid.";
pub const OVERFLOW: &str = "The timezone provided to builtin function switchoffset would cause the datetimeoffset to overflow the range of valid date range in either UTC or local time.";
pub const ATTACH_INVALID: &str =
    "The timezone provided to builtin function todatetimeoffset is invalid.";
pub const ATTACH_OVERFLOW: &str = "The timezone provided to builtin function todatetimeoffset would cause the datetimeoffset to overflow the range of valid date range in either UTC or local time.";
pub fn lower(
    expr: &mut Expr,
    parameters: &std::collections::HashMap<String, crate::parameter::Parameter>,
) -> Result<(), String> {
    for (name, attach) in [("SWITCHOFFSET", false), ("TODATETIMEOFFSET", true)] {
        if let Expr::Function(f) = expr
            && let Some([value, offset]) = crate::isnull::binary_args(f, name)?
        {
            let scale = if attach {
                crate::datetime2_compare::scale(value, parameters)
            } else {
                crate::datetimeoffset_compare::scale(value, parameters)
            }
            .unwrap_or(7);
            *expr = crate::engine::binary_function(
                &format!("__msduck_{}_{scale}", name.to_ascii_lowercase()),
                if attach {
                    crate::datetime2_cast::convert(value.clone(), scale)
                } else {
                    crate::datetimeoffset_cast::convert(value.clone(), scale)
                },
                offset.clone(),
            );
            break;
        }
    }
    Ok(())
}
fn text_offset(text: &str) -> Result<i16, &'static str> {
    let b = text.trim().as_bytes();
    if b.len() != 6
        || !matches!(b[0], b'+' | b'-')
        || b[3] != b':'
        || ![b[1], b[2], b[4], b[5]].iter().all(u8::is_ascii_digit)
    {
        return Err(INVALID);
    }
    let h = i16::from(b[1] - b'0') * 10 + i16::from(b[2] - b'0');
    let m = i16::from(b[4] - b'0') * 10 + i16::from(b[5] - b'0');
    if m >= 60 || h > 14 || (h == 14 && m != 0) {
        return Err(INVALID);
    }
    Ok((h * 60 + m) * if b[0] == b'-' { -1 } else { 1 })
}
fn kind(scale: u8) -> LogicalTypeHandle {
    LogicalTypeHandle::struct_type(&[
        (
            &format!("__msduck_datetimeoffset_{scale}"),
            Id::Bigint.into(),
        ),
        ("__msduck_offset_minutes", Id::Smallint.into()),
    ])
}
struct Switch<const SCALE: u8, const ATTACH: bool = false>;
impl<const SCALE: u8, const ATTACH: bool> VScalar for Switch<SCALE, ATTACH> {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let invalid = if ATTACH { ATTACH_INVALID } else { INVALID };
        let overflow = if ATTACH { ATTACH_OVERFLOW } else { OVERFLOW };
        let len = input.len();
        let source = input.flat_vector(0);
        let zone = input.flat_vector(1);
        let zone_type = zone.logical_type().id();
        if !matches!(
            zone_type,
            Id::Varchar
                | Id::Tinyint
                | Id::UTinyint
                | Id::Smallint
                | Id::Integer
                | Id::Bigint
                | Id::SqlNull
        ) {
            return Err("unsupported timezone input type".into());
        }
        let structure = input.struct_vector(0);
        let ticks = structure.child(0, len);
        let old_offset = (!ATTACH).then(|| structure.child(1, len));
        let mut result = output.struct_vector();
        let mut utc_result = result.child(0, len);
        let mut zone_result = result.child(1, len);
        for row in 0..len {
            if source.row_is_null(row as u64) || zone.row_is_null(row as u64) {
                result.set_null(row);
                utc_result.set_null(row);
                zone_result.set_null(row);
                continue;
            }
            if ticks.row_is_null(row as u64)
                || old_offset
                    .as_ref()
                    .is_some_and(|v| v.row_is_null(row as u64))
            {
                return Err("invalid DATETIMEOFFSET payload".into());
            }
            let minutes = unsafe {
                match zone_type {
                    Id::Varchar => {
                        let mut s =
                            zone.as_slice_with_len::<duckdb::ffi::duckdb_string_t>(len)[row];
                        let bytes = std::slice::from_raw_parts(
                            duckdb::ffi::duckdb_string_t_data(&mut s).cast::<u8>(),
                            duckdb::ffi::duckdb_string_t_length(s) as usize,
                        );
                        i64::from(
                            text_offset(std::str::from_utf8(bytes).map_err(|_| invalid)?)
                                .map_err(|_| invalid)?,
                        )
                    }
                    Id::Tinyint => i64::from(zone.as_slice_with_len::<i8>(len)[row]),
                    Id::UTinyint => i64::from(zone.as_slice_with_len::<u8>(len)[row]),
                    Id::Smallint => i64::from(zone.as_slice_with_len::<i16>(len)[row]),
                    Id::Integer => i64::from(zone.as_slice_with_len::<i32>(len)[row]),
                    Id::Bigint => zone.as_slice_with_len::<i64>(len)[row],
                    _ => return Err(invalid.into()),
                }
            };
            if !(-840..=840).contains(&minutes) {
                return Err(invalid.into());
            }
            let utc = crate::datetime2::DateTime2::from_ticks(unsafe {
                ticks.as_slice_with_len::<i64>(len)[row]
            })?;
            let utc = if let Some(old_offset) = &old_offset {
                crate::datetimeoffset::DateTimeOffset::from_utc(utc, unsafe {
                    old_offset.as_slice_with_len::<i16>(len)[row]
                })?;
                crate::datetimeoffset::DateTimeOffset::from_utc(utc, minutes as i16)
                    .map_err(|_| overflow)?
                    .utc()
            } else {
                crate::datetimeoffset::DateTimeOffset::from_local(utc, minutes as i16)
                    .map_err(|_| overflow)?
                    .utc()
            };
            unsafe {
                utc_result.as_mut_slice_with_len::<i64>(len)[row] = utc.ticks();
                zone_result.as_mut_slice_with_len::<i16>(len)[row] = minutes as i16;
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![
                if ATTACH {
                    LogicalTypeHandle::struct_type(&[(
                        &format!("__msduck_datetime2_{SCALE}"),
                        Id::Bigint.into(),
                    )])
                } else {
                    kind(SCALE)
                },
                Id::Any.into(),
            ],
            kind(SCALE),
        )]
    }
}
pub fn register(db: &duckdb::Connection) -> duckdb::Result<()> {
    macro_rules! register {($($s:literal),*)=>{$(db.register_scalar_function::<Switch<$s>>(concat!("__msduck_switchoffset_",stringify!($s)))?; db.register_scalar_function::<Switch<$s,true>>(concat!("__msduck_todatetimeoffset_",stringify!($s)))?;)*};}
    register!(0, 1, 2, 3, 4, 5, 6, 7);
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn attaching_offsets_retains_local_ticks_and_evaluates_once() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        let base = crate::datetime2::DateTime2::parse_iso("2024-01-01T00:15:30.1234567").unwrap();
        for scale in 0..=7 {
            let local = base.round(scale).unwrap().ticks();
            let sql = format!(
                "WITH v AS MATERIALIZED (SELECT i,__msduck_todatetimeoffset_{scale}(__msduck_datetime2_cast_{scale}(CASE WHEN i%17=0 THEN NULL ELSE '2024-01-01T00:15:30.1234567' END),CAST(CASE WHEN i%19=0 THEN NULL ELSE i%1681-840 END AS INT)) d FROM range(6000) r(i)) SELECT count(*) FROM v WHERE d.__msduck_datetimeoffset_{scale} IS DISTINCT FROM CASE WHEN i%17=0 OR i%19=0 THEN NULL ELSE {local}-(i%1681-840)*600000000 END OR d.__msduck_offset_minutes IS DISTINCT FROM CASE WHEN i%17=0 OR i%19=0 THEN NULL ELSE i%1681-840 END"
            );
            assert_eq!(db.query_row(&sql, [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        }
        db.execute_batch(
            "CREATE SEQUENCE attach_input START 1; CREATE SEQUENCE attach_zone START 1",
        )
        .unwrap();
        assert_eq!(db.query_row("SELECT count(__msduck_todatetimeoffset_7(__msduck_datetime2_cast_7(printf('2024-01-01T00:00:%02d',nextval('attach_input')%60)),CAST(nextval('attach_zone')%1681-840 AS INT))) FROM range(6000)",[],|r|r.get::<_,i64>(0)).unwrap(),6000);
        for sequence in ["attach_input", "attach_zone"] {
            assert_eq!(
                db.query_row("SELECT currval(?)", [sequence], |r| r.get::<_, i64>(0))
                    .unwrap(),
                6000
            );
        }
    }

    #[test]
    fn utc_ticks_offsets_nulls_and_single_evaluation_across_chunks() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        let base =
            crate::datetimeoffset::DateTimeOffset::parse_iso("2024-01-01T00:15:30.1234567+14:00")
                .unwrap();
        for scale in 0..=7 {
            let utc = base.round(scale).unwrap().utc().ticks();
            let sql = format!(
                "WITH v AS MATERIALIZED (SELECT i,__msduck_switchoffset_{scale}(__msduck_datetimeoffset_cast_{scale}(CASE WHEN i%17=0 THEN NULL ELSE '2024-01-01T00:15:30.1234567+14:00' END),CAST(CASE WHEN i%19=0 THEN NULL ELSE i%1681-840 END AS INT)) d FROM range(6000) r(i)) SELECT count(*) FROM v WHERE d.__msduck_datetimeoffset_{scale} IS DISTINCT FROM CASE WHEN i%17=0 OR i%19=0 THEN NULL ELSE {utc} END OR d.__msduck_offset_minutes IS DISTINCT FROM CASE WHEN i%17=0 OR i%19=0 THEN NULL ELSE i%1681-840 END"
            );
            assert_eq!(db.query_row(&sql, [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        }
        db.execute_batch(
            "CREATE SEQUENCE switch_input START 1; CREATE SEQUENCE switch_zone START 1",
        )
        .unwrap();
        let count:i64=db.query_row("SELECT count(__msduck_switchoffset_7(__msduck_datetimeoffset_cast_7(printf('2024-01-01T00:00:%02dZ',nextval('switch_input')%60)),CAST(nextval('switch_zone')%1681-840 AS INT))) FROM range(6000)",[],|r|r.get(0)).unwrap();
        assert_eq!(count, 6000);
        for sequence in ["switch_input", "switch_zone"] {
            assert_eq!(
                db.query_row("SELECT currval(?)", [sequence], |r| r.get::<_, i64>(0))
                    .unwrap(),
                6000
            );
        }
    }
}
