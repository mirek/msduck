//! Exact TIME constructors with a compile-time fractional scale.
use duckdb::{
    core::{DataChunkHandle, LogicalTypeId as Id},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use sqlparser::ast::*;

pub const INVALID: &str =
    "Cannot construct data type time, some of the arguments have values which are not valid.";

pub const INVALID_SCALE: &str = "Scale argument is not valid. Valid expressions for data type time scale argument are integer constants and integer constant expressions.";

pub use msduck_sql::expression_metadata::temporal::timefromparts_scale as scale;

pub fn lower(expr: &mut Expr) -> Result<(), String> {
    let Expr::Function(f) = expr else {
        return Ok(());
    };
    if !f.name.to_string().eq_ignore_ascii_case("TIMEFROMPARTS") {
        return Ok(());
    }
    let FunctionArguments::List(args) = &f.args else {
        return Err("TIMEFROMPARTS requires five scalar arguments".into());
    };
    if args.args.len() != 5
        || args
            .args
            .iter()
            .any(|a| !matches!(a, FunctionArg::Unnamed(FunctionArgExpr::Expr(_))))
        || args.duplicate_treatment.is_some()
        || !args.clauses.is_empty()
        || !matches!(f.parameters, FunctionArguments::None)
        || f.over.is_some()
        || f.filter.is_some()
        || f.null_treatment.is_some()
        || !f.within_group.is_empty()
    {
        return Err("TIMEFROMPARTS requires five scalar arguments without modifiers".into());
    }
    let scale = scale(f).ok_or(INVALID_SCALE)?;
    f.name = ObjectName::from(vec![Ident::new(format!("__msduck_timefromparts_{scale}"))]);
    let FunctionArguments::List(args) = &mut f.args else {
        unreachable!()
    };
    args.args.pop();
    for arg in &mut args.args {
        let FunctionArg::Unnamed(FunctionArgExpr::Expr(value)) = arg else {
            unreachable!()
        };
        *value = crate::assignment::convert(value.clone(), &DataType::Int(None), false);
    }
    Ok(())
}

pub(crate) fn nanos(parts: [i32; 4], scale: u8) -> Result<i64, &'static str> {
    let [hour, minute, second, fraction] = parts;
    if !(0..24).contains(&hour)
        || !(0..60).contains(&minute)
        || !(0..60).contains(&second)
        || !(0..10i32.pow(u32::from(scale))).contains(&fraction)
    {
        return Err(INVALID);
    }
    Ok(
        (i64::from(hour) * 3600 + i64::from(minute) * 60 + i64::from(second)) * 1_000_000_000
            + i64::from(fraction) * 10i64.pow(9 - u32::from(scale)),
    )
}
struct Parts<const SCALE: u8>;
impl<const SCALE: u8> VScalar for Parts<SCALE> {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let values = (0..4).map(|i| input.flat_vector(i)).collect::<Vec<_>>();
        let mut result = output.flat_vector();
        for row in 0..len {
            if values.iter().any(|v| v.row_is_null(row as u64)) {
                result.set_null(row);
                continue;
            }
            // Exact INTEGER signatures establish input width; TIME_NS uses i64.
            let parts =
                std::array::from_fn(|i| unsafe { values[i].as_slice_with_len::<i32>(len)[row] });
            let value = nanos(parts, SCALE)?;
            unsafe {
                result.as_mut_slice_with_len::<i64>(len)[row] = value;
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            (0..4).map(|_| Id::Integer.into()).collect(),
            Id::TimeNs.into(),
        )]
    }
}
pub fn register(db: &duckdb::Connection) -> duckdb::Result<()> {
    macro_rules! register { ($($s:literal),*) => { $(db.register_scalar_function::<Parts<$s>>(concat!("__msduck_timefromparts_",stringify!($s)))?;)* }; }
    register!(0, 1, 2, 3, 4, 5, 6, 7);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn precision_ranges_nulls_and_single_evaluation() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        for scale in 0..=7 {
            let maximum = 10i32.pow(scale) - 1;
            assert_eq!(
                nanos([23, 59, 59, maximum], scale as u8).unwrap(),
                86_400_000_000_000 - 10i64.pow(9 - scale)
            );
            for parts in [
                [24, 0, 0, 0],
                [-1, 0, 0, 0],
                [0, 60, 0, 0],
                [0, 0, 60, 0],
                [0, 0, 0, -1],
                [0, 0, 0, maximum + 1],
            ] {
                assert_eq!(nanos(parts, scale as u8), Err(INVALID));
            }
            let sql = format!(
                "SELECT TIMEFROMPARTS(CASE WHEN i%17=0 THEN NULL ELSE i%24 END,i%60,i%60,i%{}, {scale}) t INTO dbo.parts_{scale} FROM range(6000) r(i)",
                maximum + 1
            );
            assert!(
                session
                    .batch_response(&sql, &Default::default(), false, None)
                    .1
            );
            let wrong:i64=session.db.query_row(&format!("SELECT count(*) FROM dbo.parts_{scale} p JOIN range(6000) r(i) ON p.rowid=i WHERE epoch_ns(t) IS DISTINCT FROM CASE WHEN i%17=0 THEN NULL ELSE ((i%24)*3600+(i%60)*60+i%60)*1000000000+(i%{})*{} END",maximum+1,10i64.pow(9-scale)),[],|r|r.get(0)).unwrap();
            assert_eq!(wrong, 0);
        }
        for name in ["parts_h", "parts_m", "parts_s", "parts_f"] {
            session
                .db
                .execute_batch(&format!("CREATE SEQUENCE {name}"))
                .unwrap();
        }
        assert!(session.batch_response("SELECT TIMEFROMPARTS(nextval('parts_h')%24,nextval('parts_m')%60,nextval('parts_s')%60,nextval('parts_f')%100,2) t INTO dbo.parts_calls FROM range(6000)",&Default::default(),false,None).1);
        for name in ["parts_h", "parts_m", "parts_s", "parts_f"] {
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
