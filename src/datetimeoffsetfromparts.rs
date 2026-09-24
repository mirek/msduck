//! Exact DATETIMEOFFSET constructors with a compile-time fractional scale.
use duckdb::{
    core::{DataChunkHandle, LogicalTypeHandle, LogicalTypeId as Id},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use sqlparser::ast::*;

pub const INVALID: &str = "Cannot construct data type datetimeoffset, some of the arguments have values which are not valid.";

pub use msduck_sql::expression_metadata::temporal::datetimeoffsetfromparts_scale as scale;

pub fn lower(expr: &mut Expr) -> Result<(), String> {
    let Expr::Function(f) = expr else {
        return Ok(());
    };
    if !f
        .name
        .to_string()
        .eq_ignore_ascii_case("DATETIMEOFFSETFROMPARTS")
    {
        return Ok(());
    }
    let FunctionArguments::List(args) = &f.args else {
        return Err("DATETIMEOFFSETFROMPARTS requires ten scalar arguments".into());
    };
    if args.args.len() != 10
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
        return Err(
            "DATETIMEOFFSETFROMPARTS requires ten scalar arguments without modifiers".into(),
        );
    }
    let scale = scale(f).ok_or(INVALID_SCALE)?;
    f.name = ObjectName::from(vec![Ident::new(format!(
        "__msduck_datetimeoffsetfromparts_{scale}"
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

pub const INVALID_SCALE: &str = "Scale argument is not valid. Valid expressions for data type datetimeoffset scale argument are integer constants and integer constant expressions.";

fn construct(
    parts: [i32; 9],
    scale: u8,
) -> Result<crate::datetimeoffset::DateTimeOffset, &'static str> {
    let [y, m, d, h, mi, s, f, oh, om] = parts;
    if !(-14..=14).contains(&oh)
        || !(-59..=59).contains(&om)
        || (oh > 0 && om < 0)
        || (oh < 0 && om > 0)
        || (oh.abs() == 14 && om != 0)
    {
        return Err(INVALID);
    }
    let ticks =
        crate::datetime2fromparts::ticks([y, m, d, h, mi, s, f], scale).map_err(|_| INVALID)?;
    let local = crate::datetime2::DateTime2::from_ticks(ticks).map_err(|_| INVALID)?;
    crate::datetimeoffset::DateTimeOffset::from_local(local, (oh * 60 + om) as i16)
        .map_err(|_| INVALID)
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
        let values = (0..9).map(|i| input.flat_vector(i)).collect::<Vec<_>>();
        let mut result = output.struct_vector();
        let mut child = result.child(0, len);
        let mut offset = result.child(1, len);
        for row in 0..len {
            if values.iter().any(|v| v.row_is_null(row as u64)) {
                result.set_null(row);
                child.set_null(row);
                offset.set_null(row);
                continue;
            }
            // Exact INTEGER signatures establish input width; the result child uses i64.
            let parts =
                std::array::from_fn(|i| unsafe { values[i].as_slice_with_len::<i32>(len)[row] });
            let value = construct(parts, SCALE)?;
            unsafe {
                child.as_mut_slice_with_len::<i64>(len)[row] = value.utc().ticks();
                offset.as_mut_slice_with_len::<i16>(len)[row] = value.offset_minutes();
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            (0..9).map(|_| Id::Integer.into()).collect(),
            LogicalTypeHandle::struct_type(&[
                (
                    &format!("__msduck_datetimeoffset_{SCALE}"),
                    Id::Bigint.into(),
                ),
                ("__msduck_offset_minutes", Id::Smallint.into()),
            ]),
        )]
    }
}
pub fn register(db: &duckdb::Connection) -> duckdb::Result<()> {
    macro_rules! register { ($($s:literal),*) => { $(db.register_scalar_function::<Parts<$s>>(concat!("__msduck_datetimeoffsetfromparts_",stringify!($s)))?;)* }; }
    register!(0, 1, 2, 3, 4, 5, 6, 7);
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn offsets_scales_nulls_and_single_evaluation() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        for scale in 0..=7 {
            let maximum = 10i32.pow(scale) - 1;
            let base =
                crate::datetime2fromparts::ticks([2024, 1, 1, 12, 0, 0, maximum], scale as u8)
                    .unwrap();
            for minutes in -840..=840 {
                let value = construct(
                    [2024, 1, 1, 12, 0, 0, maximum, minutes / 60, minutes % 60],
                    scale as u8,
                )
                .unwrap();
                assert_eq!(value.utc().ticks(), base - i64::from(minutes) * 600_000_000);
                assert_eq!(value.offset_minutes(), minutes as i16);
            }
            let sql = format!(
                "SELECT DATETIMEOFFSETFROMPARTS(CASE WHEN i%17=0 THEN NULL ELSE 2024 END,1,1,12,0,0,{maximum},CAST((i%1681-840)/60 AS INT),CAST(CASE WHEN i%19=0 THEN NULL ELSE (i%1681-840)%60 END AS INT),{scale}) d INTO dbo.offsetparts_{scale} FROM range(6000) r(i)"
            );
            assert!(
                session
                    .batch_response(&sql, &Default::default(), false, None)
                    .1
            );
            let wrong:i64=session.db.query_row(&format!("SELECT count(*) FROM dbo.offsetparts_{scale} p JOIN range(6000) r(i) ON p.rowid=i WHERE d.__msduck_datetimeoffset_{scale} IS DISTINCT FROM CASE WHEN i%17=0 OR i%19=0 THEN NULL ELSE {base}-(i%1681-840)*600000000 END OR d.__msduck_offset_minutes IS DISTINCT FROM CASE WHEN i%17=0 OR i%19=0 THEN NULL ELSE i%1681-840 END"),[],|r|r.get(0)).unwrap();
            assert_eq!(wrong, 0);
        }
        for (h, m) in [
            (1, -1),
            (-1, 1),
            (14, 1),
            (-14, -1),
            (15, 0),
            (0, 60),
            (i32::MIN, 0),
        ] {
            assert_eq!(construct([2024, 1, 1, 0, 0, 0, 0, h, m], 7), Err(INVALID));
        }
        assert_eq!(construct([1, 1, 1, 0, 0, 0, 0, 0, 1], 7), Err(INVALID));
        assert_eq!(
            construct([9999, 12, 31, 23, 59, 59, 9999999, 0, -1], 7),
            Err(INVALID)
        );
        for i in 0..9 {
            session
                .db
                .execute_batch(&format!("CREATE SEQUENCE offsetpart_arg_{i}"))
                .unwrap();
        }
        assert!(session.batch_response("SELECT DATETIMEOFFSETFROMPARTS(2000+nextval('offsetpart_arg_0')%20,1+nextval('offsetpart_arg_1')%12,1+nextval('offsetpart_arg_2')%28,nextval('offsetpart_arg_3')%24,nextval('offsetpart_arg_4')%60,nextval('offsetpart_arg_5')%60,nextval('offsetpart_arg_6')%100,nextval('offsetpart_arg_7')%14,nextval('offsetpart_arg_8')%60,2) d INTO dbo.offsetparts_calls FROM range(6000)",&Default::default(),false,None).1);
        for i in 0..9 {
            assert_eq!(
                session
                    .db
                    .query_row("SELECT currval(?)", [format!("offsetpart_arg_{i}")], |r| {
                        r.get::<_, i64>(0)
                    })
                    .unwrap(),
                6000
            );
        }
    }
}
