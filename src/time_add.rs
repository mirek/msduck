//! Exact TIME DATEADD with wraparound within a civil day.
use duckdb::{
    core::{DataChunkHandle, LogicalTypeId as Id},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
const DAY: i128 = 864_000_000_000;
fn add(nanos: i64, number: i32, part: i32, scale: u8) -> Result<i64, &'static str> {
    if !(0..86_400_000_000_000).contains(&nanos) {
        return Err("invalid TIME value");
    }
    let n = i128::from(number);
    let delta = match part {
        5 => n * 36_000_000_000,
        6 => n * 600_000_000,
        7 => n * 10_000_000,
        8 => n * 10_000,
        9 => n * 10,
        10 => (n + if n < 0 { -50 } else { 50 }) / 100,
        _ => return Err("invalid TIME DATEADD datepart"),
    };
    let ticks = (i128::from(nanos) / 100 + delta).rem_euclid(DAY);
    let quantum = 10i128.pow(7 - u32::from(scale));
    Ok((((ticks + quantum / 2) / quantum * quantum) % DAY * 100) as i64)
}
struct Add<const SCALE: u8>;
impl<const SCALE: u8> VScalar for Add<SCALE> {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let number = input.flat_vector(0);
        let value = input.flat_vector(1);
        let part = input.flat_vector(2);
        let mut result = output.flat_vector();
        for row in 0..len {
            if number.row_is_null(row as u64)
                || value.row_is_null(row as u64)
                || part.row_is_null(row as u64)
            {
                result.set_null(row);
                continue;
            }
            // Exact signatures establish all physical widths.
            unsafe {
                result.as_mut_slice_with_len::<i64>(len)[row] = add(
                    value.as_slice_with_len::<i64>(len)[row],
                    number.as_slice_with_len::<i32>(len)[row],
                    part.as_slice_with_len::<i32>(len)[row],
                    SCALE,
                )?;
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![Id::Integer.into(), Id::TimeNs.into(), Id::Integer.into()],
            Id::TimeNs.into(),
        )]
    }
}
pub fn register(db: &duckdb::Connection) -> duckdb::Result<()> {
    macro_rules! register { ($($s:literal),*) => { $(db.register_scalar_function::<Add<$s>>(concat!("__msduck_time_dateadd_",stringify!($s)))?;)* }; }
    register!(0, 1, 2, 3, 4, 5, 6, 7);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wraparound_precision_and_extreme_offsets() {
        assert_eq!(add(0, -1, 7, 7).unwrap(), 86_399_000_000_000);
        assert_eq!(add(0, -50, 10, 7).unwrap(), 86_399_999_999_900);
        assert_eq!(add(0, 50, 10, 7).unwrap(), 100);
        assert_eq!(add(86_399_999_999_900, 50, 10, 7).unwrap(), 0);
        assert_eq!(add(0, i32::MAX, 5, 7).unwrap(), 7 * 3_600_000_000_000);
        assert_eq!(add(0, i32::MIN, 5, 7).unwrap(), 16 * 3_600_000_000_000);
        for scale in 0..=7 {
            let quantum = 10i64.pow(9 - u32::from(scale));
            let expected = ((123_456_700 + quantum / 2) / quantum) * quantum;
            assert_eq!(add(123_456_700, 0, 7, scale).unwrap(), expected);
        }
    }
    #[test]
    fn chunks_preserve_nulls_and_single_evaluation() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        for scale in 0..=7 {
            let sql = format!(
                "SELECT DATEADD(second,CAST(CASE WHEN i%17=0 THEN NULL ELSE i-3000 END AS INT),CAST(CASE WHEN i%19=0 THEN NULL ELSE '00:00:00' END AS TIME({scale}))) t,i INTO dbo.time_add_{scale} FROM range(6000) r(i)"
            );
            assert!(
                session
                    .batch_response(&sql, &Default::default(), false, None)
                    .1
            );
            let wrong:i64=session.db.query_row(&format!("SELECT count(*) FROM dbo.time_add_{scale} WHERE epoch_ns(t) IS DISTINCT FROM CASE WHEN i%17=0 OR i%19=0 THEN NULL ELSE ((i-3000+86400)%86400)*1000000000 END"),[],|r|r.get(0)).unwrap();
            assert_eq!(wrong, 0);
        }
        session
            .db
            .execute_batch("CREATE SEQUENCE time_add_number; CREATE SEQUENCE time_add_value")
            .unwrap();
        assert!(session.batch_response("SELECT DATEADD(second,CAST(nextval('time_add_number') AS INT),CAST(CASE WHEN nextval('time_add_value')%3=0 THEN NULL ELSE '23:59:00' END AS TIME(3))) t INTO dbo.time_add_calls FROM range(6000)",&Default::default(),false,None).1);
        for name in ["time_add_number", "time_add_value"] {
            assert_eq!(
                session
                    .db
                    .query_row("SELECT currval(?)", [name], |r| r.get::<_, i64>(0))
                    .unwrap(),
                6000
            );
        }
    }
}
