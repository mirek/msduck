//! NCHAR generation for the current non-SC collation.
use duckdb::{
    core::{DataChunkHandle, Inserter, LogicalTypeId},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use sqlparser::ast::*;

pub use msduck_sql::expression_metadata::character::nchar_cast_width as cast_width;

pub fn lower(expr: &mut Expr) -> Result<(), String> {
    let cast = match expr {
        Expr::Cast {
            expr: value,
            data_type,
            kind,
            format: None,
        } => Some((
            value,
            data_type,
            matches!(kind, CastKind::TryCast | CastKind::SafeCast),
        )),
        Expr::Convert {
            expr: value,
            data_type: Some(data_type),
            is_try,
            styles,
            charset: None,
            ..
        } if styles.is_empty() => Some((value, data_type, *is_try)),
        _ => None,
    };
    if let Some((value, kind, trying)) = cast
        && let Some(width) = cast_width(kind)?
    {
        let width = Expr::Value(Value::Number(width.to_string(), false).into());
        let converted = crate::engine::binary_function(
            if trying {
                "__msduck_try_nvarchar"
            } else {
                "__msduck_cast_nvarchar"
            },
            *value.clone(),
            width.clone(),
        );
        *expr = crate::engine::binary_function("__msduck_nchar_width", converted, width);
        return Ok(());
    }

    if let Expr::Function(function) = expr
        && let Some(value) = crate::function_args::unary(function, "NCHAR")?
    {
        *expr = crate::engine::unary_function(
            "__msduck_nchar",
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

pub struct NCharacter;
impl VScalar for NCharacter {
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
            if !(0..=65535).contains(&count) {
                result.set_null(index);
            } else {
                let character = char::from_u32(count as u32)
                    .ok_or("NCHAR isolated surrogate values are not yet supported")?;
                let mut bytes = [0u8; 4];
                result.insert(index, &*character.encode_utf8(&mut bytes));
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
    fn fixed_width_padding_counts_utf16_across_chunks() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        let wrong: i64 = db.query_row("SELECT count(*) FROM range(6000) r(n) WHERE __msduck_nchar_width(__msduck_cast_nvarchar(CASE n%4 WHEN 0 THEN NULL WHEN 1 THEN '' WHEN 2 THEN '🦆' ELSE 'abcdef' END,3),3) IS DISTINCT FROM CASE n%4 WHEN 0 THEN NULL WHEN 1 THEN '   ' WHEN 2 THEN '🦆 ' ELSE 'abc' END", [], |r|r.get(0)).unwrap();
        assert_eq!(wrong, 0);
    }
    #[test]
    fn bmp_roundtrip_and_nulls_across_chunks() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        let wrong: i64 = db.query_row("SELECT count(*) FROM range(65536) r(n) WHERE (n<55296 OR n>57343) AND __msduck_unicode(__msduck_nchar(CAST(CASE WHEN n BETWEEN 55296 AND 57343 THEN NULL ELSE n END AS INTEGER))) IS DISTINCT FROM n", [], |r| r.get(0)).unwrap();
        assert_eq!(wrong, 0);
        let count: i64 = db.query_row("SELECT count(__msduck_nchar(CAST(CASE WHEN n%3=0 THEN NULL WHEN n%3=1 THEN -1 ELSE 65536 END AS INTEGER))) FROM range(6000) r(n)", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 0);
    }
}
