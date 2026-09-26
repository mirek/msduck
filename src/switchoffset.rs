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
            let money = crate::engine::money_expr(offset, parameters);
            *expr = crate::engine::binary_function(
                &format!(
                    "__msduck_{}_{scale}{}",
                    name.to_ascii_lowercase(),
                    if money { "_money" } else { "" }
                ),
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
    let b = text.as_bytes();
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
fn floating_minutes(value: f64, invalid: &'static str) -> Result<i64, &'static str> {
    if !value.is_finite() || !(-841.0..841.0).contains(&value) {
        return Err(invalid);
    }
    Ok(value.trunc() as i64)
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
struct Switch<const SCALE: u8, const ATTACH: bool = false, const MONEY: bool = false>;
impl<const SCALE: u8, const ATTACH: bool, const MONEY: bool> VScalar
    for Switch<SCALE, ATTACH, MONEY>
{
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
        let logical = zone.logical_type();
        let zone_type = logical.id();
        let unicode = crate::unicode_carrier::is_logical(&logical);
        if !matches!(
            zone_type,
            Id::Varchar
                | Id::Boolean
                | Id::Tinyint
                | Id::UTinyint
                | Id::Smallint
                | Id::USmallint
                | Id::Integer
                | Id::UInteger
                | Id::Bigint
                | Id::UBigint
                | Id::Float
                | Id::Double
                | Id::Decimal
                | Id::SqlNull
        ) && !unicode
        {
            return Err(invalid.into());
        }
        let unicode_stored = unicode.then(|| input.struct_vector(1).child(0, len));
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
                    Id::Struct if unicode => {
                        let stored = unicode_stored.as_ref().ok_or(invalid)?;
                        if stored.row_is_null(row as u64) {
                            return Err("invalid Unicode carrier: NULL payload".into());
                        }
                        let bytes = crate::unicode_carrier::bytes(stored, row, len)?;
                        if bytes.len() != 12 {
                            return Err(invalid.into());
                        }
                        let units: Vec<_> = bytes
                            .chunks_exact(2)
                            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                            .collect();
                        let text = String::from_utf16(&units).map_err(|_| invalid)?;
                        i64::from(text_offset(&text).map_err(|_| invalid)?)
                    }
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
                    Id::Boolean => i64::from(zone.as_slice_with_len::<u8>(len)[row]),
                    Id::Tinyint => i64::from(zone.as_slice_with_len::<i8>(len)[row]),
                    Id::UTinyint => i64::from(zone.as_slice_with_len::<u8>(len)[row]),
                    Id::Smallint => i64::from(zone.as_slice_with_len::<i16>(len)[row]),
                    Id::USmallint => i64::from(zone.as_slice_with_len::<u16>(len)[row]),
                    Id::Integer => i64::from(zone.as_slice_with_len::<i32>(len)[row]),
                    Id::UInteger => i64::from(zone.as_slice_with_len::<u32>(len)[row]),
                    Id::Bigint => zone.as_slice_with_len::<i64>(len)[row],
                    Id::UBigint => i64::try_from(zone.as_slice_with_len::<u64>(len)[row])
                        .map_err(|_| invalid)?,
                    Id::Float => floating_minutes(
                        f64::from(zone.as_slice_with_len::<f32>(len)[row]),
                        invalid,
                    )?,
                    Id::Double => {
                        floating_minutes(zone.as_slice_with_len::<f64>(len)[row], invalid)?
                    }
                    Id::Decimal => {
                        let coefficient = match logical.decimal_width() {
                            1..=4 => i128::from(zone.as_slice_with_len::<i16>(len)[row]),
                            5..=9 => i128::from(zone.as_slice_with_len::<i32>(len)[row]),
                            10..=18 => i128::from(zone.as_slice_with_len::<i64>(len)[row]),
                            19..=38 => {
                                let v =
                                    zone.as_slice_with_len::<duckdb::ffi::duckdb_hugeint>(len)[row];
                                (i128::from(v.upper) << 64) | i128::from(v.lower)
                            }
                            _ => return Err(invalid.into()),
                        };
                        let divisor = 10_i128.pow(u32::from(logical.decimal_scale()));
                        let whole = if MONEY {
                            // MONEY converts to integer minutes by rounding ties away from zero.
                            let half = divisor / 2;
                            coefficient / divisor + i128::from(coefficient % divisor >= half)
                                - i128::from(coefficient % divisor <= -half)
                        } else {
                            coefficient / divisor
                        };
                        i64::try_from(whole).map_err(|_| invalid)?
                    }
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
    macro_rules! register {($($s:literal),*)=>{$(db.register_scalar_function::<Switch<$s>>(concat!("__msduck_switchoffset_",stringify!($s)))?; db.register_scalar_function::<Switch<$s,true>>(concat!("__msduck_todatetimeoffset_",stringify!($s)))?; db.register_scalar_function::<Switch<$s,false,true>>(concat!("__msduck_switchoffset_",stringify!($s),"_money"))?; db.register_scalar_function::<Switch<$s,true,true>>(concat!("__msduck_todatetimeoffset_",stringify!($s),"_money"))?;)*};}
    register!(0, 1, 2, 3, 4, 5, 6, 7);
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn captured_zone_coercions_use_source_type_and_exact_text() {
        assert_eq!(super::text_offset("+01:30"), Ok(90));
        assert!(super::text_offset(" +01:30 ").is_err());
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        let source = "__msduck_datetime2_cast_7('2024-01-01T00:00:00')";
        for (zone, expected) in [
            ("CAST(90.5 AS DECIMAL(8,1))", 90),
            ("CAST(-90.5 AS NUMERIC(8,1))", -90),
            ("CAST(90.5 AS DOUBLE)", 90),
            ("CAST(-90.5 AS FLOAT)", -90),
            ("CAST(1 AS BOOLEAN)", 1),
            ("CAST(90.5 AS DECIMAL(19,4))", 90),
        ] {
            let sql = format!(
                "SELECT (__msduck_todatetimeoffset_7({source},{zone})).__msduck_offset_minutes"
            );
            assert_eq!(
                db.query_row(&sql, [], |row| row.get::<_, i16>(0)).unwrap(),
                expected,
                "{zone}"
            );
        }
        let sql = format!(
            "SELECT (__msduck_todatetimeoffset_7_money({source},CAST(90.5 AS DECIMAL(19,4)))).__msduck_offset_minutes"
        );
        assert_eq!(
            db.query_row(&sql, [], |row| row.get::<_, i16>(0)).unwrap(),
            91
        );
        for zone in ["' +01:30 '", "'90'", "CAST(841 AS INT)"] {
            let sql = format!("SELECT __msduck_todatetimeoffset_7({source},{zone})");
            assert!(db.execute_batch(&sql).is_err(), "{zone}");
        }
    }

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
