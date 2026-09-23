//! SQL declarations and DuckDB vectors for deterministic JSON string escaping.
use duckdb::{
    core::{DataChunkHandle, Inserter, LogicalTypeId as Id},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use msduck_core::{diagnostic::SqlError, json_escape};
use sqlparser::ast::*;

pub const ARITY: &str = "The string_escape function requires 2 argument(s).";
pub const NULL_FORMAT: &str =
    "Argument data type NULL is invalid for argument 2 of string_escape function.";

pub fn diagnostic(message: &str) -> Option<SqlError> {
    (message == json_escape::INVALID_FORMAT).then(|| SqlError::new(13622, 1, message))
}
pub fn lower(expr: &mut Expr) -> Result<(), String> {
    let Expr::Function(f) = expr else {
        return Ok(());
    };
    if !f.name.to_string().eq_ignore_ascii_case("STRING_ESCAPE") {
        return Ok(());
    }
    let FunctionArguments::List(args) = &f.args else {
        return Err(ARITY.into());
    };
    if args.args.len() != 2 {
        return Err(ARITY.into());
    }
    if args.duplicate_treatment.is_some()
        || !args.clauses.is_empty()
        || !matches!(f.parameters, FunctionArguments::None)
        || f.over.is_some()
        || f.filter.is_some()
        || f.null_treatment.is_some()
        || !f.within_group.is_empty()
    {
        return Err("unsupported STRING_ESCAPE modifiers".into());
    }
    let [
        FunctionArg::Unnamed(FunctionArgExpr::Expr(source)),
        FunctionArg::Unnamed(FunctionArgExpr::Expr(format)),
    ] = args.args.as_slice()
    else {
        return Err("unsupported STRING_ESCAPE arguments".into());
    };
    let mut unwrapped = format;
    while let Expr::Nested(inner) = unwrapped {
        unwrapped = inner;
    }
    if matches!(unwrapped, Expr::Value(value) if matches!(value.value, Value::Null)) {
        return Err(NULL_FORMAT.into());
    }
    let text = |expr: &Expr| Expr::Cast {
        kind: CastKind::Cast,
        expr: Box::new(expr.clone()),
        data_type: DataType::Text,
        format: None,
    };
    *expr = crate::engine::binary_function("__msduck_string_escape", text(source), text(format));
    Ok(())
}
pub struct Escape;
impl VScalar for Escape {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let values = [input.flat_vector(0), input.flat_vector(1)];
        let mut result = output.flat_vector();
        for row in 0..len {
            if values.iter().any(|v| v.row_is_null(row as u64)) {
                result.set_null(row);
                continue;
            }
            let mut texts = [String::new(), String::new()];
            for (index, value) in values.iter().enumerate() {
                // Exact VARCHAR inputs. Copy inline storage before borrowing its bytes.
                let mut value =
                    unsafe { value.as_slice_with_len::<duckdb::ffi::duckdb_string_t>(len)[row] };
                let bytes = unsafe {
                    std::slice::from_raw_parts(
                        duckdb::ffi::duckdb_string_t_data(&mut value).cast::<u8>(),
                        duckdb::ffi::duckdb_string_t_length(value) as usize,
                    )
                };
                texts[index] = std::str::from_utf8(bytes)?.to_owned();
            }
            let escaped = json_escape::escape(&texts[0], &texts[1])?;
            result.insert(row, escaped.as_ref());
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![Id::Varchar.into(), Id::Varchar.into()],
            Id::Varchar.into(),
        )]
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn chunk_nulls_and_single_evaluation() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        let wrong:i64=db.query_row("SELECT count(*) FROM range(6000) r(i) WHERE __msduck_string_escape(CASE WHEN i%17=0 THEN NULL ELSE '/雪' END,CASE WHEN i%19=0 THEN NULL ELSE 'json' END) IS DISTINCT FROM CASE WHEN i%17=0 OR i%19=0 THEN NULL ELSE '\\/雪' END",[],|r|r.get(0)).unwrap();
        assert_eq!(wrong, 0);
        db.execute_batch("CREATE SEQUENCE escape_source; CREATE SEQUENCE escape_format")
            .unwrap();
        let n:i64=db.query_row("SELECT count(*) FROM range(6000) WHERE __msduck_string_escape(printf('/%d',nextval('escape_source')),CASE WHEN nextval('escape_format')>0 THEN 'json' ELSE 'xml' END) LIKE '\\/%'",[],|r|r.get(0)).unwrap();
        assert_eq!(n, 6000);
        for name in ["escape_source", "escape_format"] {
            let calls: i64 = db
                .query_row("SELECT currval(?)", [name], |r| r.get(0))
                .unwrap();
            assert_eq!(calls, 6000);
        }
    }
}
