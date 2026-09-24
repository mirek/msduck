//! Exact DATEADD arithmetic without a microsecond timestamp intermediate.
use crate::datetime2::DateTime2;
use duckdb::{
    core::{DataChunkHandle, LogicalTypeHandle, LogicalTypeId as Id},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
pub const OVERFLOW: &str = "Adding a value to a 'datetime2' column caused overflow.";
pub const OFFSET_OVERFLOW: &str = "Adding a value to a 'datetimeoffset' column caused overflow.";
const DAY: i64 = 864_000_000_000;

fn add(value: DateTime2, number: i32, part: usize, scale: u8) -> Result<DateTime2, &'static str> {
    let number = i64::from(number);
    let ticks = if part <= 4 {
        let days = (value.ticks() / DAY - 719162) as i32;
        let days = crate::dateadd::add(days, number as i32, part).map_err(|_| OVERFLOW)?;
        (i64::from(days) + 719162) * DAY + value.ticks() % DAY
    } else {
        let number = i128::from(number);
        let delta = match part {
            5 => number * 36_000_000_000,
            6 => number * 600_000_000,
            7 => number * 10_000_000,
            8 => number * 10_000,
            9 => number * 10,
            10 => (number + if number < 0 { -50 } else { 50 }) / 100,
            _ => return Err("invalid DATEADD datepart"),
        };
        i64::try_from(i128::from(value.ticks()) + delta).map_err(|_| OVERFLOW)?
    };
    DateTime2::from_ticks(ticks)
        .and_then(|v| v.round(scale))
        .map_err(|_| OVERFLOW)
}

struct Add<const SCALE: u8, const OFFSET: bool = false>;
fn kind(scale: u8, offset: bool) -> LogicalTypeHandle {
    if offset {
        return LogicalTypeHandle::struct_type(&[
            (
                &format!("__msduck_datetimeoffset_{scale}"),
                Id::Bigint.into(),
            ),
            ("__msduck_offset_minutes", Id::Smallint.into()),
        ]);
    }
    LogicalTypeHandle::struct_type(&[(&format!("__msduck_datetime2_{scale}"), Id::Bigint.into())])
}
impl<const SCALE: u8, const OFFSET: bool> VScalar for Add<SCALE, OFFSET> {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let number = input.flat_vector(0);
        let source = input.flat_vector(1);
        let structure = input.struct_vector(1);
        let ticks = structure.child(0, len);
        let offsets = OFFSET.then(|| structure.child(1, len));
        let part = input.flat_vector(2);
        let mut result = output.struct_vector();
        let mut child = result.child(0, len);
        let mut offset_result = OFFSET.then(|| result.child(1, len));
        for row in 0..len {
            if number.row_is_null(row as u64)
                || source.row_is_null(row as u64)
                || part.row_is_null(row as u64)
            {
                result.set_null(row);
                child.set_null(row);
                if let Some(offset_result) = &mut offset_result {
                    offset_result.set_null(row);
                }
                continue;
            }
            if ticks.row_is_null(row as u64) {
                return Err("invalid DATETIME2 ticks".into());
            }
            // Exact signatures fix storage widths; only bounded live slots are accessed.
            let (n, t, p) = unsafe {
                (
                    number.as_slice_with_len::<i32>(len)[row],
                    ticks.as_slice_with_len::<i64>(len)[row],
                    part.as_slice_with_len::<i32>(len)[row],
                )
            };
            let overflow = if OFFSET { OFFSET_OVERFLOW } else { OVERFLOW };
            let value = DateTime2::from_ticks(t).map_err(|_| overflow)?;
            let value = if let Some(offsets) = &offsets {
                if offsets.row_is_null(row as u64) {
                    return Err("invalid DATETIMEOFFSET offset".into());
                }
                let minutes = unsafe { offsets.as_slice_with_len::<i16>(len)[row] };
                let local = crate::datetimeoffset::DateTimeOffset::from_utc(value, minutes)
                    .map_err(|_| overflow)?
                    .local();
                let local = add(local, n, usize::try_from(p)?, SCALE).map_err(|_| overflow)?;
                let converted = crate::datetimeoffset::DateTimeOffset::from_local(local, minutes)
                    .map_err(|_| overflow)?;
                unsafe {
                    offset_result
                        .as_mut()
                        .unwrap()
                        .as_mut_slice_with_len::<i16>(len)[row] = minutes;
                }
                converted.utc()
            } else {
                add(value, n, usize::try_from(p)?, SCALE)?
            };
            unsafe {
                child.as_mut_slice_with_len::<i64>(len)[row] = value.ticks();
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![Id::Integer.into(), kind(SCALE, OFFSET), Id::Integer.into()],
            kind(SCALE, OFFSET),
        )]
    }
}
pub fn register(db: &duckdb::Connection) -> duckdb::Result<()> {
    macro_rules! register { ($($s:literal),*) => { $(
        db.register_scalar_function::<Add<$s>>(concat!("__msduck_datetime2_dateadd_", stringify!($s)))?;
        db.register_scalar_function::<Add<$s, true>>(concat!("__msduck_datetimeoffset_dateadd_", stringify!($s)))?;
    )* }; }
    register!(0, 1, 2, 3, 4, 5, 6, 7);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn offset_arithmetic_preserves_payloads_and_single_evaluation() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        for scale in 0..=7 {
            let base = crate::datetimeoffset::DateTimeOffset::parse_iso(
                "2024-01-01T00:15:30.1234567+14:00",
            )
            .unwrap()
            .round(scale)
            .unwrap()
            .utc()
            .ticks();
            let wrong: i64 = db.query_row(&format!("WITH v AS MATERIALIZED (SELECT i,__msduck_datetimeoffset_dateadd_{scale}(CAST(CASE WHEN i%17=0 THEN NULL ELSE i-3000 END AS INT),__msduck_datetimeoffset_cast_{scale}(CASE WHEN i%19=0 THEN NULL ELSE '2024-01-01T00:15:30.1234567+14:00' END),7) d FROM range(6000) r(i)) SELECT count(*) FROM v WHERE d.__msduck_datetimeoffset_{scale} IS DISTINCT FROM CASE WHEN i%17=0 OR i%19=0 THEN NULL ELSE {base}::BIGINT+(i-3000)*10000000 END OR d.__msduck_offset_minutes IS DISTINCT FROM CASE WHEN i%17=0 OR i%19=0 THEN NULL ELSE 840 END"), [], |r| r.get(0)).unwrap();
            assert_eq!(wrong, 0, "scale {scale}");
        }
        db.execute_batch(
            "CREATE SEQUENCE add_number_calls START 1; CREATE SEQUENCE add_value_calls START 1",
        )
        .unwrap();
        let count: i64 = db.query_row("SELECT count(__msduck_datetimeoffset_dateadd_7(CAST(nextval('add_number_calls') AS INT),__msduck_datetimeoffset_cast_7(CASE WHEN nextval('add_value_calls')%3=0 THEN NULL ELSE '2024-01-01T12:00:00-00:30' END),7)) FROM range(6000)", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 4000);
        for sequence in ["add_number_calls", "add_value_calls"] {
            let calls: i64 = db
                .query_row(&format!("SELECT currval('{sequence}')"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(calls, 6000);
        }
    }

    #[test]
    fn exact_ticks_and_checked_offsets() {
        let value = DateTime2::parse_iso("2024-01-01T00:00:00.1111111").unwrap();
        for (n, ticks) in [(49, 0), (50, 1), (150, 2), (-49, 0), (-50, -1), (-150, -2)] {
            assert_eq!(add(value, n, 10, 7).unwrap().ticks(), value.ticks() + ticks);
        }
        for part in 0..=5 {
            assert!(add(value, i32::MAX, part, 7).is_err());
            assert!(add(value, i32::MIN, part, 7).is_err());
        }
        let max = DateTime2::parse_iso("9999-12-31T23:59:59.9999999").unwrap();
        assert!(add(max, 50, 10, 7).is_err());
        assert!(add(DateTime2::from_ticks(0).unwrap(), -50, 10, 7).is_err());
        let half = DateTime2::parse_iso("2024-01-01T00:00:00").unwrap();
        assert_eq!(
            add(half, 500, 8, 0).unwrap().ticks(),
            half.ticks() + 10_000_000
        );
        assert_eq!(add(half, -500, 8, 0).unwrap().ticks(), half.ticks());
    }
    #[test]
    fn typed_nulls_and_precision_across_chunks() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::datetime2_cast::register(&db).unwrap();
        register(&db).unwrap();
        for scale in 0..=7 {
            let base = DateTime2::parse_iso("2024-01-01T12:34:56.1234567")
                .unwrap()
                .round(scale)
                .unwrap()
                .ticks();
            let sql = format!(
                "SELECT count(*) FROM (SELECT CAST(CASE WHEN n%17=0 THEN NULL ELSE n-3000 END AS INT) n, CASE WHEN n%19=0 THEN NULL ELSE __msduck_datetime2_cast_{scale}('2024-01-01T12:34:56.1234567') END d FROM range(6000) r(n)) WHERE (__msduck_datetime2_dateadd_{scale}(n,d,7)).__msduck_datetime2_{scale} IS DISTINCT FROM CASE WHEN d IS NULL THEN NULL ELSE {base}::BIGINT+n::BIGINT*10000000 END"
            );
            let wrong: i64 = db.query_row(&sql, [], |r| r.get(0)).unwrap();
            assert_eq!(wrong, 0, "scale {scale}");
        }
    }
}
