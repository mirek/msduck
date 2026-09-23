//! SQL syntax and DuckDB adapters for deterministic JSON path extraction.
use duckdb::{
    core::{DataChunkHandle, Inserter, LogicalTypeId as Id},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use msduck_core::{diagnostic::SqlError, json_path::extract};
use sqlparser::ast::*;
/// Recover the native diagnostic after DuckDB adds its scalar-error wrapper.
/// Exact matching avoids treating user text embedded in unrelated errors as JSON.
pub fn diagnostic(message: &str) -> Option<SqlError> {
    let message = message
        .strip_prefix("Invalid Input Error: ")
        .unwrap_or(message);
    if let Some(diagnostic) = crate::openjson::diagnostic(message) {
        return Some(diagnostic);
    }
    msduck_core::json_path::diagnostic(message)
        .or_else(|| crate::string_escape::diagnostic(message))
}
pub fn lower(expr: &mut Expr) -> Result<(), String> {
    let Expr::Function(f) = expr else {
        return Ok(());
    };
    let name = f.name.to_string().to_ascii_uppercase();
    if !matches!(
        name.as_str(),
        "JSON_VALUE" | "JSON_QUERY" | "JSON_PATH_EXISTS"
    ) {
        return Ok(());
    }
    let FunctionArguments::List(args) = &f.args else {
        return Err(format!("invalid {name} arguments"));
    };
    let query = name == "JSON_QUERY";
    if args.duplicate_treatment.is_some()
        || !args.clauses.is_empty()
        || !matches!(f.parameters, FunctionArguments::None)
        || f.over.is_some()
        || f.filter.is_some()
        || f.null_treatment.is_some()
        || !f.within_group.is_empty()
    {
        return Err(format!("unsupported {name} modifiers"));
    }
    let (source, path) = match args.args.as_slice() {
        [FunctionArg::Unnamed(FunctionArgExpr::Expr(s))] if query => (
            s.clone(),
            Expr::Value(Value::SingleQuotedString("$".into()).into()),
        ),
        [
            FunctionArg::Unnamed(FunctionArgExpr::Expr(s)),
            FunctionArg::Unnamed(FunctionArgExpr::Expr(p)),
        ] => (s.clone(), p.clone()),
        _ => return Err(format!("invalid {name} arguments")),
    };
    let text = |e| Expr::Cast {
        kind: CastKind::Cast,
        expr: Box::new(e),
        data_type: DataType::Text,
        format: None,
    };
    *expr = crate::engine::binary_function(
        if query {
            "__msduck_json_query"
        } else if name == "JSON_PATH_EXISTS" {
            "__msduck_json_path_exists"
        } else {
            "__msduck_json_value"
        },
        text(source),
        text(path),
    );
    Ok(())
}
/// MODE: 0 scalar value, 1 container query, 2 existence predicate.
struct Extract<const MODE: u8>;
impl<const MODE: u8> VScalar for Extract<MODE> {
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
            let mut texts = Vec::with_capacity(2);
            for value in &values {
                // Exact VARCHAR signature; copied inline storage lives until text is copied.
                let mut text =
                    unsafe { value.as_slice_with_len::<duckdb::ffi::duckdb_string_t>(len)[row] };
                let bytes = unsafe {
                    std::slice::from_raw_parts(
                        duckdb::ffi::duckdb_string_t_data(&mut text).cast::<u8>(),
                        duckdb::ffi::duckdb_string_t_length(text) as usize,
                    )
                };
                texts.push(std::str::from_utf8(bytes)?.to_owned());
            }
            if MODE == 2 {
                let present = i32::from(msduck_core::json_path::exists(&texts[0], &texts[1]));
                // Existence has an exact INTEGER result, bounded by this chunk.
                unsafe {
                    result.as_mut_slice_with_len::<i32>(len)[row] = present;
                }
            } else if let Some(value) = extract(&texts[0], &texts[1], MODE == 1)? {
                result.insert(row, value.as_str());
            } else {
                result.set_null(row);
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![Id::Varchar.into(), Id::Varchar.into()],
            if MODE == 2 { Id::Integer } else { Id::Varchar }.into(),
        )]
    }
}
pub fn register(db: &duckdb::Connection) -> duckdb::Result<()> {
    db.register_scalar_function::<Extract<0>>("__msduck_json_value")?;
    db.register_scalar_function::<Extract<1>>("__msduck_json_query")?;
    db.register_scalar_function::<Extract<2>>("__msduck_json_path_exists")?;
    Ok(())
}
#[cfg(test)]
mod tests {
    #[test]
    fn existence_chunk_nulls_and_volatile_arguments() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        let wrong: i64 = db.query_row("SELECT count(*) FROM range(6000) r(i) WHERE __msduck_json_path_exists(CASE WHEN i%17=0 THEN NULL WHEN i%5=0 THEN '{bad}' ELSE '[{\"x\":null},{}]' END,CASE WHEN i%19=0 THEN NULL ELSE '$[*].x' END) IS DISTINCT FROM CASE WHEN i%17=0 OR i%19=0 THEN NULL WHEN i%5=0 THEN 0 ELSE 1 END", [], |r|r.get(0)).unwrap();
        assert_eq!(wrong, 0);
        db.execute_batch("CREATE SEQUENCE existence_source; CREATE SEQUENCE existence_path")
            .unwrap();
        let present: i64 = db.query_row("SELECT count(*) FROM range(6000) WHERE __msduck_json_path_exists(printf('[{\"x\":%d}]',nextval('existence_source')),CASE WHEN nextval('existence_path')>0 THEN '$[*].x' ELSE '$' END)=1", [], |r|r.get(0)).unwrap();
        assert_eq!(present, 6000);
        for name in ["existence_source", "existence_path"] {
            let calls: i64 = db
                .query_row("SELECT currval(?)", [name], |r| r.get(0))
                .unwrap();
            assert_eq!(calls, 6000);
        }
    }
    #[test]
    fn early_matches_across_chunks_and_nested_prefixes() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        let wrong:i64=db.query_row("SELECT count(*) FROM range(6000) r(i) WHERE __msduck_json_value(CASE WHEN i%17=0 THEN NULL ELSE printf('{\"a\":[{\"x\":%d,\"tail\":invalid}],\"rest\":invalid}',i) END,'$.a[0].x') IS DISTINCT FROM CASE WHEN i%17=0 THEN NULL ELSE CAST(i AS VARCHAR) END",[],|r|r.get(0)).unwrap();
        assert_eq!(wrong, 0);
    }

    #[test]
    fn native_chunk_nulls_and_volatile_inputs() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        let wrong:i64=db.query_row("SELECT count(*) FROM range(6000) r(i) WHERE __msduck_json_value(CASE WHEN i%17=0 THEN NULL ELSE printf('{\"n\":%d}',i) END,CASE WHEN i%19=0 THEN NULL ELSE '$.n' END) IS DISTINCT FROM CASE WHEN i%17=0 OR i%19=0 THEN NULL ELSE CAST(i AS VARCHAR) END",[],|r|r.get(0)).unwrap();
        assert_eq!(wrong, 0);
        db.execute_batch("CREATE SEQUENCE json_source; CREATE SEQUENCE json_path")
            .unwrap();
        let count:i64=db.query_row("SELECT count(__msduck_json_value(printf('{\"n\":%d}',nextval('json_source')),CASE WHEN nextval('json_path')>0 THEN '$.n' ELSE '$' END)) FROM range(6000)",[],|r|r.get(0)).unwrap();
        assert_eq!(count, 6000);
        for name in ["json_source", "json_path"] {
            assert_eq!(
                db.query_row("SELECT currval(?)", [name], |r| r.get::<_, i64>(0))
                    .unwrap(),
                6000
            );
        }
    }
}
