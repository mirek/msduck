//! Bounded SPACE generation with SQL Server integer argument conversion.
use duckdb::{
    core::{DataChunkHandle, Inserter, LogicalTypeId},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use sqlparser::ast::*;

pub fn lower(expr: &mut Expr) -> Result<(), String> {
    if let Expr::Function(function) = expr
        && let Some(value) = crate::function_args::unary(function, "SPACE")?
    {
        *expr = crate::engine::unary_function(
            "__msduck_space",
            Expr::Cast {
                kind: CastKind::Cast,
                expr: Box::new(value.clone()),
                data_type: DataType::Int(None),
                format: None,
            },
        );
    }
    Ok(())
}

pub struct Space;
impl VScalar for Space {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let source = input.flat_vector(0);
        let mut result = output.flat_vector();
        let spaces = " ".repeat(8000);
        for index in 0..len {
            if source.row_is_null(index as u64) {
                result.set_null(index);
                continue;
            }
            // The exact INTEGER signature fixes physical storage; only live,
            // non-null slots within this chunk are read.
            let count = unsafe { source.as_slice_with_len::<i32>(len)[index] };
            if count < 0 {
                result.set_null(index);
            } else {
                result.insert(index, &spaces[..count.min(8000) as usize]);
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![LogicalTypeId::Integer.into()],
            LogicalTypeId::Varchar.into(),
        )]
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn handles_null_negative_inline_heap_and_capped_results_across_chunks() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        let wrong: i64 = db.query_row("SELECT COUNT(*) FROM (SELECT CAST(CASE WHEN n%17=0 THEN NULL ELSE n-10 END AS INTEGER) n FROM range(10000) r(n)) s WHERE length(__msduck_space(n)) IS DISTINCT FROM CASE WHEN n IS NULL OR n<0 THEN NULL ELSE least(n,8000) END", [], |r| r.get(0)).unwrap();
        assert_eq!(wrong, 0);
    }
}
