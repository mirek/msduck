//! Count temporal boundaries using exact 100ns ticks and bounded integer results.
use crate::datetime2::DateTime2;
use duckdb::{
    core::{DataChunkHandle, LogicalTypeHandle, LogicalTypeId as Id},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use sqlparser::ast::*;

pub const OVERFLOW: &str = "The datediff function resulted in an overflow. The number of dateparts separating two date/time instances is too large. Try to use datediff with a less precise datepart.";
const DAY: i64 = 864_000_000_000;

pub fn lower(expr: &mut Expr) -> Result<(), String> {
    let Expr::Function(f) = expr else {
        return Ok(());
    };
    let name = f.name.to_string().to_ascii_lowercase();
    if !matches!(name.as_str(), "datediff" | "datediff_big") {
        return Ok(());
    }
    let FunctionArguments::List(args) = &f.args else {
        return Err(format!("{name} requires three scalar arguments"));
    };
    if !matches!(f.parameters, FunctionArguments::None)
        || f.over.is_some()
        || f.filter.is_some()
        || f.null_treatment.is_some()
        || !f.within_group.is_empty()
        || args.duplicate_treatment.is_some()
        || !args.clauses.is_empty()
    {
        return Err(format!("unsupported {name} modifiers"));
    }
    let [
        FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Identifier(part))),
        FunctionArg::Unnamed(FunctionArgExpr::Expr(start)),
        FunctionArg::Unnamed(FunctionArgExpr::Expr(end)),
    ] = args.args.as_slice()
    else {
        return Err(format!(
            "{name} requires a datepart keyword and two scalar arguments"
        ));
    };
    let part =
        crate::dateadd::datepart(&part.value).ok_or_else(|| format!("invalid {name} datepart"))?;
    let call = crate::engine::binary_function(
        &format!("__msduck_{name}_{part}"),
        crate::engine::unary_function("__msduck_datediff_input", start.clone()),
        crate::engine::unary_function("__msduck_datediff_input", end.clone()),
    );
    *expr = Expr::Cast {
        kind: CastKind::Cast,
        expr: Box::new(call),
        data_type: if name == "datediff" {
            DataType::Int(None)
        } else {
            DataType::BigInt(None)
        },
        format: None,
    };
    Ok(())
}

fn difference(start: DateTime2, end: DateTime2, part: usize) -> i128 {
    let boundary = |value: DateTime2| -> i128 {
        let ticks = value.ticks();
        match part {
            0..=2 => {
                let p = value.parts();
                let year = i128::from(p.year);
                let month = i128::from(p.month - 1);
                match part {
                    0 => year,
                    1 => year * 4 + month / 3,
                    _ => year * 12 + month,
                }
            }
            3 => i128::from(ticks / DAY),
            // Day zero (0001-01-01) is Monday; Sundays start a new week.
            4 => i128::from((ticks / DAY + 1) / 7),
            5 => i128::from(ticks / 36_000_000_000),
            6 => i128::from(ticks / 600_000_000),
            7 => i128::from(ticks / 10_000_000),
            8 => i128::from(ticks / 10_000),
            9 => i128::from(ticks / 10),
            10 => i128::from(ticks) * 100,
            _ => unreachable!("datepart is validated before registration"),
        }
    };
    boundary(end) - boundary(start)
}
struct Diff<const PART: usize, const BIG: bool>;
fn kind() -> LogicalTypeHandle {
    LogicalTypeHandle::struct_type(&[("__msduck_datetime2_7", Id::Bigint.into())])
}
impl<const PART: usize, const BIG: bool> VScalar for Diff<PART, BIG> {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let a = input.flat_vector(0);
        let b = input.flat_vector(1);
        let sa = input.struct_vector(0);
        let sb = input.struct_vector(1);
        let ta = sa.child(0, len);
        let tb = sb.child(0, len);
        let mut out = output.flat_vector();
        for row in 0..len {
            if a.row_is_null(row as u64) || b.row_is_null(row as u64) {
                out.set_null(row);
                continue;
            }
            if ta.row_is_null(row as u64) || tb.row_is_null(row as u64) {
                return Err("invalid DATETIME2 ticks".into());
            }
            // Exact signatures fix each child to BIGINT; only live non-NULL slots are read.
            let (a, b) = unsafe {
                (
                    ta.as_slice_with_len::<i64>(len)[row],
                    tb.as_slice_with_len::<i64>(len)[row],
                )
            };
            let n = difference(DateTime2::from_ticks(a)?, DateTime2::from_ticks(b)?, PART);
            if BIG {
                let n = i64::try_from(n).map_err(|_| OVERFLOW)?;
                unsafe {
                    out.as_mut_slice_with_len::<i64>(len)[row] = n;
                }
            } else {
                let n = i32::try_from(n).map_err(|_| OVERFLOW)?;
                unsafe {
                    out.as_mut_slice_with_len::<i32>(len)[row] = n;
                }
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![kind(), kind()],
            if BIG {
                Id::Bigint.into()
            } else {
                Id::Integer.into()
            },
        )]
    }
}
// A separate UTC adapter keeps DATEDIFF semantics independent of local-clock casts.
struct OffsetUtc;
impl VScalar for OffsetUtc {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let source = input.flat_vector(0);
        let structure = input.struct_vector(0);
        let ticks = structure.child(0, len);
        let offsets = structure.child(1, len);
        let mut result = output.struct_vector();
        let mut target = result.child(0, len);
        for row in 0..len {
            if source.row_is_null(row as u64) {
                result.set_null(row);
                target.set_null(row);
                continue;
            }
            if ticks.row_is_null(row as u64) || offsets.row_is_null(row as u64) {
                return Err("invalid DATETIMEOFFSET payload".into());
            }
            let value = unsafe {
                crate::datetimeoffset::DateTimeOffset::from_utc(
                    DateTime2::from_ticks(ticks.as_slice_with_len::<i64>(len)[row])?,
                    offsets.as_slice_with_len::<i16>(len)[row],
                )?
            };
            unsafe {
                target.as_mut_slice_with_len::<i64>(len)[row] = value.utc().ticks();
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![LogicalTypeHandle::struct_type(&[
                ("__msduck_datetimeoffset_7", Id::Bigint.into()),
                ("__msduck_offset_minutes", Id::Smallint.into()),
            ])],
            kind(),
        )]
    }
}

pub fn register(db: &duckdb::Connection) -> duckdb::Result<()> {
    db.register_scalar_function::<OffsetUtc>("__msduck_datediff_offset_utc")?;
    let offsets = (0..=7)
        .map(|s| format!("'{}'", crate::datetimeoffset_cast::storage_type(s)))
        .collect::<Vec<_>>()
        .join(",");
    db.execute_batch(&format!("CREATE OR REPLACE MACRO main.__msduck_datediff_input(value) AS CASE WHEN typeof(value) IN ({offsets}) THEN __msduck_datediff_offset_utc(__msduck_datetimeoffset_cast_7(value)) WHEN typeof(value) IN ('TINYINT','UTINYINT','SMALLINT','INTEGER','BIGINT') THEN __msduck_datetime2_cast_7(__msduck_integer_date(CAST(CAST(value AS VARCHAR) AS BIGINT))) ELSE __msduck_datetime2_cast_7(value) END"))?;
    macro_rules! register { ($($p:literal),*) => { $(
        db.register_scalar_function::<Diff<$p,false>>(concat!("__msduck_datediff_",stringify!($p)))?;
        db.register_scalar_function::<Diff<$p,true>>(concat!("__msduck_datediff_big_",stringify!($p)))?;
    )* }; }
    register!(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn offset_inputs_preserve_utc_ticks_nulls_and_single_evaluation() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        let utc = DateTime2::parse_iso("2024-01-01T06:30:00.1234567").unwrap();
        for scale in 0..=7 {
            db.execute_batch("CREATE OR REPLACE SEQUENCE offset_diff_a; CREATE OR REPLACE SEQUENCE offset_diff_b").unwrap();
            let sql = format!(
                "SELECT __msduck_datediff_big_10(__msduck_datediff_input(__msduck_datetimeoffset_cast_{scale}(CASE WHEN nextval('offset_diff_a')%17=0 THEN NULL ELSE '2024-01-01T12:00:00.1234567+05:30' END)),__msduck_datediff_input(__msduck_datetimeoffset_cast_7(CASE WHEN nextval('offset_diff_b')%19=0 THEN NULL ELSE '2024-01-01T01:30:00.1234567-05:00' END))) FROM range(6000)"
            );
            let mut statement = db.prepare(&sql).unwrap();
            let values = statement
                .query_map([], |r| r.get::<_, Option<i64>>(0))
                .unwrap()
                .collect::<duckdb::Result<Vec<_>>>()
                .unwrap();
            assert_eq!(values.len(), 6000);
            let expected = (utc.ticks() - utc.round(scale).unwrap().ticks()) * 100;
            for (i, value) in values.into_iter().enumerate() {
                assert_eq!(
                    value,
                    if (i + 1) % 17 == 0 || (i + 1) % 19 == 0 {
                        None
                    } else {
                        Some(expected)
                    }
                );
            }
            for name in ["offset_diff_a", "offset_diff_b"] {
                assert_eq!(
                    db.query_row("SELECT currval(?)", [name], |r| r.get::<_, i64>(0))
                        .unwrap(),
                    6000
                );
            }
        }
    }

    #[test]
    fn boundaries_and_full_range() {
        let a = DateTime2::parse_iso("2005-12-31T23:59:59.9999999").unwrap();
        let b = DateTime2::parse_iso("2006-01-01T00:00:00").unwrap();
        for part in 0..=10 {
            let expected = if part == 10 { 100 } else { 1 };
            assert_eq!(difference(a, b, part), expected);
            assert_eq!(difference(b, a, part), -expected);
        }
        let first = DateTime2::from_ticks(0).unwrap();
        let last = DateTime2::parse_iso("9999-12-31T23:59:59.9999999").unwrap();
        assert_eq!(difference(first, last, 9), 315537897599999999);
        assert_eq!(difference(first, last, 10), 315537897599999999900);
        assert_eq!(difference(first, last, 0), 9998);
    }
    #[test]
    fn chunks_nulls_and_single_evaluation() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        for (part, unit) in [
            (0, "year"),
            (1, "quarter"),
            (2, "month"),
            (3, "day"),
            (5, "hour"),
            (6, "minute"),
            (7, "second"),
            (8, "millisecond"),
            (9, "microsecond"),
        ] {
            let sql = format!(
                "SELECT count(*) FROM (SELECT CASE WHEN n%17=0 THEN NULL ELSE TIMESTAMP '1999-12-31 23:59:59.999999'+n*INTERVAL 23 HOUR END a, CASE WHEN n%19=0 THEN NULL ELSE TIMESTAMP '2000-01-01'+n*INTERVAL 21 HOUR END b FROM range(6000) r(n)) WHERE __msduck_datediff_big_{part}(__msduck_datediff_input(a),__msduck_datediff_input(b)) IS DISTINCT FROM date_diff('{unit}',a,b)"
            );
            let wrong: i64 = db.query_row(&sql, [], |r| r.get(0)).unwrap();
            assert_eq!(wrong, 0, "{unit}");
        }
        db.execute_batch("CREATE SEQUENCE diff_a START 1; CREATE SEQUENCE diff_b START 2")
            .unwrap();
        let sum:i64=db.query_row("SELECT sum(__msduck_datediff_3(__msduck_datediff_input(nextval('diff_a')),__msduck_datediff_input(nextval('diff_b')))) FROM range(6000)",[],|r|r.get(0)).unwrap();
        assert_eq!(sum, 6000);
        let counts: (i64, i64) = db
            .query_row("SELECT currval('diff_a'),currval('diff_b')", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(counts, (6000, 6001));
    }
}
