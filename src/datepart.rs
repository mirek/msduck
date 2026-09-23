//! DATEPART extraction from exact temporal values with session DATEFIRST.
use duckdb::{
    core::{DataChunkHandle, Inserter, LogicalTypeId as Id},
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
    macro_rules! register { ($($n:literal),*) => { $(db.register_scalar_function::<Part<$n>>(concat!("__msduck_datepart_",stringify!($n)))?;)* }; }
    register!(0, 1, 2, 3, 4, 7, 8, 9, 10, 11, 12, 13, 14);
    db.register_scalar_function::<Part<13, true>>("__msduck_datename_offset")?;
    let offset_types = (0..=7)
        .map(|s| format!("'{}'", crate::datetimeoffset_cast::storage_type(s)))
        .collect::<Vec<_>>()
        .join(",");
    db.register_scalar_function::<Part<5>>("__msduck_datepart_5_raw")?;
    db.register_scalar_function::<Part<6>>("__msduck_datepart_6_raw")?;
    for p in [5, 6] {
        db.execute_batch(&format!("CREATE OR REPLACE MACRO main.__msduck_datepart_{p}(value) AS __msduck_datepart_{p}_raw(value, CAST(coalesce(getvariable('__msduck_datefirst'),7) AS INTEGER))"))?;
    }
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
        for function in ["datepart", "datename"] {
            let unsupported = if function == "datename" && part == 13 {
                "WHEN FALSE THEN 'unused'"
            } else {
                unsupported
            };
            db.execute_batch(&format!("CREATE OR REPLACE MACRO main.__msduck_{function}_input_{part}(value) AS CASE WHEN (CASE {unsupported} ELSE NULL END) IS NOT NULL THEN error('The datepart {name} is not supported by date function {function} for data type ' || (CASE {unsupported} ELSE NULL END) || '.') WHEN typeof(value) IN ('TINYINT','UTINYINT','SMALLINT','INTEGER','BIGINT') THEN __msduck_datetime2_cast_7(__msduck_integer_date(CAST(CAST(value AS VARCHAR) AS BIGINT))) ELSE __msduck_datetime2_cast_7(value) END"))?;
        }
        let result = match part {
            2 => "monthname(__msduck_datetime2_date(value))".to_string(),
            6 => "dayname(__msduck_datetime2_date(value))".to_string(),
            _ => format!("CAST(__msduck_datepart_{part}(value) AS VARCHAR)"),
        };
        db.execute_batch(&format!(
            "CREATE OR REPLACE MACRO main.__msduck_datename_{part}(value) AS {result}"
        ))?;
        for function in ["datepart", "datename"] {
            let offset_function = if function == "datename" && part == 13 {
                "__msduck_datename_offset".to_string()
            } else {
                format!("__msduck_{function}_{part}")
            };
            db.execute_batch(&format!("CREATE OR REPLACE MACRO main.__msduck_{function}_dispatch_{part}(value) AS CASE WHEN typeof(value) IN ({offset_types}) THEN {offset_function}(__msduck_datetimeoffset_cast_7(value)) ELSE __msduck_{function}_{part}(__msduck_{function}_input_{part}(value)) END"))?;
        }
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
        let rows: i64 = db.query_row("SELECT count(__msduck_datepart_9(__msduck_datepart_input_9(printf('2026-01-01T00:00:%02d',nextval('datepart_calls')%60)))) FROM range(6000)", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 6000);
        db.execute_batch("CREATE SEQUENCE integer_datepart_calls START 1")
            .unwrap();
        let integer_rows: i64 = db.query_row("SELECT count(__msduck_datepart_0(__msduck_datepart_input_0(nextval('integer_datepart_calls')%100))) FROM range(6000)", [], |r| r.get(0)).unwrap();
        assert_eq!(integer_rows, 6000);
        db.execute_batch("CREATE SEQUENCE datename_calls START 1")
            .unwrap();
        let names: i64 = db.query_row("SELECT count(__msduck_datename_2(__msduck_datename_input_2(printf('2026-%02d-01',nextval('datename_calls')%12+1)))) FROM range(6000)", [], |r| r.get(0)).unwrap();
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
