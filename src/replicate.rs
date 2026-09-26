//! Native REPLICATE adapters. SQL lowering supplies converted VARCHAR/INTEGER
//! inputs and selects the source family/MAX variant. No expression is duplicated.
use duckdb::{
    core::{DataChunkHandle, Inserter, LogicalTypeId},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use msduck_core::character::{CharacterType, Family, Length};
use std::borrow::Cow;

// Allocation policy, separate from SQL's 8,000-byte bounded-result rule. The
// chunk allowance accommodates ordinary bounded results even when CP1252 values
// expand to three UTF-8 bytes per character. MAX results also have a cell cap.
const CHUNK_OUTPUT_LIMIT: usize = 64 * 1024 * 1024;
const CELL_OUTPUT_LIMIT: usize = 16 * 1024 * 1024;

// SQL Server's default implicit REAL/FLOAT-to-VARCHAR style retains six
// significant digits. It selects fixed notation after rounding for exponents
// -4..=5 and otherwise uses an exponent with at least three digits.
fn float_text<T: std::fmt::LowerExp + Copy + Into<f64>>(value: T) -> Result<String, &'static str> {
    let number: f64 = value.into();
    if !number.is_finite() {
        return Err("REPLICATE floating source is not finite");
    }
    if number == 0.0 {
        return Ok("0".into());
    }
    let scientific = format!("{value:.5e}");
    let (mantissa, exponent) = scientific
        .split_once('e')
        .ok_or("invalid floating conversion")?;
    let exponent: i32 = exponent.parse().map_err(|_| "invalid floating exponent")?;
    let (negative, mantissa) = if let Some(unsigned) = mantissa.strip_prefix('-') {
        (true, unsigned)
    } else {
        (false, mantissa)
    };
    let mut digits: String = mantissa.chars().filter(|ch| *ch != '.').collect();
    while digits.ends_with('0') && digits.len() > 1 {
        digits.pop();
    }
    let mut result = String::with_capacity(24);
    if negative {
        result.push('-');
    }
    if !(-4..=5).contains(&exponent) {
        result.push(digits.as_bytes()[0] as char);
        if digits.len() > 1 {
            result.push('.');
            result.push_str(&digits[1..]);
        }
        result.push('e');
        result.push_str(&format!("{exponent:+04}"));
    } else {
        let position = exponent + 1;
        if position <= 0 {
            result.push_str("0.");
            result.extend(std::iter::repeat_n('0', (-position) as usize));
            result.push_str(&digits);
        } else if position as usize >= digits.len() {
            result.push_str(&digits);
            result.extend(std::iter::repeat_n('0', position as usize - digits.len()));
        } else {
            result.push_str(&digits[..position as usize]);
            result.push('.');
            result.push_str(&digits[position as usize..]);
        }
    }
    Ok(result)
}

struct Replicate<
    const UNICODE: bool,
    const MAX: bool,
    const BUDGET: usize = CHUNK_OUTPUT_LIMIT,
    const BINARY: bool = false,
    const FLOAT_KIND: u8 = 0,
>;
impl<
    const UNICODE: bool,
    const MAX: bool,
    const BUDGET: usize,
    const BINARY: bool,
    const FLOAT_KIND: u8,
> VScalar for Replicate<UNICODE, MAX, BUDGET, BINARY, FLOAT_KIND>
{
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let strings = input.flat_vector(0);
        let counts = input.flat_vector(1);
        let mut result = output.flat_vector();
        let kind = CharacterType::new(
            if UNICODE {
                Family::Nvarchar
            } else {
                Family::Varchar
            },
            if MAX { Length::Max } else { Length::Bounded(1) },
        )?;
        let mut remaining = BUDGET;
        for row in 0..len {
            if strings.row_is_null(row as u64) || counts.row_is_null(row as u64) {
                result.set_null(row);
                continue;
            }
            // The exact VARCHAR/INTEGER signature fixes physical layouts. Read
            // only non-NULL slots inside this chunk; DuckDB owns the string bytes
            // for the duration of this invocation, including inline strings.
            let count = unsafe { counts.as_slice_with_len::<i32>(len)[row] };
            let text = if FLOAT_KIND == 1 {
                Cow::Owned(float_text(unsafe {
                    strings.as_slice_with_len::<f32>(len)[row]
                })?)
            } else if FLOAT_KIND == 2 {
                Cow::Owned(float_text(unsafe {
                    strings.as_slice_with_len::<f64>(len)[row]
                })?)
            } else {
                let mut source =
                    unsafe { strings.as_slice_with_len::<duckdb::ffi::duckdb_string_t>(len)[row] };
                let bytes = unsafe {
                    std::slice::from_raw_parts(
                        duckdb::ffi::duckdb_string_t_data(&mut source).cast::<u8>(),
                        duckdb::ffi::duckdb_string_t_length(source) as usize,
                    )
                };
                if BINARY {
                    if bytes.len() > CELL_OUTPUT_LIMIT {
                        return Err(
                            "REPLICATE binary source exceeds the configured input limit".into()
                        );
                    }
                    Cow::Owned(msduck_core::encoding::decode_cp1252(bytes))
                } else {
                    Cow::Borrowed(std::str::from_utf8(bytes)?)
                }
            };
            match msduck_core::replicate::plan(
                Some(text.as_ref()),
                Some(count),
                kind,
                remaining.min(CELL_OUTPUT_LIMIT),
            )? {
                None => result.set_null(row),
                Some(plan) => {
                    remaining -= plan.output_bytes();
                    result.insert(row, plan.render().as_str());
                }
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![
                if FLOAT_KIND == 1 {
                    LogicalTypeId::Float.into()
                } else if FLOAT_KIND == 2 {
                    LogicalTypeId::Double.into()
                } else if BINARY {
                    LogicalTypeId::Blob.into()
                } else {
                    LogicalTypeId::Varchar.into()
                },
                LogicalTypeId::Integer.into(),
            ],
            LogicalTypeId::Varchar.into(),
        )]
    }
}

pub fn register(db: &duckdb::Connection) -> duckdb::Result<()> {
    db.register_scalar_function::<Replicate<false, false>>("__msduck_replicate_varchar")?;
    db.register_scalar_function::<Replicate<true, false>>("__msduck_replicate_nvarchar")?;
    db.register_scalar_function::<Replicate<false, true>>("__msduck_replicate_varchar_max")?;
    db.register_scalar_function::<Replicate<true, true>>("__msduck_replicate_nvarchar_max")?;
    db.register_scalar_function::<Replicate<false, false, CHUNK_OUTPUT_LIMIT, true>>(
        "__msduck_replicate_binary",
    )?;
    db.register_scalar_function::<Replicate<false, true, CHUNK_OUTPUT_LIMIT, true>>(
        "__msduck_replicate_binary_max",
    )?;
    db.register_scalar_function::<Replicate<false, false, CHUNK_OUTPUT_LIMIT, false, 1>>(
        "__msduck_replicate_real",
    )?;
    db.register_scalar_function::<Replicate<false, false, CHUNK_OUTPUT_LIMIT, false, 2>>(
        "__msduck_replicate_float",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn captured_float_literals_match_sql_server_implicit_text() {
        let reference: serde_json::Value =
            serde_json::from_str(include_str!("../reference/replicate-float.json")).unwrap();
        let db = duckdb::Connection::open_in_memory().unwrap();
        register(&db).unwrap();
        let cases = reference["runs"][0].as_array().unwrap();
        let mut compared = 0;
        for case in cases {
            let name = case["name"].as_str().unwrap();
            if !name.contains(" literal ") {
                continue;
            }
            let sql = case["sql"].as_str().unwrap();
            let source = sql
                .strip_prefix("SELECT REPLICATE(")
                .unwrap()
                .split_once(",2) AS repeated")
                .unwrap()
                .0;
            let numeric = source
                .strip_prefix("CAST(")
                .unwrap()
                .split_once(" AS ")
                .unwrap()
                .0;
            // Bind exact IEEE values. DuckDB's decimal-literal-to-REAL cast
            // has a separate double-rounding boundary from the native text
            // conversion exercised here.
            let actual: Option<String> = if name.starts_with("REAL ") {
                let value = if numeric == "NULL" {
                    None
                } else {
                    Some(numeric.parse::<f32>().unwrap())
                };
                db.query_row(
                    "SELECT __msduck_replicate_real(?1,2)",
                    duckdb::params![value],
                    |row| row.get(0),
                )
                .unwrap()
            } else {
                let value = if numeric == "NULL" {
                    None
                } else {
                    Some(numeric.parse::<f64>().unwrap())
                };
                db.query_row(
                    "SELECT __msduck_replicate_float(?1,2)",
                    duckdb::params![value],
                    |row| row.get(0),
                )
                .unwrap()
            };
            let expected = case["result"]["sets"][0]["rows"][0][0]
                .as_str()
                .map(str::to_owned);
            assert_eq!(actual, expected, "{name}");
            compared += 1;
        }
        assert_eq!(compared, 46);
    }
    #[test]
    fn values_caps_nulls_heap_strings_and_unicode_cross_chunks() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        register(&db).unwrap();
        for (function, text, count, expected) in [
            (
                "__msduck_replicate_varchar",
                "abc",
                2667,
                "abc".repeat(2666),
            ),
            (
                "__msduck_replicate_nvarchar",
                "🦆x",
                1334,
                "🦆x".repeat(1333),
            ),
            ("__msduck_replicate_varchar", "€", 8100, "€".repeat(8000)),
            ("__msduck_replicate_nvarchar", "雪", 4100, "雪".repeat(4000)),
            (
                "__msduck_replicate_varchar_max",
                "x",
                8100,
                "x".repeat(8100),
            ),
            (
                "__msduck_replicate_nvarchar_max",
                "雪",
                4100,
                "雪".repeat(4100),
            ),
            ("__msduck_replicate_varchar", "x  ", 2, "x  x  ".into()),
            ("__msduck_replicate_varchar", "", i32::MAX, String::new()),
            ("__msduck_replicate_varchar", "\0€A", 2, "\0€A\0€A".into()),
        ] {
            let actual: String = db
                .query_row(
                    &format!("SELECT {function}(?, ?::INTEGER)"),
                    duckdb::params![text, count],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(actual, expected, "{function}");
        }
        let wrong: i64 = db.query_row("SELECT COUNT(*) FROM (SELECT CASE WHEN i%7=0 THEN NULL WHEN i%3=0 THEN 'a' ELSE 'long heap string €' END s,CAST(CASE WHEN i%11=0 THEN NULL ELSE i%13-2 END AS INTEGER) n FROM range(6000) r(i)) v WHERE __msduck_replicate_varchar(s,n) IS DISTINCT FROM CASE WHEN s IS NULL OR n IS NULL OR n<0 THEN NULL ELSE repeat(s,n) END", [], |row| row.get(0)).unwrap();
        assert_eq!(wrong, 0);
    }
    #[test]
    fn sql_lowering_retains_catalog_widths_and_nested_results() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        let (response, success) = session.batch_response("CREATE TABLE dbo.repeat_source(s VARCHAR(10), n INT); INSERT INTO dbo.repeat_source VALUES('x',3); SELECT REPLICATE(s,3) AS literal_count,REPLICATE(s,n) AS variable_count INTO dbo.repeat_result FROM dbo.repeat_source", &Default::default(), false, None);
        assert!(success, "{response:?}");
        let widths: Vec<(String,i32)> = session.db.prepare("SELECT name,CAST(max_length AS INTEGER) FROM sys.columns WHERE object_id=__msduck_object_id('dbo.repeat_result',NULL) ORDER BY column_id").unwrap().query_map([], |row| Ok((row.get(0)?,row.get(1)?))).unwrap().collect::<duckdb::Result<_>>().unwrap();
        assert_eq!(
            widths,
            [
                ("literal_count".into(), 30),
                ("variable_count".into(), 8000)
            ]
        );
        let values: (String, String) = session
            .db
            .query_row("SELECT * FROM dbo.repeat_result", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!(values, ("xxx".into(), "xxx".into()));
        let (response, success) = session.batch_response("WITH c AS (SELECT REPLICATE(N'雪',3) AS s) SELECT REPLICATE(s,2) AS s INTO dbo.repeat_nested FROM c", &Default::default(), false, None);
        assert!(success, "{response:?}");
        let text: String = session
            .db
            .query_row("SELECT s FROM dbo.repeat_nested", [], |row| row.get(0))
            .unwrap();
        assert_eq!(text, "雪".repeat(6));
    }
    #[test]
    fn inputs_are_evaluated_once_and_allocations_are_bounded() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        register(&db).unwrap();
        db.execute_batch("CREATE SEQUENCE source_seq; CREATE SEQUENCE count_seq START 2")
            .unwrap();
        let actual: String = db.query_row("SELECT __msduck_replicate_varchar(CAST(nextval('source_seq') AS VARCHAR),CAST(nextval('count_seq') AS INTEGER))", [], |row| row.get(0)).unwrap();
        assert_eq!(actual, "11");
        let counts: (i64, i64) = db
            .query_row(
                "SELECT currval('source_seq'),currval('count_seq')",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(counts, (1, 2));
        db.execute_batch("CREATE SEQUENCE real_source_seq; CREATE SEQUENCE real_count_seq START 2")
            .unwrap();
        let actual: String = db
            .query_row(
                "SELECT __msduck_replicate_real(CAST(nextval('real_source_seq') AS REAL),CAST(nextval('real_count_seq') AS INTEGER))",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(actual, "11");
        let counts: (i64, i64) = db
            .query_row(
                "SELECT currval('real_source_seq'),currval('real_count_seq')",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(counts, (1, 2));
        let error = db
            .query_row(
                "SELECT __msduck_replicate_nvarchar_max('x',2147483647)",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("REPLICATE result exceeds the configured output limit")
        );
        // Exercise the same cumulative-budget path with small allocations.
        db.register_scalar_function::<Replicate<false, true, 256>>("small_budget_repeat")
            .unwrap();
        let error = db.query_row("SELECT SUM(length(small_budget_repeat('x',CAST(i+100 AS INTEGER)))) FROM range(4) r(i)", [], |row| row.get::<_,i64>(0)).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("REPLICATE result exceeds the configured output limit")
        );
        let live: String = db
            .query_row("SELECT __msduck_replicate_varchar('ok',2)", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(live, "okok");
    }
}
