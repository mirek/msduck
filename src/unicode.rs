//! UNICODE under the current non-SC character semantics.
use duckdb::{
    core::{DataChunkHandle, LogicalTypeId},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use sqlparser::ast::*;

pub fn argument(function: &Function) -> Result<Option<&Expr>, String> {
    crate::function_args::unary(function, "UNICODE")
}

pub fn lower(expr: &mut Expr) -> Result<(), String> {
    if let Expr::Function(function) = expr
        && let Some(value) = argument(function)?
    {
        *expr = crate::engine::unary_function(
            "__msduck_carrier_unicode",
            crate::engine::unary_function("__msduck_carrier_input", value.clone()),
        );
    }
    Ok(())
}

pub struct Unicode;
impl VScalar for Unicode {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let source = input.flat_vector(0);
        let mut result = output.flat_vector();
        for index in 0..len {
            if source.row_is_null(index as u64) {
                result.set_null(index);
                continue;
            }
            // The exact VARCHAR signature fixes storage. Copy inline storage
            // and keep it alive while borrowing bytes; heap data belongs to the
            // input chunk. Only initialized non-null slots are accessed.
            let mut value =
                unsafe { source.as_slice_with_len::<duckdb::ffi::duckdb_string_t>(len)[index] };
            let bytes = unsafe {
                std::slice::from_raw_parts(
                    duckdb::ffi::duckdb_string_t_data(&mut value).cast::<u8>(),
                    duckdb::ffi::duckdb_string_t_length(value) as usize,
                )
            };
            match std::str::from_utf8(bytes)?.encode_utf16().next() {
                Some(unit) => unsafe {
                    result.as_mut_slice_with_len::<i32>(len)[index] = i32::from(unit);
                },
                None => result.set_null(index),
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![LogicalTypeId::Varchar.into()],
            LogicalTypeId::Integer.into(),
        )]
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn raw_unicode_results_emit_reference_flags_for_values_nulls_and_empty_sets() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        for (sql, value, empty) in [
            ("SELECT UNICODE(LEFT(N'🦆',1)) AS u", Some(55358i32), false),
            ("SELECT UNICODE(RIGHT(N'🦆',1)) AS u", Some(56710), false),
            ("SELECT UNICODE(N'') AS u", None, false),
            ("SELECT UNICODE(NULL) AS u", None, false),
            ("SELECT UNICODE(12) AS u", Some(49), false),
            ("SELECT UNICODE(N'x') AS u WHERE 1=0", None, true),
            (
                "WITH q AS (SELECT RIGHT(N'🦆',1) AS s) SELECT UNICODE(s) AS u FROM q",
                Some(56710),
                false,
            ),
        ] {
            let (wire, success) = session.batch_response(sql, &Default::default(), false, None);
            assert!(success, "{sql}: {wire:?}");
            // One nullable computed INTN column named u. Flags 33 are captured
            // independently in reference/unicode-first-unit.json, including NULL.
            let metadata = [0x81, 1, 0, 0, 0, 0, 0, 33, 0, 0x26, 4, 1, b'u', 0];
            assert!(wire.starts_with(&metadata), "{sql}: {wire:?}");
            let rows = &wire[metadata.len()..];
            if empty {
                assert_eq!(rows[0], 0xfd);
            } else {
                let mut expected = vec![0xd1];
                if let Some(value) = value {
                    expected.push(4);
                    expected.extend(value.to_le_bytes());
                } else {
                    expected.push(0);
                }
                assert!(rows.starts_with(&expected), "{sql}: {wire:?}");
            }
        }
    }

    #[test]
    fn reads_inline_heap_and_null_inputs_across_chunks() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        let wrong: i64 = db.query_row("SELECT count(*) FROM range(6000) t(n) WHERE __msduck_unicode(CASE n%4 WHEN 0 THEN NULL WHEN 1 THEN '' WHEN 2 THEN 'Å' ELSE '🦆' || repeat('x', CAST(n%100 AS INT)) END) IS DISTINCT FROM CASE n%4 WHEN 2 THEN 197 WHEN 3 THEN 55358 ELSE NULL END", [], |r| r.get(0)).unwrap();
        assert_eq!(wrong, 0);
    }
}
