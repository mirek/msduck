//! DATEADD dispatch for DATE and exact DATETIME2 arithmetic.
use duckdb::{
    core::{DataChunkHandle, LogicalTypeId as Id},
    ffi::{duckdb_date, duckdb_from_date},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use sqlparser::ast::*;

pub fn lower(
    expr: &mut Expr,
    parameters: &std::collections::HashMap<String, crate::parameter::Parameter>,
) -> Result<(), String> {
    let Expr::Function(f) = expr else {
        return Ok(());
    };
    if !f.name.to_string().eq_ignore_ascii_case("DATEADD") {
        return Ok(());
    }
    let FunctionArguments::List(args) = &f.args else {
        return Err("DATEADD requires three scalar arguments".into());
    };
    if !matches!(f.parameters, FunctionArguments::None)
        || f.over.is_some()
        || f.filter.is_some()
        || f.null_treatment.is_some()
        || !f.within_group.is_empty()
        || args.duplicate_treatment.is_some()
        || !args.clauses.is_empty()
    {
        return Err("unsupported DATEADD modifiers".into());
    }
    let [
        FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Identifier(part))),
        FunctionArg::Unnamed(FunctionArgExpr::Expr(number)),
        FunctionArg::Unnamed(FunctionArgExpr::Expr(date)),
    ] = args.args.as_slice()
    else {
        return Err("DATEADD requires a datepart keyword and two scalar arguments".into());
    };
    let part = datepart(&part.value).ok_or("invalid DATEADD datepart")?;
    let offset_scale = crate::datetimeoffset_compare::scale(date, parameters);
    let time_scale = crate::time_results::scale(date).or_else(|| {
        if let Expr::Identifier(id) = date
            && let Some(parameter) = parameters.get(&id.value.to_lowercase())
            && let DataType::Time(scale, TimezoneInfo::None) = parameter.ast_type()
        {
            return u8::try_from(scale.unwrap_or(7)).ok().filter(|s| *s <= 7);
        }
        None
    });
    if time_scale.is_some() && part < 5 {
        let name = ["year", "quarter", "month", "day", "week"][part];
        return Err(format!(
            "The datepart {name} is not supported by date function dateadd for data type time."
        ));
    }
    let scale = offset_scale
        .or_else(|| crate::datetime2_compare::scale(date, parameters))
        .or(time_scale);
    let name = scale
        .map(|s| {
            format!(
                "__msduck_{}_dateadd_{s}",
                if offset_scale.is_some() {
                    "datetimeoffset"
                } else if time_scale.is_some() {
                    "time"
                } else {
                    "datetime2"
                }
            )
        })
        .unwrap_or_else(|| format!("__msduck_dateadd_date_{part}"));
    *expr = crate::engine::binary_function(
        &name,
        Expr::Cast {
            kind: CastKind::Cast,
            expr: Box::new(number.clone()),
            data_type: DataType::Int(None),
            format: None,
        },
        scale
            .map(|s| {
                if offset_scale.is_some() {
                    crate::datetimeoffset_cast::convert(date.clone(), s)
                } else if time_scale.is_some() {
                    crate::assignment::convert(
                        date.clone(),
                        &DataType::Time(Some(u64::from(s)), TimezoneInfo::None),
                        false,
                    )
                } else {
                    crate::datetime2_cast::convert(date.clone(), s)
                }
            })
            .unwrap_or_else(|| date.clone()),
    );
    if scale.is_some()
        && let Expr::Function(f) = expr
        && let FunctionArguments::List(args) = &mut f.args
    {
        args.args
            .push(FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(
                Value::Number(part.to_string(), false).into(),
            ))));
    }
    Ok(())
}

pub(super) fn add(days: i32, number: i32, part: usize) -> Result<i32, &'static str> {
    let overflow = crate::eomonth::OVERFLOW;
    if !(-719162..=2932896).contains(&days) {
        return Err(overflow);
    }
    if part >= 3 {
        let result = i64::from(days) + i64::from(number) * if part == 4 { 7 } else { 1 };
        return if (-719162..=2932896).contains(&result) {
            Ok(result as i32)
        } else {
            Err(overflow)
        };
    }
    // Plain value structs, after rejecting infinity and out-of-range dates.
    let date = unsafe { duckdb_from_date(duckdb_date { days }) };
    let month = i64::from(date.year - 1) * 12
        + i64::from(date.month - 1)
        + i64::from(number) * [12, 3, 1][part];
    if !(0..9999 * 12).contains(&month) {
        return Err(overflow);
    }
    let year = (month / 12 + 1) as i32;
    let month = (month % 12 + 1) as i32;
    let leap = year % 400 == 0 || (year % 4 == 0 && year % 100 != 0);
    let last = match month {
        2 => 28 + i32::from(leap),
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    crate::scalar::date_days(year, month, i32::from(date.day).min(last))
}

struct Add<const PART: usize>;
impl<const PART: usize> VScalar for Add<PART> {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let number = input.flat_vector(0);
        let date = input.flat_vector(1);
        if date.logical_type().id() != Id::Date {
            return Err("DATEADD currently supports only typed DATE inputs".into());
        }
        if PART >= 5 {
            let name = [
                "hour",
                "minute",
                "second",
                "millisecond",
                "microsecond",
                "nanosecond",
            ][PART - 5];
            return Err(format!(
                "The datepart {name} is not supported by date function dateadd for data type date."
            )
            .into());
        }
        let mut result = output.flat_vector();
        for row in 0..len {
            if number.row_is_null(row as u64) || date.row_is_null(row as u64) {
                result.set_null(row);
                continue;
            }
            // INTEGER and DATE have i32 storage; only live non-NULL slots are read.
            let (number, date) = unsafe {
                (
                    number.as_slice_with_len::<i32>(len)[row],
                    date.as_slice_with_len::<i32>(len)[row],
                )
            };
            let value = add(date, number, PART)?;
            unsafe {
                result.as_mut_slice_with_len::<i32>(len)[row] = value;
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![Id::Integer.into(), Id::Any.into()],
            Id::Date.into(),
        )]
    }
}

pub fn register(db: &duckdb::Connection) -> duckdb::Result<()> {
    macro_rules! register { ($($part:literal),*) => { $(
        db.register_scalar_function::<Add<$part>>(concat!("__msduck_dateadd_date_", stringify!($part)))?;
    )* }; }
    register!(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10);
    crate::datetime2_add::register(db)?;
    crate::time_add::register(db)?;
    Ok(())
}

/// Dateparts shared by DATEADD and DATEDIFF, with calendar-day aliases collapsed.
pub fn datepart(name: &str) -> Option<usize> {
    Some(match name.to_ascii_lowercase().as_str() {
        "year" | "yy" | "yyyy" => 0,
        "quarter" | "qq" | "q" => 1,
        "month" | "mm" | "m" => 2,
        "dayofyear" | "dy" | "y" | "day" | "dd" | "d" | "weekday" | "dw" | "w" => 3,
        "week" | "wk" | "ww" => 4,
        "hour" | "hh" => 5,
        "minute" | "mi" | "n" => 6,
        "second" | "ss" | "s" => 7,
        "millisecond" | "ms" => 8,
        "microsecond" | "mcs" => 9,
        "nanosecond" | "ns" => 10,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn calendar_chunks_and_boundaries() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        super::register(&db).unwrap();
        for (part, unit) in [
            (0, "YEAR"),
            (1, "QUARTER"),
            (2, "MONTH"),
            (3, "DAY"),
            (4, "WEEK"),
        ] {
            let sql = format!(
                "SELECT count(*) FROM (SELECT CAST(CASE WHEN n%17=0 THEN NULL ELSE n%101-50 END AS INT) n, CASE WHEN n%19=0 THEN NULL ELSE DATE '2000-02-29'+CAST(n AS INT) END d FROM range(6000) r(n)) WHERE __msduck_dateadd_date_{part}(n,d) IS DISTINCT FROM CAST(d+n*INTERVAL 1 {unit} AS DATE)"
            );
            let wrong: i64 = db.query_row(&sql, [], |r| r.get(0)).unwrap();
            assert_eq!(wrong, 0);
            for (day, n) in [
                (-719162, -1),
                (2932896, 1),
                (0, i32::MAX),
                (0, i32::MIN),
                (i32::MAX, 0),
                (i32::MIN, 0),
            ] {
                assert!(super::add(day, n, part).is_err());
            }
        }
    }
}
