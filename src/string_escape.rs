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
pub const TYPED_NULL_FORMAT: &str =
    "Argument data type NULL is invalid for argument 2 of STRING_ESCAPE function.";

pub fn diagnostic(message: &str) -> Option<SqlError> {
    let message = message
        .strip_prefix("Invalid Input Error: ")
        .unwrap_or(message);
    match message {
        json_escape::INVALID_FORMAT => Some(SqlError::new(13622, 1, message)),
        TYPED_NULL_FORMAT => Some(SqlError::new(8116, 8, message)),
        _ => None,
    }
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
    let text = |expr: &Expr| crate::engine::unary_function("__msduck_carrier_input", expr.clone());
    *expr = crate::engine::binary_function("__msduck_string_escape", text(source), text(format));
    Ok(())
}
pub struct Escape;
impl VScalar for Escape {
    type State = ();
    fn special_null_handling() -> bool {
        true
    }
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if crate::unicode_carrier::is_logical(&input.flat_vector(0).logical_type()) {
            return escape_unicode(input, output);
        }
        let len = input.len();
        let values = [input.flat_vector(0), input.flat_vector(1)];
        let mut result = output.flat_vector();
        for row in 0..len {
            if values[1].row_is_null(row as u64) {
                return Err(TYPED_NULL_FORMAT.into());
            }
            let mut texts = [String::new(), String::new()];
            for (index, value) in values.iter().enumerate() {
                if index == 0 && value.row_is_null(row as u64) {
                    continue;
                }
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
            json_escape::escape("", &texts[1])?;
            if values[0].row_is_null(row as u64) {
                result.set_null(row);
                continue;
            }
            let escaped = json_escape::escape(&texts[0], &texts[1])?;
            result.insert(row, escaped.as_ref());
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![
            ScalarFunctionSignature::exact(
                vec![Id::Varchar.into(), Id::Varchar.into()],
                Id::Varchar.into(),
            ),
            ScalarFunctionSignature::exact(
                vec![
                    crate::unicode_carrier::kind(),
                    crate::unicode_carrier::kind(),
                ],
                crate::unicode_carrier::kind(),
            ),
        ]
    }
}
fn escape_unicode(
    input: &mut DataChunkHandle,
    output: &mut dyn WritableVector,
) -> Result<(), Box<dyn std::error::Error>> {
    use crate::unicode_carrier::{CELL_LIMIT, CHUNK_LIMIT, bytes};
    let len = input.len();
    let parents = [input.flat_vector(0), input.flat_vector(1)];
    let structures = [input.struct_vector(0), input.struct_vector(1)];
    let values = [structures[0].child(0, len), structures[1].child(0, len)];
    // Retain bounded output buffers until DuckDB has copied every BLOB.
    let mut encoded = Vec::with_capacity(len);
    let mut remaining = CHUNK_LIMIT;
    for row in 0..len {
        if parents[1].row_is_null(row as u64) {
            return Err(TYPED_NULL_FORMAT.into());
        }
        let mut units = [Vec::new(), Vec::new()];
        for (index, value) in values.iter().enumerate() {
            if index == 0 && parents[0].row_is_null(row as u64) {
                continue;
            }
            if value.row_is_null(row as u64) {
                return Err("invalid Unicode carrier: NULL payload".into());
            }
            let raw = bytes(value, row, len)?;
            if raw.len() % 2 != 0 {
                return Err("invalid Unicode carrier byte length".into());
            }
            units[index] = raw
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect();
        }
        // Validate the format before reporting any resource limit, without
        // allocating the escaped text. An empty source has the same format rule.
        json_escape::escape_utf16(&[], &units[1])?;
        if parents[0].row_is_null(row as u64) {
            encoded.push(None);
            continue;
        }
        let size: usize = units[0]
            .iter()
            .map(|&unit| match unit {
                8 | 9 | 10 | 12 | 13 | 34 | 47 | 92 => 4,
                0..=31 => 12,
                _ => 2,
            })
            .sum();
        if size > CELL_LIMIT || size > remaining {
            return Err("STRING_ESCAPE result exceeds the configured output limit".into());
        }
        remaining -= size;
        let escaped = json_escape::escape_utf16(&units[0], &units[1])?;
        encoded.push(Some(
            escaped
                .iter()
                .flat_map(|unit| unit.to_le_bytes())
                .collect::<Vec<_>>(),
        ));
    }
    let mut result = output.struct_vector();
    let mut payload = result.child(0, len);
    for (row, value) in encoded.iter().enumerate() {
        if let Some(value) = value {
            payload.insert(row, value.as_slice());
        } else {
            result.set_null(row);
            payload.set_null(row);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_runtime_diagnostics() {
        assert_eq!(
            diagnostic(TYPED_NULL_FORMAT),
            Some(SqlError::new(8116, 8, TYPED_NULL_FORMAT))
        );
        assert_eq!(
            diagnostic(&format!("Invalid Input Error: {TYPED_NULL_FORMAT}")),
            Some(SqlError::new(8116, 8, TYPED_NULL_FORMAT))
        );
        assert_eq!(
            diagnostic(json_escape::INVALID_FORMAT),
            Some(SqlError::new(13622, 1, json_escape::INVALID_FORMAT))
        );
        assert!(diagnostic("unrelated NULL format").is_none());
    }

    #[test]
    fn chunk_nulls_and_single_evaluation() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        let wrong:i64=db.query_row("SELECT count(*) FROM range(6000) r(i) WHERE __msduck_string_escape(CASE WHEN i%17=0 THEN NULL ELSE '/雪' END,'json ') IS DISTINCT FROM CASE WHEN i%17=0 THEN NULL ELSE '\\/雪' END",[],|r|r.get(0)).unwrap();
        assert_eq!(wrong, 0);
        for sql in [
            "SELECT __msduck_string_escape(NULL,'xml')",
            "SELECT __msduck_string_escape('x',NULL)",
        ] {
            let error = db.execute_batch(sql).unwrap_err().to_string();
            assert!(
                error.contains(if sql.contains("'xml'") {
                    json_escape::INVALID_FORMAT
                } else {
                    TYPED_NULL_FORMAT
                }),
                "{error}"
            );
        }
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
