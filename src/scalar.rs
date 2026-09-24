//! Native scalar functions preserve single evaluation and work in stored defaults.
use duckdb::{
    Connection,
    core::{DataChunkHandle, LogicalTypeHandle, LogicalTypeId},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};

pub fn register(db: &Connection) -> duckdb::Result<()> {
    crate::money_range::register(db)?;
    crate::money_arithmetic::register(db)?;
    crate::money_format::register(db)?;
    crate::identity_metadata::register(db)?;
    crate::variant::register(db)?;
    crate::variant_pack::register(db)?;
    crate::variant_extreme::register(db)?;
    crate::datetime2_cast::register(db)?;
    crate::datetimeoffset_cast::register(db)?;
    crate::switchoffset::register(db)?;
    crate::datetime2_date::register(db)?;
    crate::percentile_input::register(db)?;
    db.register_scalar_function::<crate::ntile::Buckets>("__msduck_ntile_count")?;
    db.register_scalar_function::<crate::value_window::Offset>("__msduck_lag_offset")?;
    crate::integer_aggregate::register(db)?;
    crate::character_aggregate::register(db)?;
    crate::ansi_binary::register(db)?;
    crate::decimal_aggregate::register(db)?;
    crate::decimal_division::register(db)?;
    crate::replicate::register(db)?;
    crate::unicode_carrier::register(db)?;
    crate::unicode_case::register(db)?;
    crate::unicode_trim::register(db)?;
    db.register_scalar_function::<crate::aggregate::SumResult<false>>("__msduck_sum_int_result")?;
    db.register_scalar_function::<crate::aggregate::SumResult<true>>("__msduck_sum_big_result")?;
    crate::calendar_parts::register(db)?;
    crate::datepart::register(db)?;
    crate::dateadd::register(db)?;
    crate::isjson::register(db)?;
    crate::json_extract::register(db)?;
    db.register_scalar_function::<crate::string_escape::Escape>("__msduck_string_escape")?;
    crate::openjson::register(db)?;
    crate::for_json::register(db)?;
    crate::character_storage::register(db)?;
    crate::timefromparts::register(db)?;
    crate::datetime2fromparts::register(db)?;
    crate::datetimeoffsetfromparts::register(db)?;
    crate::datediff::register(db)?;
    crate::nvarchar::register(db)?;
    crate::varchar::register(db)?;
    db.execute_batch("CREATE OR REPLACE MACRO main.__msduck_nchar_width(value,width) AS __msduck_nvarchar_width(value || repeat(' ',width),width)")?;
    crate::eomonth::register(db)?;
    db.execute_batch(
        "CREATE OR REPLACE MACRO main.__msduck_char_width(value, width) AS rpad(value, width, ' ');
        CREATE OR REPLACE MACRO main.__msduck_varchar_width(value, width) AS left(value, width)",
    )?;
    db.register_scalar_function::<crate::ncharacter::NCharacter>("__msduck_nchar")?;
    db.register_scalar_function::<crate::character::Character>("__msduck_char")?;
    db.register_scalar_function::<crate::space::Space>("__msduck_space")?;
    db.register_scalar_function::<crate::unicode::Unicode>("__msduck_unicode")?;
    db.execute_batch("CREATE OR REPLACE MACRO main.__msduck_abs(value) AS abs(value)")?;
    db.execute_batch("CREATE OR REPLACE MACRO main.__msduck_ceiling(value) AS ceiling(value)")?;
    db.execute_batch("CREATE OR REPLACE MACRO main.__msduck_floor(value) AS floor(value)")?;
    db.execute_batch("CREATE OR REPLACE MACRO main.__msduck_sign(value) AS sign(value)")?;
    db.register_scalar_function::<crate::bitwise::Binary<0>>("__msduck_bitand")?;
    db.register_scalar_function::<crate::bitwise::Binary<1>>("__msduck_bitor")?;
    db.register_scalar_function::<crate::bitwise::Binary<2>>("__msduck_bitxor")?;
    db.register_scalar_function::<BitwiseNot>("__msduck_bitnot")?;
    db.register_scalar_function::<RowCount<0>>("__msduck_top_count")?;
    db.register_scalar_function::<RowCount<1>>("__msduck_offset_count")?;
    db.register_scalar_function::<RowCount<2>>("__msduck_fetch_count")?;
    crate::integer_conversion::register(db)?;
    crate::variant_cast::register(db)?;
    db.register_scalar_function::<DateFromParts>("__msduck_datefromparts")?;
    db.execute_batch(
        "CREATE OR REPLACE MACRO main.__msduck_isnull(first_value, replacement) AS
        coalesce(first_value, CASE
        WHEN typeof(first_value) IN ('UTINYINT','SMALLINT','INTEGER','BIGINT') THEN
            cast_to_type(__msduck_integer_input(replacement, typeof(first_value)), first_value)
        ELSE cast_to_type(replacement, first_value) END)",
    )?;
    db.register_scalar_function::<crate::datalength::Bytes<0>>("__msduck_datalength_ansi")?;
    db.register_scalar_function::<crate::datalength::Bytes<1>>("__msduck_datalength_unicode")?;
    db.register_scalar_function::<crate::datalength::Bytes<2>>("__msduck_datalength_binary")?;
    db.register_scalar_function::<Length<false>>("__msduck_text_len")?;
    db.register_scalar_function::<Length<true>>("__msduck_binary_len")?;
    // Defaults retain this name, so the dispatch macro must be shared across
    // connections and available after reopening. typeof resolves at binding,
    // leaving one native call and one evaluation of the input.
    db.execute_batch(
        "CREATE OR REPLACE MACRO main.__msduck_len(value) AS CASE
        WHEN typeof(value)='BLOB' THEN __msduck_binary_len(CAST(value AS BLOB))
        ELSE __msduck_text_len(CAST(value AS VARCHAR)) END",
    )
}

struct Length<const BINARY: bool>;
struct BitwiseNot;
impl VScalar for BitwiseNot {
    type State = ();
    fn invoke(
        _: &Self::State,
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let source = input.flat_vector(0);
        let kind = source.logical_type().id();
        let mut result = output.flat_vector();
        for index in 0..len {
            if source.row_is_null(index as u64) {
                result.set_null(index);
                continue;
            }
            // Each overload has identical input/output storage, and only live,
            // non-null slots within this chunk are accessed.
            macro_rules! invert {
                ($kind:ty) => {
                    unsafe {
                        result.as_mut_slice_with_len::<$kind>(len)[index] =
                            !source.as_slice_with_len::<$kind>(len)[index];
                    }
                };
            }
            match kind {
                LogicalTypeId::Boolean => invert!(bool),
                LogicalTypeId::UTinyint => invert!(u8),
                LogicalTypeId::Smallint => invert!(i16),
                LogicalTypeId::Integer => invert!(i32),
                LogicalTypeId::Bigint => invert!(i64),
                _ => return Err("unsupported bitwise NOT input".into()),
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        [
            LogicalTypeId::Boolean,
            LogicalTypeId::UTinyint,
            LogicalTypeId::Smallint,
            LogicalTypeId::Integer,
            LogicalTypeId::Bigint,
        ]
        .into_iter()
        .map(|kind| {
            ScalarFunctionSignature::exact(
                vec![LogicalTypeHandle::from(kind)],
                LogicalTypeHandle::from(kind),
            )
        })
        .collect()
    }
}

struct RowCount<const MODE: u8>;
impl<const MODE: u8> VScalar for RowCount<MODE> {
    type State = ();

    fn invoke(
        _: &Self::State,
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let source = input.flat_vector(0);
        let mut result = output.flat_vector();
        for index in 0..len {
            if source.row_is_null(index as u64) {
                return Err("A TOP or FETCH clause contains an invalid value.".into());
            }
            let count = unsafe { source.as_slice_with_len::<i64>(len)[index] };
            if count < 0 || (MODE == 2 && count == 0) {
                return Err(match MODE {
                    1 => "The offset specified in a OFFSET clause may not be negative.",
                    2 => {
                        "The number of rows provided for a FETCH clause must be greater then zero."
                    }
                    _ => "A TOP or FETCH clause contains an invalid value.",
                }
                .into());
            }
            unsafe {
                result.as_mut_slice_with_len::<i64>(len)[index] = count;
            }
        }
        Ok(())
    }

    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![LogicalTypeHandle::from(LogicalTypeId::Bigint)],
            LogicalTypeHandle::from(LogicalTypeId::Bigint),
        )]
    }
}

impl<const BINARY: bool> VScalar for Length<BINARY> {
    type State = ();

    fn invoke(
        _: &Self::State,
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
            // VARCHAR and BLOB both use duckdb_string_t. Read only initialized,
            // non-null slots, and keep the copied inline storage alive while
            // borrowing its bytes. Heap storage remains owned by this chunk.
            let mut value =
                unsafe { source.as_slice_with_len::<duckdb::ffi::duckdb_string_t>(len)[index] };
            let bytes = unsafe {
                let size = duckdb::ffi::duckdb_string_t_length(value) as usize;
                let data = duckdb::ffi::duckdb_string_t_data(&mut value).cast::<u8>();
                std::slice::from_raw_parts(data, size)
            };
            let length = if BINARY {
                bytes
                    .iter()
                    .rposition(|byte| *byte != b' ')
                    .map_or(0, |last| last + 1)
            } else {
                std::str::from_utf8(bytes)?
                    .trim_end_matches(' ')
                    .encode_utf16()
                    .count()
            };
            // BIGINT physical output; the translator narrows known non-MAX
            // calls to INT. No mutable slice survives a validity operation.
            unsafe {
                result.as_mut_slice_with_len::<i64>(len)[index] = i64::try_from(length)?;
            }
        }
        Ok(())
    }

    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![LogicalTypeHandle::from(if BINARY {
                LogicalTypeId::Blob
            } else {
                LogicalTypeId::Varchar
            })],
            LogicalTypeHandle::from(LogicalTypeId::Bigint),
        )]
    }
}

const INVALID_DATE: &str =
    "Cannot construct data type date, some of the arguments have values which are not valid.";

pub(crate) fn date_days(year: i32, month: i32, day: i32) -> Result<i32, &'static str> {
    if !(1..=9999).contains(&year) || !(1..=12).contains(&month) {
        return Err(INVALID_DATE);
    }
    let leap = year % 400 == 0 || (year % 4 == 0 && year % 100 != 0);
    let lengths = [
        31,
        28 + i32::from(leap),
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if !(1..=lengths[month as usize - 1]).contains(&day) {
        return Err(INVALID_DATE);
    }
    let previous = year - 1;
    let before_year = previous * 365 + previous / 4 - previous / 100 + previous / 400;
    Ok(before_year + lengths[..month as usize - 1].iter().sum::<i32>() + day - 1 - 719162)
}

struct DateFromParts;
impl VScalar for DateFromParts {
    type State = ();

    fn invoke(
        _: &Self::State,
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let year = input.flat_vector(0);
        let month = input.flat_vector(1);
        let day = input.flat_vector(2);
        // The registered signature guarantees three INTEGER vectors. All slices
        // stay within this chunk and are discarded before returning to DuckDB.
        let (years, months, days) = unsafe {
            (
                year.as_slice_with_len::<i32>(len),
                month.as_slice_with_len::<i32>(len),
                day.as_slice_with_len::<i32>(len),
            )
        };
        let mut result = output.flat_vector();
        for index in 0..len {
            if year.row_is_null(index as u64)
                || month.row_is_null(index as u64)
                || day.row_is_null(index as u64)
            {
                result.set_null(index);
            } else {
                let value = date_days(years[index], months[index], days[index])?;
                // DATE has i32 physical storage (days since 1970-01-01).
                // No overlapping writable slice is held while setting validity.
                unsafe {
                    result.as_mut_slice_with_len::<i32>(len)[index] = value;
                }
            }
        }
        Ok(())
    }

    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            (0..3)
                .map(|_| LogicalTypeHandle::from(LogicalTypeId::Integer))
                .collect(),
            LogicalTypeHandle::from(LogicalTypeId::Date),
        )]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitwise_not_handles_nulls_and_widths_across_chunks() {
        let db = Connection::open_in_memory().unwrap();
        register(&db).unwrap();
        let wrong: i64 = db.query_row(
            "SELECT COUNT(*) FROM (SELECT CASE WHEN n%17=0 THEN NULL ELSE n END AS n FROM range(5000) r(n)) s
             WHERE __msduck_bitnot(CAST(n%256 AS UTINYINT)) IS DISTINCT FROM CAST(255-n%256 AS UTINYINT)
                OR __msduck_bitnot(CAST(n AS SMALLINT)) IS DISTINCT FROM CAST(-n-1 AS SMALLINT)
                OR __msduck_bitnot(CAST(n AS INTEGER)) IS DISTINCT FROM CAST(-n-1 AS INTEGER)
                OR __msduck_bitnot(n) IS DISTINCT FROM -n-1
                OR __msduck_bitnot(CAST(n%2 AS BOOLEAN)) IS DISTINCT FROM (n%2=0)",
            [], |row| row.get(0)).unwrap();
        assert_eq!(wrong, 0);
    }

    #[test]
    fn length_handles_nulls_and_heap_strings_across_chunks() {
        let db = Connection::open_in_memory().unwrap();
        register(&db).unwrap();
        let mismatches: i64 = db.query_row(
            "SELECT count(*) FROM range(6000) t(i) WHERE __msduck_text_len(CASE WHEN i%3=0 THEN NULL ELSE repeat('🦆', CAST(i%100 AS INT)) || '  ' END) IS DISTINCT FROM CASE WHEN i%3=0 THEN NULL ELSE 2*(i%100) END",
            [], |row| row.get(0)).unwrap();
        assert_eq!(mismatches, 0);
        let mut bytes = (0u8..=255).collect::<Vec<_>>();
        bytes.extend_from_slice(b"  ");
        let length: i64 = db
            .query_row("SELECT __msduck_binary_len(?)", [bytes], |row| row.get(0))
            .unwrap();
        assert_eq!(length, 256);
    }

    #[test]
    fn date_constructor_matches_duckdb_across_calendar_and_vector_boundaries() {
        let db = Connection::open_in_memory().unwrap();
        register(&db).unwrap();
        let mismatches: i64 = db.query_row(
            "SELECT count(*) FROM (SELECT CAST(value AS DATE) AS d FROM generate_series(DATE '0001-01-01', DATE '9999-12-31', INTERVAL 1 DAY) AS t(value)) WHERE __msduck_datefromparts(CAST(year(d) AS INT), CAST(month(d) AS INT), CAST(day(d) AS INT)) <> d", [], |row| row.get(0)).unwrap();
        assert_eq!(mismatches, 0);
    }
}
