//! Exact DATETIME2 constructors with a compile-time fractional scale.
use duckdb::{
    core::{DataChunkHandle, LogicalTypeHandle, LogicalTypeId as Id},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use sqlparser::ast::*;

pub const INVALID: &str =
    "Cannot construct data type datetime2, some of the arguments have values which are not valid.";

pub use msduck_sql::expression_metadata::temporal::datetime2fromparts_scale as scale;

pub fn lower(expr: &mut Expr) -> Result<(), String> {
    let Expr::Function(f) = expr else {
        return Ok(());
    };
    if !f
        .name
        .to_string()
        .eq_ignore_ascii_case("DATETIME2FROMPARTS")
    {
        return Ok(());
    }
    let FunctionArguments::List(args) = &f.args else {
        return Err("DATETIME2FROMPARTS requires eight scalar arguments".into());
    };
    if args.args.len() != 8
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
        return Err("DATETIME2FROMPARTS requires eight scalar arguments without modifiers".into());
    }
    let scale = scale(f).ok_or(INVALID_SCALE)?;
    f.name = ObjectName::from(vec![Ident::new(format!(
        "__msduck_datetime2fromparts_{scale}"
    ))]);
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

pub const INVALID_SCALE: &str = "Scale argument is not valid. Valid expressions for data type datetime2 scale argument are integer constants and integer constant expressions.";

pub(crate) fn ticks(parts: [i32; 7], scale: u8) -> Result<i64, &'static str> {
    let [year, month, day, hour, minute, second, fraction] = parts;
    let days = crate::scalar::date_days(year, month, day).map_err(|_| INVALID)?;
    let nanos = crate::timefromparts::nanos([hour, minute, second, fraction], scale)
        .map_err(|_| INVALID)?;
    Ok((i64::from(days) + 719162) * 864_000_000_000 + nanos / 100)
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
        let values = (0..7).map(|i| input.flat_vector(i)).collect::<Vec<_>>();
        let mut result = output.struct_vector();
        let mut child = result.child(0, len);
        for row in 0..len {
            if values.iter().any(|v| v.row_is_null(row as u64)) {
                result.set_null(row);
                child.set_null(row);
                continue;
            }
            // Exact INTEGER signatures establish input width; the result child uses i64.
            let parts =
                std::array::from_fn(|i| unsafe { values[i].as_slice_with_len::<i32>(len)[row] });
            let value = ticks(parts, SCALE)?;
            unsafe {
                child.as_mut_slice_with_len::<i64>(len)[row] = value;
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            (0..7).map(|_| Id::Integer.into()).collect(),
            LogicalTypeHandle::struct_type(&[(
                &format!("__msduck_datetime2_{SCALE}"),
                Id::Bigint.into(),
            )]),
        )]
    }
}
pub fn register(db: &duckdb::Connection) -> duckdb::Result<()> {
    macro_rules! register { ($($s:literal),*) => { $(db.register_scalar_function::<Parts<$s>>(concat!("__msduck_datetime2fromparts_",stringify!($s)))?;)* }; }
    register!(0, 1, 2, 3, 4, 5, 6, 7);
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn calendar_fraction_bounds_and_vector_evaluation() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        for scale in 0..=7 {
            let maximum = 10i32.pow(scale) - 1;
            assert_eq!(ticks([1, 1, 1, 0, 0, 0, 0], scale as u8).unwrap(), 0);
            assert_eq!(
                ticks([9999, 12, 31, 23, 59, 59, maximum], scale as u8).unwrap(),
                3_155_378_976_000_000_000 - 10i64.pow(7 - scale)
            );
            for parts in [
                [0, 1, 1, 0, 0, 0, 0],
                [10000, 1, 1, 0, 0, 0, 0],
                [2023, 2, 29, 0, 0, 0, 0],
                [1900, 2, 29, 0, 0, 0, 0],
                [2024, 13, 1, 0, 0, 0, 0],
                [2024, 1, 0, 0, 0, 0, 0],
                [2024, 1, 1, 24, 0, 0, 0],
                [2024, 1, 1, 0, 60, 0, 0],
                [2024, 1, 1, 0, 0, 60, 0],
                [2024, 1, 1, 0, 0, 0, -1],
                [2024, 1, 1, 0, 0, 0, maximum + 1],
            ] {
                assert_eq!(ticks(parts, scale as u8), Err(INVALID));
            }
            let sql = format!(
                "SELECT DATETIME2FROMPARTS(CASE WHEN i%17=0 THEN NULL ELSE 2000 END,2,29,i%24,i%60,i%60,i%{}, {scale}) d INTO dbo.dt2parts_{scale} FROM range(6000) r(i)",
                maximum + 1
            );
            assert!(
                session
                    .batch_response(&sql, &Default::default(), false, None)
                    .1
            );
            let base = ticks([2000, 2, 29, 0, 0, 0, 0], scale as u8).unwrap();
            let wrong:i64=session.db.query_row(&format!("SELECT count(*) FROM dbo.dt2parts_{scale} p JOIN range(6000) r(i) ON p.rowid=i WHERE d.__msduck_datetime2_{scale} IS DISTINCT FROM CASE WHEN i%17=0 THEN NULL ELSE {base}+((i%24)*3600+(i%60)*60+i%60)*10000000+(i%{})*{} END",maximum+1,10i64.pow(7-scale)),[],|r|r.get(0)).unwrap();
            assert_eq!(wrong, 0);
        }
        for i in 0..7 {
            session
                .db
                .execute_batch(&format!("CREATE SEQUENCE dt2parts_{i}"))
                .unwrap();
        }
        assert!(session.batch_response("SELECT DATETIME2FROMPARTS(2000+nextval('dt2parts_0')%20,1+nextval('dt2parts_1')%12,1+nextval('dt2parts_2')%28,nextval('dt2parts_3')%24,nextval('dt2parts_4')%60,nextval('dt2parts_5')%60,nextval('dt2parts_6')%100,2) d INTO dbo.dt2parts_calls FROM range(6000)",&Default::default(),false,None).1);
        for i in 0..7 {
            assert_eq!(
                session
                    .db
                    .query_row("SELECT currval(?)", [format!("dt2parts_{i}")], |r| r
                        .get::<_, i64>(0))
                    .unwrap(),
                6000
            );
        }
    }
}
