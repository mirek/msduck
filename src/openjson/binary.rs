//! OPENJSON Base64 string conversion, separate from ordinary binary casts.
use super::*;
use msduck_core::openjson::binary::convert;
struct Binary<const FIXED: bool>;
impl<const FIXED: bool> VScalar for Binary<FIXED> {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let width = input.flat_vector(2);
        let mut result = output.flat_vector();
        for row in 0..input.len() {
            if width.row_is_null(row as u64) {
                result.set_null(row);
                continue;
            }
            // Exact INTEGER third argument, bounded by the input chunk length.
            let width = unsafe { width.as_slice_with_len::<i32>(input.len())[row] };
            if let Some([source, path]) = schema::texts(input, row)?
                && let Some(bytes) = convert(&source, &path, width, FIXED)?
            {
                result.insert(row, bytes.as_slice());
            } else {
                result.set_null(row);
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![Id::Varchar.into(), Id::Varchar.into(), Id::Integer.into()],
            Id::Blob.into(),
        )]
    }
}
pub(super) fn register(db: &duckdb::Connection) -> duckdb::Result<()> {
    db.register_scalar_function::<Binary<false>>("__msduck_openjson_varbinary")?;
    db.register_scalar_function::<Binary<true>>("__msduck_openjson_binary")
}
pub(super) fn expression(
    source: Expr,
    path: Expr,
    kind: &DataType,
) -> Result<Option<Expr>, String> {
    let (fixed, width) = match kind {
        DataType::Binary(n) => (true, n.unwrap_or(1)),
        DataType::Varbinary(Some(BinaryLength::Max)) => (false, 0),
        DataType::Varbinary(Some(BinaryLength::IntegerLength { length })) => (false, *length),
        DataType::Varbinary(None) => (false, 1),
        _ => return Ok(None),
    };
    if width > 8000 || width == 0 && !matches!(kind, DataType::Varbinary(Some(BinaryLength::Max))) {
        return Err("invalid OPENJSON binary width".into());
    };
    let mut expr = crate::engine::binary_function(
        if fixed {
            "__msduck_openjson_binary"
        } else {
            "__msduck_openjson_varbinary"
        },
        source,
        path,
    );
    let Expr::Function(f) = &mut expr else {
        unreachable!()
    };
    let FunctionArguments::List(args) = &mut f.args else {
        unreachable!()
    };
    args.args
        .push(FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(
            Value::Number(
                if width == 0 {
                    "-1".into()
                } else {
                    width.to_string()
                },
                false,
            )
            .into(),
        ))));
    Ok(Some(expr))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_vectors_and_binary_bytes() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        register(&db).unwrap();
        db.execute_batch("CREATE SEQUENCE binary_calls").unwrap();
        let wrong:i64=db.query_row("SELECT count(*) FROM (SELECT __msduck_openjson_varbinary(CASE WHEN nextval('binary_calls')%17=0 THEN NULL ELSE '\"AP8B\"' END,'$',-1) b,i FROM range(6000) r(i)) WHERE b IS DISTINCT FROM CASE WHEN (i+1)%17=0 THEN NULL ELSE from_hex('00FF01') END",[],|r|r.get(0)).unwrap();
        assert_eq!(wrong, 0);
        let calls: i64 = db
            .query_row("SELECT currval('binary_calls')", [], |r| r.get(0))
            .unwrap();
        assert_eq!(calls, 6000);
    }
}
