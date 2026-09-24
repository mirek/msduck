//! SQL Server JSON syntax validation, adapted from upstream json.ts lexical rules.
//! An explicit grammar stack avoids recursion and numeric conversion limits.
use duckdb::{
    core::{DataChunkHandle, LogicalTypeId as Id},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use sqlparser::ast::*;

use msduck_core::json::valid;

pub fn lower(expr: &mut Expr) -> Result<(), String> {
    let Expr::Function(f) = expr else {
        return Ok(());
    };
    if !f.name.to_string().eq_ignore_ascii_case("ISJSON") {
        return Ok(());
    }
    let FunctionArguments::List(args) = &f.args else {
        return Err("ISJSON requires one or two scalar arguments".into());
    };
    if !(1..=2).contains(&args.args.len())
        || args.duplicate_treatment.is_some()
        || !args.clauses.is_empty()
        || !matches!(f.parameters, FunctionArguments::None)
        || f.over.is_some()
        || f.filter.is_some()
        || f.null_treatment.is_some()
        || !f.within_group.is_empty()
    {
        return Err("unsupported ISJSON arguments or modifiers".into());
    }
    let FunctionArg::Unnamed(FunctionArgExpr::Expr(value)) = &args.args[0] else {
        return Err("ISJSON requires a scalar input".into());
    };
    let mode = if let Some(arg) = args.args.get(1) {
        let FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Identifier(kind))) = arg else {
            return Err("ISJSON requires a JSON type keyword".into());
        };
        match kind.value.to_ascii_uppercase().as_str() {
            "VALUE" => 1,
            "ARRAY" => 2,
            "OBJECT" => 3,
            "SCALAR" => 4,
            _ => return Err("invalid ISJSON type constraint".into()),
        }
    } else {
        0
    };
    *expr = crate::engine::unary_function(
        &format!("__msduck_isjson_{mode}"),
        crate::engine::unary_function("__msduck_carrier_input", value.clone()),
    );
    Ok(())
}
struct IsJson<const MODE: u8>;
impl<const MODE: u8> VScalar for IsJson<MODE> {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let source = input.flat_vector(0);
        let structure = crate::unicode_carrier::is_logical(&source.logical_type())
            .then(|| input.struct_vector(0));
        let stored = structure.as_ref().map(|s| s.child(0, len));
        let mut result = output.flat_vector();
        for row in 0..len {
            if source.row_is_null(row as u64) {
                result.set_null(row);
                continue;
            }
            let value = if let Some(stored) = &stored {
                if stored.row_is_null(row as u64) {
                    return Err("invalid Unicode carrier: NULL payload".into());
                }
                let bytes = crate::unicode_carrier::bytes(stored, row, len)?;
                if bytes.len() % 2 != 0 {
                    return Err("invalid Unicode carrier byte length".into());
                }
                let units: Vec<_> = bytes
                    .chunks_exact(2)
                    .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                    .collect();
                i32::from(msduck_core::json::valid_utf16(&units, MODE))
            } else {
                // Exact VARCHAR signature: copied inline storage stays alive.
                let mut text =
                    unsafe { source.as_slice_with_len::<duckdb::ffi::duckdb_string_t>(len)[row] };
                let bytes = unsafe {
                    std::slice::from_raw_parts(
                        duckdb::ffi::duckdb_string_t_data(&mut text).cast::<u8>(),
                        duckdb::ffi::duckdb_string_t_length(text) as usize,
                    )
                };
                i32::from(valid(bytes, MODE))
            };
            unsafe {
                result.as_mut_slice_with_len::<i32>(len)[row] = value;
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![
            ScalarFunctionSignature::exact(vec![Id::Varchar.into()], Id::Integer.into()),
            ScalarFunctionSignature::exact(
                vec![crate::unicode_carrier::kind()],
                Id::Integer.into(),
            ),
        ]
    }
}
pub fn register(db: &duckdb::Connection) -> duckdb::Result<()> {
    macro_rules! register {($($m:literal),*)=>{$(db.register_scalar_function::<IsJson<$m>>(concat!("__msduck_isjson_",stringify!($m)))?;)*};}
    register!(0, 1, 2, 3, 4);
    Ok(())
}
#[cfg(test)]
mod tests {
    #[test]
    fn chunks_nulls_and_single_evaluation() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        let wrong:i64=db.query_row("SELECT count(*) FROM range(6000) r(i) WHERE __msduck_isjson_0(CASE i%4 WHEN 0 THEN NULL WHEN 1 THEN '{\"a\":1}' WHEN 2 THEN '[1,]' ELSE '42' END) IS DISTINCT FROM CASE i%4 WHEN 0 THEN NULL WHEN 1 THEN 1 ELSE 0 END",[],|r|r.get(0)).unwrap();
        assert_eq!(wrong, 0);
        db.execute_batch("CREATE SEQUENCE json_calls").unwrap();
        let count:i64=db.query_row("SELECT count(*) FROM range(6000) WHERE __msduck_isjson_0(printf('[%d]',nextval('json_calls')))=1",[],|r|r.get(0)).unwrap();
        assert_eq!(count, 6000);
        assert_eq!(
            db.query_row("SELECT currval('json_calls')", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            6000
        );
    }
}
