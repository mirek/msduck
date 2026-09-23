//! YEAR/MONTH/DAY typing and integer day offsets from SQL Server's base date.
use duckdb::{
    core::{DataChunkHandle, Inserter, LogicalTypeId},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use sqlparser::ast::*;

pub const OVERFLOW: &str = "Arithmetic overflow error converting expression to data type datetime.";

pub use msduck_sql::expression_metadata::temporal::calendar_argument as argument;

pub fn lower(expr: &mut Expr) -> Result<(), String> {
    if let Expr::Function(function) = expr
        && let Some((name, value)) = argument(function)?
    {
        *expr = crate::engine::unary_function(
            &format!("__msduck_{}", name.to_ascii_lowercase()),
            value.clone(),
        );
    }
    Ok(())
}

pub fn register(db: &duckdb::Connection) -> duckdb::Result<()> {
    db.register_scalar_function::<IntegerDate>("__msduck_integer_date")?;
    db.register_scalar_function::<CalendarText>("__msduck_calendar_text")?;
    // typeof is resolved during binding. Only the selected arm evaluates the
    // input; text normalization and the DATE cast each consume that value once.
    db.execute_batch(&format!(
        "CREATE OR REPLACE MACRO main.__msduck_calendar_date(value) AS
        CASE WHEN {} THEN __msduck_datetime2_date(value)
        WHEN typeof(value) IN ('TINYINT','UTINYINT','SMALLINT','INTEGER','BIGINT')
            THEN __msduck_integer_date(CAST(CAST(value AS VARCHAR) AS BIGINT))
        ELSE CAST(__msduck_calendar_text(CAST(value AS VARCHAR)) AS DATE)
        END",
        crate::datetime2_date::type_condition()
    ))?;
    for name in ["year", "month", "day"] {
        db.execute_batch(&format!("CREATE OR REPLACE MACRO main.__msduck_{name}(value) AS CAST({name}(__msduck_calendar_date(value)) AS INTEGER)"))?;
    }
    Ok(())
}

// Recognize time-only input without invoking a second expression evaluation.
// Date/timestamp text passes unchanged to the backend DATE conversion.
fn time_only(value: &str) -> bool {
    let value = value.trim();
    let (value, twelve_hour) = if value
        .get(value.len().saturating_sub(2)..)
        .is_some_and(|suffix| {
            suffix.eq_ignore_ascii_case("am") || suffix.eq_ignore_ascii_case("pm")
        }) {
        (value[..value.len() - 2].trim_end(), true)
    } else {
        (value, false)
    };
    let parts: Vec<_> = value.split(':').collect();
    if !(2..=3).contains(&parts.len()) {
        return false;
    }
    let number = |s: &str| -> Option<u32> {
        if s.is_empty() || s.len() > 2 || !s.bytes().all(|b| b.is_ascii_digit()) {
            None
        } else {
            s.parse().ok()
        }
    };
    let Some(hour) = number(parts[0]) else {
        return false;
    };
    let Some(minute) = number(parts[1]) else {
        return false;
    };
    if minute > 59 || (twelve_hour && !(1..=12).contains(&hour)) || (!twelve_hour && hour > 23) {
        return false;
    }
    if parts.len() == 3 {
        let (seconds, fraction) = parts[2]
            .split_once('.')
            .map_or((parts[2], None), |(s, f)| (s, Some(f)));
        if number(seconds).is_none_or(|n| n > 59) {
            return false;
        }
        if fraction
            .is_some_and(|f| f.is_empty() || f.len() > 7 || !f.bytes().all(|b| b.is_ascii_digit()))
        {
            return false;
        }
    }
    true
}

struct CalendarText;
impl VScalar for CalendarText {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let source = input.flat_vector(0);
        let mut result = output.flat_vector();
        for index in 0..len {
            if source.row_is_null(index as u64) {
                result.set_null(index);
                continue;
            }
            // Copy inline storage before taking a pointer; heap data remains
            // owned by the input chunk. Only live VARCHAR slots are accessed.
            let mut value =
                unsafe { source.as_slice_with_len::<duckdb::ffi::duckdb_string_t>(len)[index] };
            let bytes = unsafe {
                std::slice::from_raw_parts(
                    duckdb::ffi::duckdb_string_t_data(&mut value).cast::<u8>(),
                    duckdb::ffi::duckdb_string_t_length(value) as usize,
                )
            };
            let value = std::str::from_utf8(bytes)?;
            result.insert(
                index,
                if time_only(value) {
                    "1900-01-01"
                } else {
                    value
                },
            );
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![LogicalTypeId::Varchar.into()],
            LogicalTypeId::Varchar.into(),
        )]
    }
}

struct IntegerDate;
impl VScalar for IntegerDate {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let source = input.flat_vector(0);
        let mut result = output.flat_vector();
        let base = crate::scalar::date_days(1900, 1, 1)?;
        let first = i64::from(crate::scalar::date_days(1753, 1, 1)? - base);
        let last = i64::from(crate::scalar::date_days(9999, 12, 31)? - base);
        for index in 0..len {
            if source.row_is_null(index as u64) {
                result.set_null(index);
                continue;
            }
            // Exact BIGINT signature; only valid slots in this chunk are read.
            let offset = unsafe { source.as_slice_with_len::<i64>(len)[index] };
            if !(first..=last).contains(&offset) {
                return Err(OVERFLOW.into());
            }
            // Range checked above; DATE stores i32 days from the Unix epoch.
            unsafe {
                result.as_mut_slice_with_len::<i32>(len)[index] = base + offset as i32;
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![LogicalTypeId::Bigint.into()],
            LogicalTypeId::Date.into(),
        )]
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn time_text_requires_valid_components() {
        for value in ["00:00", "23:59:59.1234567", "1:02:03 PM", " 12:00 am "] {
            assert!(super::time_only(value), "{value}");
        }
        for value in [
            "",
            "☃",
            "a🦆",
            "2024-01-01",
            "2024-01-01 12:34:56",
            "24:00",
            "12:60",
            "12:00:60",
            "0:00 PM",
            "12:00:00.",
            "12:00:00.12345678",
            "12:00:00x",
        ] {
            assert!(!super::time_only(value), "{value}");
        }
    }

    #[test]
    fn calendar_arguments_are_evaluated_once_per_row() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        for (name, expected) in [("year", 1900), ("month", 1), ("day", 1)] {
            db.execute_batch("CREATE OR REPLACE SEQUENCE evaluations START 1")
                .unwrap();
            let mut statement = db.prepare(&format!("SELECT __msduck_{name}(printf('%02d:00:00', nextval('evaluations')%24)) FROM range(6000)")).unwrap();
            let rows = statement.query_map([], |r| r.get::<_, i32>(0)).unwrap();
            for row in rows {
                assert_eq!(row.unwrap(), expected);
            }
            let count: i64 = db
                .query_row("SELECT currval('evaluations')", [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 6000, "{name} repeated its argument");
        }
    }

    #[test]
    fn integer_calendar_across_chunks_and_nulls() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        let wrong: i64 = db.query_row("SELECT count(*) FROM (SELECT CASE WHEN n%17=0 THEN NULL ELSE n-3000 END n FROM range(6000) r(n)) WHERE __msduck_integer_date(n) IS DISTINCT FROM DATE '1900-01-01' + CAST(n AS INT)", [], |r| r.get(0)).unwrap();
        assert_eq!(wrong, 0);
        for value in [i64::MIN, i64::MAX, -53691, 2958464] {
            assert!(
                db.query_row("SELECT __msduck_integer_date(?)", [value], |r| r
                    .get::<_, i32>(0))
                    .is_err()
            );
        }
    }
}
