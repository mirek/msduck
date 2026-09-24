//! Calendar month ends, bounded by SQL Server's DATE range.
use duckdb::{
    core::{DataChunkHandle, LogicalTypeId},
    ffi::{duckdb_date, duckdb_from_date},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use sqlparser::ast::*;

pub const OVERFLOW: &str = "Adding a value to a 'date' column caused overflow.";

pub fn lower(expr: &mut Expr) -> Result<(), String> {
    let Expr::Function(function) = expr else {
        return Ok(());
    };
    if !function.name.to_string().eq_ignore_ascii_case("EOMONTH") {
        return Ok(());
    }
    let FunctionArguments::List(args) = &function.args else {
        return Err("EOMONTH requires one or two scalar arguments".into());
    };
    if !matches!(function.parameters, FunctionArguments::None)
        || function.over.is_some()
        || function.filter.is_some()
        || function.null_treatment.is_some()
        || !function.within_group.is_empty()
        || args.duplicate_treatment.is_some()
        || !args.clauses.is_empty()
    {
        return Err("unsupported EOMONTH modifiers".into());
    }
    let (date, offset) = match args.args.as_slice() {
        [FunctionArg::Unnamed(FunctionArgExpr::Expr(date))] => (
            date.clone(),
            Expr::Value(Value::Number("0".into(), false).into()),
        ),
        [
            FunctionArg::Unnamed(FunctionArgExpr::Expr(date)),
            FunctionArg::Unnamed(FunctionArgExpr::Expr(offset)),
        ] => (date.clone(), offset.clone()),
        _ => return Err("EOMONTH requires one or two scalar arguments".into()),
    };
    let cast = |expr, data_type| Expr::Cast {
        kind: CastKind::Cast,
        expr: Box::new(expr),
        data_type,
        format: None,
    };
    *expr = crate::engine::binary_function(
        "__msduck_eomonth",
        crate::engine::unary_function("__msduck_eomonth_date", date),
        cast(offset, DataType::Int(None)),
    );
    Ok(())
}

pub fn register(db: &duckdb::Connection) -> duckdb::Result<()> {
    db.register_scalar_function::<EndOfMonth>("__msduck_eomonth")?;
    db.execute_batch("CREATE OR REPLACE MACRO main.__msduck_eomonth_date(value) AS CASE WHEN typeof(value) IN ('TINYINT','UTINYINT','SMALLINT','INTEGER','BIGINT') THEN __msduck_integer_date(CAST(CAST(value AS VARCHAR) AS BIGINT)) ELSE __msduck_cast_date(value) END")?;
    Ok(())
}

fn month_end(days: i32, offset: i32) -> Result<i32, &'static str> {
    // Check before calling the C calendar helper, including DuckDB infinities.
    if !(-719162..=2932896).contains(&days) {
        return Err(OVERFLOW);
    }
    // Takes and returns plain value structs; no borrowed memory or pointers.
    let date = unsafe { duckdb_from_date(duckdb_date { days }) };
    let month_index = i64::from(date.year - 1) * 12 + i64::from(date.month - 1) + i64::from(offset);
    if !(0..9999 * 12).contains(&month_index) {
        return Err(OVERFLOW);
    }
    let year = (month_index / 12 + 1) as i32;
    let month = (month_index % 12 + 1) as i32;
    let leap = year % 400 == 0 || (year % 4 == 0 && year % 100 != 0);
    let day = match month {
        2 => 28 + i32::from(leap),
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    crate::scalar::date_days(year, month, day)
}

pub struct EndOfMonth;
impl VScalar for EndOfMonth {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let date = input.flat_vector(0);
        let offset = input.flat_vector(1);
        let mut result = output.flat_vector();
        for index in 0..len {
            if date.row_is_null(index as u64) || offset.row_is_null(index as u64) {
                result.set_null(index);
                continue;
            }
            // DATE and INTEGER both have i32 storage. Read only live chunk slots.
            let (days, months) = unsafe {
                (
                    date.as_slice_with_len::<i32>(len)[index],
                    offset.as_slice_with_len::<i32>(len)[index],
                )
            };
            let end = month_end(days, months)?;
            unsafe {
                result.as_mut_slice_with_len::<i32>(len)[index] = end;
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![LogicalTypeId::Date.into(), LogicalTypeId::Integer.into()],
            LogicalTypeId::Date.into(),
        )]
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn integer_inputs_across_chunks_and_single_evaluation() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        let wrong:i64=db.query_row("SELECT count(*) FROM range(6000) r(i) WHERE __msduck_eomonth(__msduck_eomonth_date(CASE WHEN i%17=0 THEN NULL ELSE i-3000 END),0) IS DISTINCT FROM CASE WHEN i%17=0 THEN NULL ELSE last_day(DATE '1900-01-01'+CAST(i-3000 AS INT)) END",[],|r|r.get(0)).unwrap();
        assert_eq!(wrong, 0);
        db.execute_batch("CREATE SEQUENCE month_input START 1")
            .unwrap();
        let count:i64=db.query_row("SELECT count(__msduck_eomonth(__msduck_eomonth_date(nextval('month_input')),0)) FROM range(6000)",[],|r|r.get(0)).unwrap();
        assert_eq!(count, 6000);
        assert_eq!(
            db.query_row("SELECT currval('month_input')", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            6000
        );
    }

    #[test]
    fn calendar_and_nulls_across_chunks() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        let wrong: i64 = db.query_row("SELECT count(*) FROM (SELECT CAST(value AS DATE) d FROM generate_series(DATE '0001-01-01', DATE '9999-12-31', INTERVAL 1 DAY) AS t(value)) WHERE __msduck_eomonth(d, 0) <> last_day(d)", [], |r| r.get(0)).unwrap();
        assert_eq!(wrong, 0);
        let wrong: i64 = db.query_row("SELECT count(*) FROM (SELECT CASE WHEN n%17=0 THEN NULL ELSE DATE '2000-01-31' END d, CAST(CASE WHEN n%19=0 THEN NULL ELSE n-3000 END AS INT) m FROM range(6000) r(n)) WHERE __msduck_eomonth(d,m) IS DISTINCT FROM last_day(d + m * INTERVAL 1 MONTH)", [], |r| r.get(0)).unwrap();
        assert_eq!(wrong, 0);
        for (date, offset) in [
            (-719162, -1),
            (2932896, 1),
            (0, i32::MAX),
            (0, i32::MIN),
            (i32::MAX, 0),
            (i32::MIN, 0),
        ] {
            assert!(super::month_end(date, offset).is_err());
        }
    }
}
