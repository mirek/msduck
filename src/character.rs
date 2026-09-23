//! Single-byte CHAR generation for the current Windows-1252 code page.
use duckdb::{
    core::{DataChunkHandle, Inserter, LogicalTypeId},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use sqlparser::ast::*;

pub fn lower(expr: &mut Expr) -> Result<(), String> {
    if let Expr::Function(function) = expr
        && let Some(value) = crate::function_args::unary(function, "CHAR")?
    {
        *expr = crate::engine::unary_function(
            "__msduck_char",
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

pub struct Character;
impl VScalar for Character {
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
            // The exact INTEGER signature fixes physical storage; only live,
            // non-null slots within this chunk are read.
            let count = unsafe { source.as_slice_with_len::<i32>(len)[index] };
            if !(0..=255).contains(&count) {
                result.set_null(index);
            } else {
                let character = msduck_core::encoding::decode_cp1252(&[count as u8]);
                result.insert(index, character.as_str());
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
    fn character_codes_and_nulls_across_chunks() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        let mut statement = db.prepare("SELECT CAST(CASE WHEN n%17=0 THEN NULL ELSE n%300-20 END AS INTEGER) code, __msduck_char(CAST(CASE WHEN n%17=0 THEN NULL ELSE n%300-20 END AS INTEGER)) FROM range(6000) r(n)").unwrap();
        let rows = statement
            .query_map([], |r| {
                Ok((r.get::<_, Option<i32>>(0)?, r.get::<_, Option<String>>(1)?))
            })
            .unwrap();
        for row in rows {
            let (code, value) = row.unwrap();
            let expected = code
                .filter(|n| (0..=255).contains(n))
                .map(|n| msduck_core::encoding::decode_cp1252(&[n as u8]));
            assert_eq!(value, expected);
        }
    }
}
