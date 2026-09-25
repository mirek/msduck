//! DATEPART extraction from exact temporal values with session DATEFIRST.
use duckdb::{
    core::{DataChunkHandle, FlatVector, Inserter, LogicalTypeId as Id},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use sqlparser::ast::*;

pub use msduck_sql::expression_metadata::datepart::named_args;

pub fn lower(expr: &mut Expr) -> Result<(), String> {
    if let Expr::Function(f) = expr {
        for name in ["datepart", "datename"] {
            if let Some((part, value)) = named_args(f, name)? {
                *expr = crate::engine::unary_function(
                    &format!("__msduck_{name}_dispatch_{part}"),
                    value.clone(),
                );
                break;
            }
        }
    }
    Ok(())
}
fn year_start(year: i32) -> i64 {
    let n = i64::from(year) - 1;
    n * 365 + n / 4 - n / 100 + n / 400
}
fn weeks(year: i32) -> i64 {
    let weekday = year_start(year) % 7;
    if weekday == 3 || (weekday == 2 && year_start(year + 1) - year_start(year) == 366) {
        53
    } else {
        52
    }
}
fn extract(value: crate::datetime2::DateTime2, part: usize, first: i64) -> i32 {
    let p = value.parts();
    let days = value.ticks() / 864_000_000_000;
    let year = i32::from(p.year);
    let start = year_start(year);
    let doy = days - start + 1;
    let result = match part {
        0 => i64::from(p.year),
        1 => i64::from((p.month - 1) / 3 + 1),
        2 => i64::from(p.month),
        3 => doy,
        4 => i64::from(p.day),
        5 => (doy - 1 + (start + 8 - first) % 7) / 7 + 1,
        6 => (days + 8 - first) % 7 + 1,
        7 => i64::from(p.hour),
        8 => i64::from(p.minute),
        9 => i64::from(p.second),
        10 => i64::from(p.fraction / 10_000),
        11 => i64::from(p.fraction / 10),
        12 => i64::from(p.fraction) * 100,
        13 => 0,
        14 => {
            let week = (doy - (days % 7 + 1) + 10) / 7;
            if week == 0 {
                weeks(year - 1)
            } else if week > weeks(year) {
                1
            } else {
                week
            }
        }
        _ => unreachable!(),
    };
    result as i32
}

// Numeric SQL values are converted to legacy DATETIME's 1/300-second grid.
// Keep the grid position: DATETIME2's 100ns representation cannot reproduce
// DATEPART(nanosecond) for a legacy value (one grid tick is 3,333,333 ns).
const LEGACY_TICKS_PER_DAY: i64 = 86_400 * 300;
const DATE_TICKS_PER_DAY: i64 = 864_000_000_000;

fn checked_legacy_ticks(ticks: i128) -> Result<i64, &'static str> {
    let base = crate::scalar::date_days(1900, 1, 1)?;
    let first =
        i128::from(crate::scalar::date_days(1753, 1, 1)? - base) * i128::from(LEGACY_TICKS_PER_DAY);
    let last = i128::from(crate::scalar::date_days(9999, 12, 31)? - base + 1)
        * i128::from(LEGACY_TICKS_PER_DAY);
    if !(first..last).contains(&ticks) {
        return Err(crate::calendar_parts::OVERFLOW);
    }
    i64::try_from(ticks).map_err(|_| crate::calendar_parts::OVERFLOW)
}

fn exact_numeric_ticks(coefficient: i128, scale: u8) -> Result<i64, &'static str> {
    if scale > 38 {
        return Err(crate::calendar_parts::OVERFLOW);
    }
    let denominator = 10_i128.pow(u32::from(scale));
    let magnitude = coefficient.unsigned_abs();
    let whole = magnitude / denominator as u128;
    let fraction = magnitude % denominator as u128;
    if whole > 3_000_000 {
        return Err(crate::calendar_parts::OVERFLOW);
    }
    // Thirty decimal places are enough to decide rounding at the 1/300s
    // boundary without overflowing i128. Retain discarded digits to resolve
    // the exact half-grid case rather than converting DECIMAL through f64.
    let dropped = scale.saturating_sub(30);
    let divisor = 10_u128.pow(u32::from(dropped));
    let reduced = fraction / divisor;
    let tail = fraction % divisor;
    let reduced_denominator = 10_i128.pow(u32::from(scale.min(30)));
    let numerator = (reduced as i128) * i128::from(LEGACY_TICKS_PER_DAY);
    let quotient = numerator / reduced_denominator;
    let remainder = numerator % reduced_denominator;
    let full_denominator = reduced_denominator * divisor as i128;
    let residual = remainder * divisor as i128 + tail as i128 * i128::from(LEGACY_TICKS_PER_DAY);
    let rounded = quotient
        + residual / full_denominator
        + i128::from(residual % full_denominator >= (full_denominator + 1) / 2);
    let magnitude_ticks = (whole as i128)
        .checked_mul(i128::from(LEGACY_TICKS_PER_DAY))
        .and_then(|n| n.checked_add(rounded))
        .ok_or(crate::calendar_parts::OVERFLOW)?;
    checked_legacy_ticks(if coefficient < 0 {
        -magnitude_ticks
    } else {
        magnitude_ticks
    })
}

fn floating_numeric_ticks(value: f64) -> Result<i64, &'static str> {
    // Bound before multiplication so the rounded integer is representable.
    if !value.is_finite() || !(-54_000.0..=3_000_000.0).contains(&value) {
        return Err(crate::calendar_parts::OVERFLOW);
    }
    checked_legacy_ticks((value * LEGACY_TICKS_PER_DAY as f64).round() as i128)
}

fn numeric_ticks(source: &FlatVector<'_>, row: usize, len: usize) -> Result<i64, &'static str> {
    let logical = source.logical_type();
    macro_rules! read {
        ($ty:ty) => {
            unsafe { source.as_slice_with_len::<$ty>(len)[row] }
        };
    }
    match logical.id() {
        Id::Boolean => exact_numeric_ticks(i128::from(read!(u8)), 0),
        Id::Tinyint => exact_numeric_ticks(i128::from(read!(i8)), 0),
        Id::UTinyint => exact_numeric_ticks(i128::from(read!(u8)), 0),
        Id::Smallint => exact_numeric_ticks(i128::from(read!(i16)), 0),
        Id::USmallint => exact_numeric_ticks(i128::from(read!(u16)), 0),
        Id::Integer => exact_numeric_ticks(i128::from(read!(i32)), 0),
        Id::UInteger => exact_numeric_ticks(i128::from(read!(u32)), 0),
        Id::Bigint => exact_numeric_ticks(i128::from(read!(i64)), 0),
        Id::UBigint => exact_numeric_ticks(i128::from(read!(u64)), 0),
        Id::Float => floating_numeric_ticks(f64::from(read!(f32))),
        Id::Double => floating_numeric_ticks(read!(f64)),
        Id::Decimal => {
            let coefficient = match logical.decimal_width() {
                1..=4 => i128::from(read!(i16)),
                5..=9 => i128::from(read!(i32)),
                10..=18 => i128::from(read!(i64)),
                19..=38 => {
                    let v = read!(duckdb::ffi::duckdb_hugeint);
                    (i128::from(v.upper) << 64) | i128::from(v.lower)
                }
                _ => return Err(crate::calendar_parts::OVERFLOW),
            };
            exact_numeric_ticks(coefficient, logical.decimal_scale())
        }
        _ => Err("DATEPART requires numeric input"),
    }
}

fn numeric_datetime(ticks: i64) -> Result<crate::datetime2::DateTime2, &'static str> {
    let base = crate::datetime2::DateTime2::from_parts(crate::datetime2::Parts {
        year: 1900,
        month: 1,
        day: 1,
        hour: 0,
        minute: 0,
        second: 0,
        fraction: 0,
    })
    .map_err(|_| crate::calendar_parts::OVERFLOW)?;
    let days = ticks.div_euclid(LEGACY_TICKS_PER_DAY);
    let time = ticks.rem_euclid(LEGACY_TICKS_PER_DAY);
    let precise = base.ticks() + days * DATE_TICKS_PER_DAY + (time * 10_000_000 + 150) / 300;
    crate::datetime2::DateTime2::from_ticks(precise).map_err(|_| crate::calendar_parts::OVERFLOW)
}

struct NumericPart<const P: usize>;
impl<const P: usize> VScalar for NumericPart<P> {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let source = input.flat_vector(0);
        let first = if P == 5 || P == 6 {
            Some(input.flat_vector(1))
        } else {
            None
        };
        let mut result = output.flat_vector();
        for row in 0..len {
            if source.row_is_null(row as u64) {
                result.set_null(row);
                continue;
            }
            let first = match &first {
                Some(v) if !v.row_is_null(row as u64) => {
                    i64::from(unsafe { v.as_slice_with_len::<i32>(len)[row] })
                }
                Some(_) => return Err("DATEFIRST must be between 1 and 7".into()),
                None => 7,
            };
            if !(1..=7).contains(&first) {
                return Err("DATEFIRST must be between 1 and 7".into());
            }
            let ticks = numeric_ticks(&source, row, len)?;
            let value = numeric_datetime(ticks)?;
            let part = if P == 12 {
                ((ticks.rem_euclid(300) * 1_000_000_000) / 300) as i32
            } else {
                extract(value, P, first)
            };
            unsafe {
                result.as_mut_slice_with_len::<i32>(len)[row] = part;
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            if P == 5 || P == 6 {
                vec![Id::Any.into(), Id::Integer.into()]
            } else {
                vec![Id::Any.into()]
            },
            Id::Integer.into(),
        )]
    }
}

struct NumericDate;
impl VScalar for NumericDate {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let source = input.flat_vector(0);
        let mut result = output.flat_vector();
        for row in 0..len {
            if source.row_is_null(row as u64) {
                result.set_null(row);
                continue;
            }
            let ticks = numeric_ticks(&source, row, len)?;
            let unix_day = i64::from(crate::scalar::date_days(1900, 1, 1)?)
                + ticks.div_euclid(LEGACY_TICKS_PER_DAY);
            unsafe {
                result.as_mut_slice_with_len::<i32>(len)[row] = i32::try_from(unix_day)?;
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![Id::Any.into()],
            Id::Date.into(),
        )]
    }
}
struct Part<const P: usize, const NAME: bool = false>;
impl<const P: usize, const NAME: bool> VScalar for Part<P, NAME> {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let source = input.flat_vector(0);
        let logical = source.logical_type();
        let offset = logical.id() == Id::Struct
            && logical.num_children() == 2
            && logical.child_name(0) == "__msduck_datetimeoffset_7"
            && logical.child(0).id() == Id::Bigint
            && logical.child_name(1) == "__msduck_offset_minutes"
            && logical.child(1).id() == Id::Smallint;
        let datetime2 = logical.id() == Id::Struct
            && logical.num_children() == 1
            && logical.child(0).id() == Id::Bigint
            && logical.child_name(0) == "__msduck_datetime2_7";
        if !(offset || datetime2) {
            return Err("DATEPART requires exact temporal input".into());
        }
        let first = if P == 5 || P == 6 {
            Some(input.flat_vector(1))
        } else {
            None
        };
        if first
            .as_ref()
            .is_some_and(|v| v.logical_type().id() != Id::Integer)
        {
            return Err("DATEFIRST requires INT".into());
        }
        let structure = input.struct_vector(0);
        let ticks = structure.child(0, len);
        let offsets = offset.then(|| structure.child(1, len));
        let mut result = output.flat_vector();
        for row in 0..len {
            if source.row_is_null(row as u64) || ticks.row_is_null(row as u64) {
                result.set_null(row);
                continue;
            }
            let first = match &first {
                Some(v) if !v.row_is_null(row as u64) => {
                    i64::from(unsafe { v.as_slice_with_len::<i32>(len)[row] })
                }
                Some(_) => return Err("DATEFIRST must be between 1 and 7".into()),
                None => 7,
            };
            if !(1..=7).contains(&first) {
                return Err("DATEFIRST must be between 1 and 7".into());
            }
            let value = crate::datetime2::DateTime2::from_ticks(unsafe {
                ticks.as_slice_with_len::<i64>(len)[row]
            })?;
            let (value, minutes) = if let Some(offsets) = &offsets {
                if offsets.row_is_null(row as u64) {
                    return Err("invalid DATETIMEOFFSET offset".into());
                }
                let minutes = unsafe { offsets.as_slice_with_len::<i16>(len)[row] };
                (
                    crate::datetimeoffset::DateTimeOffset::from_utc(value, minutes)?.local(),
                    minutes,
                )
            } else {
                (value, 0)
            };
            if NAME {
                let text = if offset {
                    format!(
                        "{}{:02}:{:02}",
                        if minutes < 0 { "-" } else { "+" },
                        minutes.abs() / 60,
                        minutes.abs() % 60
                    )
                } else {
                    "0".to_string()
                };
                result.insert(row, text.as_str());
            } else {
                unsafe {
                    result.as_mut_slice_with_len::<i32>(len)[row] = if P == 13 {
                        i32::from(minutes)
                    } else {
                        extract(value, P, first)
                    };
                }
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            if P == 5 || P == 6 {
                vec![Id::Any.into(), Id::Integer.into()]
            } else {
                vec![Id::Any.into()]
            },
            if NAME { Id::Varchar } else { Id::Integer }.into(),
        )]
    }
}
pub fn register(db: &duckdb::Connection) -> duckdb::Result<()> {
    #[cfg(test)]
    let benchmark_start = std::time::Instant::now();
    macro_rules! register { ($($n:literal),*) => { $(db.register_scalar_function::<Part<$n>>(concat!("__msduck_datepart_",stringify!($n)))?;)* }; }
    register!(0, 1, 2, 3, 4, 7, 8, 9, 10, 11, 12, 13, 14);
    macro_rules! numeric { ($($n:literal),*) => { $(db.register_scalar_function::<NumericPart<$n>>(concat!("__msduck_numeric_datepart_", stringify!($n)))?;)* }; }
    numeric!(0, 1, 2, 3, 4, 7, 8, 9, 10, 11, 12, 14);
    db.register_scalar_function::<NumericPart<5>>("__msduck_numeric_datepart_5_raw")?;
    db.register_scalar_function::<NumericPart<6>>("__msduck_numeric_datepart_6_raw")?;
    db.register_scalar_function::<NumericDate>("__msduck_numeric_date")?;
    db.register_scalar_function::<Part<13, true>>("__msduck_datename_offset")?;
    let offset_types = (0..=7)
        .map(|s| format!("'{}'", crate::datetimeoffset_cast::storage_type(s)))
        .collect::<Vec<_>>()
        .join(",");
    db.register_scalar_function::<Part<5>>("__msduck_datepart_5_raw")?;
    db.register_scalar_function::<Part<6>>("__msduck_datepart_6_raw")?;
    // Keep the dependent macro definitions in their original order.
    let mut definitions = String::new();
    for p in [5, 6] {
        definitions.push_str(&format!("CREATE OR REPLACE MACRO main.__msduck_datepart_{p}(value) AS __msduck_datepart_{p}_raw(value, CAST(coalesce(getvariable('__msduck_datefirst'),7) AS INTEGER));\n"));
        definitions.push_str(&format!("CREATE OR REPLACE MACRO main.__msduck_numeric_datepart_{p}(value) AS __msduck_numeric_datepart_{p}_raw(value, CAST(coalesce(getvariable('__msduck_datefirst'),7) AS INTEGER));\n"));
    }
    let numeric_type =
        "typeof(value) IN ('BOOLEAN','FLOAT','DOUBLE') OR typeof(value) LIKE 'DECIMAL%'";
    for (part, name) in [
        "year",
        "quarter",
        "month",
        "dayofyear",
        "day",
        "week",
        "weekday",
        "hour",
        "minute",
        "second",
        "millisecond",
        "microsecond",
        "nanosecond",
        "tzoffset",
        "iso_week",
    ]
    .iter()
    .enumerate()
    {
        let unsupported = match part {
            0..=6 | 14 => "WHEN typeof(value) IN ('TIME','TIME_NS') THEN 'time'",
            7..=12 => "WHEN typeof(value)='DATE' THEN 'date'",
            13 => {
                "WHEN typeof(value)='DATE' THEN 'date' WHEN typeof(value) IN ('TIME','TIME_NS') THEN 'time' WHEN typeof(value) IN ('TIMESTAMP','TIMESTAMP_S','TIMESTAMP_MS','TIMESTAMP_NS','TINYINT','UTINYINT','SMALLINT','INTEGER','BIGINT') THEN 'datetime'"
            }
            _ => unreachable!(),
        };
        // typeof is resolved during binding; only conversion evaluates value.
        // Check before converting: a TIME value must not gain valid calendar
        // fields just because conversion supplies the 1900 base date.
        let result = match part {
            2 => "monthname(__msduck_datetime2_date(value))".to_string(),
            6 => "dayname(__msduck_datetime2_date(value))".to_string(),
            _ => format!("CAST(__msduck_datepart_{part}(value) AS VARCHAR)"),
        };
        definitions.push_str(&format!(
            "CREATE OR REPLACE MACRO main.__msduck_datename_{part}(value) AS {result};\n"
        ));
        for function in ["datepart", "datename"] {
            let unsupported = if function == "datename" && part == 13 {
                "WHEN FALSE THEN 'unused'"
            } else {
                unsupported
            };
            let input = format!(
                "CASE WHEN (CASE {unsupported} ELSE NULL END) IS NOT NULL THEN error('The datepart {name} is not supported by date function {function} for data type ' || (CASE {unsupported} ELSE NULL END) || '.') WHEN typeof(value) IN ('TINYINT','UTINYINT','SMALLINT','INTEGER','BIGINT') THEN __msduck_datetime2_cast_7(__msduck_integer_date(CAST(CAST(value AS VARCHAR) AS BIGINT))) ELSE __msduck_datetime2_cast_7(value) END"
            );
            let offset_function = if function == "datename" && part == 13 {
                "__msduck_datename_offset".to_string()
            } else {
                format!("__msduck_{function}_{part}")
            };
            let numeric_result = if part == 13 {
                format!(
                    "error('The datepart tzoffset is not supported by date function {function} for data type datetime.')"
                )
            } else if function == "datename" && part == 2 {
                "monthname(__msduck_numeric_date(value))".to_string()
            } else if function == "datename" && part == 6 {
                "dayname(__msduck_numeric_date(value))".to_string()
            } else if function == "datename" {
                format!("CAST(__msduck_numeric_datepart_{part}(value) AS VARCHAR)")
            } else {
                format!("__msduck_numeric_datepart_{part}(value)")
            };
            definitions.push_str(&format!("CREATE OR REPLACE MACRO main.__msduck_{function}_dispatch_{part}(value) AS CASE WHEN typeof(value) IN ({offset_types}) THEN {offset_function}(__msduck_datetimeoffset_cast_7(value)) WHEN ({numeric_type}) THEN {numeric_result} ELSE __msduck_{function}_{part}({input}) END;\n"));
        }
    }
    db.execute_batch(&definitions)?;
    #[cfg(test)]
    if std::env::var_os("MSDUCK_DATEPART_BENCH").is_some() {
        println!(
            "MSDUCK_DATEPART_MS {:.6}",
            benchmark_start.elapsed().as_secs_f64() * 1000.0
        );
    }
    Ok(())
}

/// Parse only the supported literal/local-variable setting value.
pub fn setting(statement: &Statement) -> Result<Option<Expr>, String> {
    use sqlparser::{dialect::GenericDialect, parser::Parser};
    let value = match statement {
        Statement::Set(Set::SetSessionParam(SetSessionParamKind::Generic(param)))
            if param.names.len() == 1 && param.names[0].eq_ignore_ascii_case("DATEFIRST") =>
        {
            Parser::new(&GenericDialect {})
                .try_with_sql(&param.value)
                .map_err(|e| e.to_string())?
                .parse_expr()
                .map_err(|e| e.to_string())?
        }
        Statement::Set(Set::SingleAssignment {
            variable,
            values,
            scope: None,
            hivevar: false,
        }) if variable.to_string().eq_ignore_ascii_case("DATEFIRST") && values.len() == 1 => {
            values[0].clone()
        }
        _ => return Ok(None),
    };
    if !matches!(&value, Expr::Value(v) if matches!(v.value, Value::Number(_, _)))
        && !matches!(&value, Expr::Identifier(id) if id.value.starts_with('@') && !id.value.starts_with("@@"))
        && !matches!(&value, Expr::UnaryOp { op: UnaryOperator::Minus | UnaryOperator::Plus, expr } if matches!(expr.as_ref(), Expr::Value(v) if matches!(v.value, Value::Number(_, _))))
    {
        return Err("SET DATEFIRST requires an integer or local variable".into());
    }
    Ok(Some(value))
}

#[cfg(test)]
mod tests {
    #[test]
    fn numeric_grid_matches_retained_sql_server_boundaries() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        for (sql, expected) in [
            (
                "SELECT __msduck_datepart_dispatch_7(CAST(0.5 AS DECIMAL(10,4)))",
                12,
            ),
            (
                "SELECT __msduck_datepart_dispatch_7(CAST(-0.5 AS DECIMAL(10,4)))",
                12,
            ),
            (
                "SELECT __msduck_datepart_dispatch_12(CAST(0.0000000192 AS DECIMAL(20,10)))",
                0,
            ),
            (
                "SELECT __msduck_datepart_dispatch_12(CAST(0.0000000194 AS DECIMAL(20,10)))",
                3_333_333,
            ),
            (
                "SELECT __msduck_datepart_dispatch_12(CAST(-0.0000000386 AS DECIMAL(20,10)))",
                996_666_666,
            ),
            (
                "SELECT __msduck_datepart_dispatch_10(CAST(0.0001 AS DECIMAL(19,4)))",
                640,
            ),
            (
                "SELECT __msduck_datepart_dispatch_0(CAST(-53690 AS DECIMAL(12,4)))",
                1753,
            ),
            (
                "SELECT __msduck_datepart_dispatch_0(CAST(2958463 AS DECIMAL(12,4)))",
                9999,
            ),
            (
                "SELECT __msduck_datepart_dispatch_4(CAST(TRUE AS BOOLEAN))",
                2,
            ),
        ] {
            let actual: i32 = db.query_row(sql, [], |r| r.get(0)).unwrap();
            assert_eq!(actual, expected, "{sql}");
        }
        for value in ["-53691", "2958464"] {
            let result: duckdb::Result<i32> = db.query_row(
                &format!("SELECT __msduck_datepart_dispatch_0(CAST({value} AS DECIMAL(12,4)))"),
                [],
                |r| r.get(0),
            );
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains(crate::calendar_parts::OVERFLOW)
            );
        }
    }

    #[test]
    fn numeric_vectors_preserve_nulls_datefirst_and_single_evaluation() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        let wrong: i64 = db.query_row("SELECT count(*) FROM range(6000) r(i) WHERE __msduck_datepart_dispatch_12(CASE WHEN i%17=0 THEN NULL ELSE CAST(0.0000000386 AS DECIMAL(20,10)) END) IS DISTINCT FROM CASE WHEN i%17=0 THEN NULL ELSE 3333333 END", [], |r| r.get(0)).unwrap();
        assert_eq!(wrong, 0);
        let sunday_first: i32 = db
            .query_row(
                "SELECT __msduck_datepart_dispatch_6(CAST(0.5 AS DECIMAL(10,4)))",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(sunday_first, 2);
        db.execute_batch("SET VARIABLE __msduck_datefirst = 1")
            .unwrap();
        let monday_first: i32 = db
            .query_row(
                "SELECT __msduck_datepart_dispatch_6(CAST(0.5 AS DECIMAL(10,4)))",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(monday_first, 1);
        db.execute_batch("CREATE SEQUENCE numeric_datepart_calls START 1")
            .unwrap();
        let rows: i64 = db.query_row("SELECT count(__msduck_datepart_dispatch_0(CAST(nextval('numeric_datepart_calls')%10 AS DECIMAL(20,12)))) FROM range(6000)", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 6000);
        let calls: i64 = db
            .query_row("SELECT currval('numeric_datepart_calls')", [], |r| r.get(0))
            .unwrap();
        assert_eq!(calls, 6000);
    }

    #[test]
    #[ignore = "repeatable local startup benchmark; use scripts/bench-datepart-startup.mjs"]
    fn startup_benchmark() {
        for _ in 0..20 {
            let start = std::time::Instant::now();
            let server = crate::server::Server::open(":memory:").unwrap();
            println!(
                "MSDUCK_SERVER_MS {:.6}",
                start.elapsed().as_secs_f64() * 1000.0
            );
            drop(server);
        }
    }

    #[test]
    fn offset_local_fields_nulls_and_single_evaluation_across_chunks() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        for scale in 0..=7 {
            let wrong: i64 = db.query_row(&format!("WITH v AS MATERIALIZED (SELECT i,__msduck_datetimeoffset_cast_{scale}(CASE WHEN i%17=0 THEN NULL WHEN i%3=0 THEN '2024-01-01T00:15:30.1234567+14:00' WHEN i%3=1 THEN '2024-01-01T23:15:30.1234567-14:00' ELSE '2024-01-01T00:15:30.1234567-00:30' END) d FROM range(6000) r(i)) SELECT count(*) FROM v WHERE __msduck_datepart_dispatch_0(d) IS DISTINCT FROM CASE WHEN i%17=0 THEN NULL ELSE 2024 END OR __msduck_datepart_dispatch_7(d) IS DISTINCT FROM CASE WHEN i%17=0 THEN NULL WHEN i%3=1 THEN 23 ELSE 0 END OR __msduck_datepart_dispatch_13(d) IS DISTINCT FROM CASE WHEN i%17=0 THEN NULL WHEN i%3=0 THEN 840 WHEN i%3=1 THEN -840 ELSE -30 END OR __msduck_datename_dispatch_13(d) IS DISTINCT FROM CASE WHEN i%17=0 THEN NULL WHEN i%3=0 THEN '+14:00' WHEN i%3=1 THEN '-14:00' ELSE '-00:30' END"), [], |r| r.get(0)).unwrap();
            assert_eq!(wrong, 0, "scale {scale}");
        }
        for function in [
            "datepart_dispatch_7",
            "datename_dispatch_13",
            "datename_dispatch_2",
        ] {
            db.execute_batch("CREATE OR REPLACE SEQUENCE offset_part_calls START 1")
                .unwrap();
            let count: i64 = db.query_row(&format!("SELECT count(__msduck_{function}(__msduck_datetimeoffset_cast_7(CASE WHEN nextval('offset_part_calls')%3=0 THEN NULL ELSE '2024-01-01T00:15:30+14:00' END))) FROM range(6000)"), [], |r| r.get(0)).unwrap();
            assert_eq!(count, 4000);
            let calls: i64 = db
                .query_row("SELECT currval('offset_part_calls')", [], |r| r.get(0))
                .unwrap();
            assert_eq!(calls, 6000, "{function}");
        }
    }

    #[test]
    fn iso_calendar_cycle_and_fraction_vectors() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        let wrong: i64 = db.query_row("SELECT count(*) FROM range(146097) r(i) WHERE __msduck_datepart_14(__msduck_datetime2_cast_7(DATE '2000-01-01'+CAST(i AS INTEGER))) <> week(DATE '2000-01-01'+CAST(i AS INTEGER))", [], |r| r.get(0)).unwrap();
        assert_eq!(wrong, 0);
        let wrong: i64 = db.query_row("SELECT count(*) FROM range(6000) r(i) WHERE __msduck_datepart_12(__msduck_datetime2_cast_7(CASE WHEN i%3=0 THEN NULL ELSE '0001-01-01T00:00:00.0000001' END)) IS DISTINCT FROM CASE WHEN i%3=0 THEN NULL ELSE 100 END", [], |r| r.get(0)).unwrap();
        assert_eq!(wrong, 0);
        db.execute_batch("CREATE SEQUENCE datepart_calls START 1")
            .unwrap();
        let rows: i64 = db.query_row("SELECT count(__msduck_datepart_dispatch_9(printf('2026-01-01T00:00:%02d',nextval('datepart_calls')%60))) FROM range(6000)", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 6000);
        db.execute_batch("CREATE SEQUENCE integer_datepart_calls START 1")
            .unwrap();
        let integer_rows: i64 = db.query_row("SELECT count(__msduck_datepart_dispatch_0(nextval('integer_datepart_calls')%100)) FROM range(6000)", [], |r| r.get(0)).unwrap();
        assert_eq!(integer_rows, 6000);
        db.execute_batch("CREATE SEQUENCE datename_calls START 1")
            .unwrap();
        let names: i64 = db.query_row("SELECT count(__msduck_datename_dispatch_2(printf('2026-%02d-01',nextval('datename_calls')%12+1))) FROM range(6000)", [], |r| r.get(0)).unwrap();
        assert_eq!(names, 6000);
        assert_eq!(
            db.query_row("SELECT currval('datename_calls')", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            6000
        );

        assert_eq!(
            db.query_row("SELECT currval('integer_datepart_calls')", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            6000
        );

        assert_eq!(
            db.query_row("SELECT currval('datepart_calls')", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            6000
        );
    }
}
