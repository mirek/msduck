//! Explicit VARCHAR conversions for representable Windows-1252 text.
use duckdb::{
    core::{DataChunkHandle, Inserter, LogicalTypeId as Id},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use msduck_core::character::{CastInput, CharacterType, Error, Family, Length};
use sqlparser::ast::*;
struct Limit<const TRY: bool, const FIXED: bool>;
impl<const TRY: bool, const FIXED: bool> VScalar for Limit<TRY, FIXED> {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let source = input.flat_vector(0);
        let widths = input.flat_vector(1);
        let modes = input.flat_vector(2);
        let mut out = output.flat_vector();
        for row in 0..len {
            if source.row_is_null(row as u64)
                || widths.row_is_null(row as u64)
                || modes.row_is_null(row as u64)
            {
                out.set_null(row);
                continue;
            }
            let width = unsafe { widths.as_slice_with_len::<i32>(len)[row] };
            let length = if width == -1 {
                Length::Max
            } else {
                Length::Bounded(width.try_into().map_err(|_| "invalid VARCHAR width")?)
            };
            let target =
                CharacterType::new(if FIXED { Family::Char } else { Family::Varchar }, length)
                    .map_err(|_| "invalid VARCHAR width")?;
            let mut raw =
                unsafe { source.as_slice_with_len::<duckdb::ffi::duckdb_string_t>(len)[row] };
            let bytes = unsafe {
                std::slice::from_raw_parts(
                    duckdb::ffi::duckdb_string_t_data(&mut raw).cast::<u8>(),
                    duckdb::ffi::duckdb_string_t_length(raw) as usize,
                )
            };
            let text = std::str::from_utf8(bytes)?;
            let mode = unsafe { modes.as_slice_with_len::<i32>(len)[row] };
            let source = match mode {
                1 => CastInput::SmallInteger,
                2 => CastInput::OtherNumeric,
                _ => CastInput::Text,
            };
            match target.cast(text, source) {
                Ok(value) => out.insert(row, value.as_ref()),
                Err(Error::NumericOverflow(_)) if TRY => out.set_null(row),
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![Id::Varchar.into(), Id::Integer.into(), Id::Integer.into()],
            Id::Varchar.into(),
        )]
    }
}
pub fn register(db: &duckdb::Connection) -> duckdb::Result<()> {
    db.register_scalar_function::<Limit<false, false>>("__msduck_varchar_limit")?;
    db.register_scalar_function::<Limit<true, false>>("__msduck_varchar_try_limit")?;
    db.register_scalar_function::<Limit<false, true>>("__msduck_char_limit")?;
    db.register_scalar_function::<Limit<true, true>>("__msduck_char_try_limit")?;
    for (name, function, cast) in [
        ("cast", "__msduck_varchar_limit", "CAST"),
        ("try", "__msduck_varchar_try_limit", "TRY_CAST"),
    ] {
        db.execute_batch(&format!("CREATE OR REPLACE MACRO main.__msduck_{name}_varchar(value,width) AS {function}({cast}(value AS VARCHAR),width,CASE WHEN typeof(value) IN ('TINYINT','UTINYINT','SMALLINT','INTEGER') THEN 1 WHEN typeof(value) IN ('BIGINT','FLOAT','DOUBLE') OR starts_with(typeof(value),'DECIMAL(') THEN 2 ELSE 0 END)"))?;
    }
    for (name, function, cast) in [
        ("cast", "__msduck_char_limit", "CAST"),
        ("try", "__msduck_char_try_limit", "TRY_CAST"),
    ] {
        db.execute_batch(&format!("CREATE OR REPLACE MACRO main.__msduck_{name}_char(value,width) AS {function}({cast}(value AS VARCHAR),width,CASE WHEN typeof(value) IN ('TINYINT','UTINYINT','SMALLINT','INTEGER') THEN 1 WHEN typeof(value) IN ('BIGINT','FLOAT','DOUBLE') OR starts_with(typeof(value),'DECIMAL(') THEN 2 ELSE 0 END)"))?;
    }
    Ok(())
}
pub use msduck_sql::expression_metadata::character::varchar_cast_width as width;

pub fn lower(expr: &mut Expr) -> Result<(), String> {
    let (value, kind, trying) = match expr {
        Expr::Cast {
            expr: value,
            data_type:
                kind @ (DataType::Varchar(Some(_))
                | DataType::Char(Some(_))
                | DataType::Character(Some(_))),
            kind: cast,
            format: None,
        } => (
            value,
            kind,
            matches!(cast, CastKind::TryCast | CastKind::SafeCast),
        ),
        Expr::Convert {
            expr: value,
            data_type:
                Some(kind @ (DataType::Varchar(_) | DataType::Char(_) | DataType::Character(_))),
            is_try,
            styles,
            charset: None,
            ..
        } if styles.is_empty() => (value, kind, *is_try),
        _ => return Ok(()),
    };
    let width = width(kind)?;
    *expr = crate::engine::binary_function(
        match (
            trying,
            matches!(kind, DataType::Char(_) | DataType::Character(_)),
        ) {
            (false, false) => "__msduck_cast_varchar",
            (true, false) => "__msduck_try_varchar",
            (false, true) => "__msduck_cast_char",
            (true, true) => "__msduck_try_char",
        },
        *value.clone(),
        Expr::Value(
            Value::Number(
                if width == u16::MAX {
                    "-1".into()
                } else {
                    width.to_string()
                },
                false,
            )
            .into(),
        ),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn limits_preserve_cp1252_nulls_and_once_per_row_evaluation() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        super::register(&db).unwrap();
        // Preserve DuckDB's constant-NULL result for row-varying NULL policies.
        assert_eq!(
            db.query_row("SELECT __msduck_varchar_limit('abc',2,NULL)", [], |r| {
                r.get::<_, Option<String>>(0)
            })
            .unwrap(),
            None
        );
        assert_eq!(db.query_row("SELECT count(*) FROM range(6000) r(i) WHERE __msduck_varchar_limit('abc',2,CAST(CASE WHEN i%2=0 THEN NULL ELSE 0 END AS INT)) IS DISTINCT FROM CASE WHEN i%2=0 THEN NULL ELSE 'ab' END",[],|r|r.get::<_,i64>(0)).unwrap(),0);
        assert_eq!(db.query_row("SELECT count(*) FROM range(6000) r(i) WHERE __msduck_cast_varchar(CASE WHEN i%3=0 THEN NULL WHEN i%3=1 THEN '€éabc' ELSE 'abcdef' END,2) IS DISTINCT FROM CASE WHEN i%3=0 THEN NULL WHEN i%3=1 THEN '€é' ELSE 'ab' END",[],|r|r.get::<_,i64>(0)).unwrap(),0);
        db.execute_batch("CREATE SEQUENCE varchar_calls").unwrap();
        assert_eq!(
            db.query_row(
                "SELECT count(__msduck_cast_varchar(nextval('varchar_calls'),4)) FROM range(6000)",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            6000
        );
        assert_eq!(
            db.query_row("SELECT currval('varchar_calls')", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            6000
        );
        assert_eq!(
            db.query_row("SELECT __msduck_try_varchar(12.3,2)", [], |r| r
                .get::<_, Option<String>>(0))
                .unwrap(),
            None
        );
    }
    #[test]
    fn varchar_max_uses_plp_for_values_empty_and_null() {
        use crate::tds::Type;
        use duckdb::types::Value;
        let mut out = vec![];
        crate::engine::encode_value(
            &mut out,
            &Type::Varchar(u16::MAX),
            &Value::Text("€é".into()),
        )
        .unwrap();
        assert_eq!(
            out,
            [
                vec![2, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 0x80, 0xe9],
                vec![0; 4]
            ]
            .concat()
        );
        out.clear();
        crate::engine::encode_value(
            &mut out,
            &Type::Varchar(u16::MAX),
            &Value::Text(String::new()),
        )
        .unwrap();
        assert_eq!(out, vec![0; 12]);
        out.clear();
        crate::engine::encode_value(&mut out, &Type::Varchar(u16::MAX), &Value::Null).unwrap();
        assert_eq!(out, vec![255; 8]);
    }
}

#[cfg(test)]
mod char_tests {
    #[test]
    fn fixed_casts_pad_after_conversion_and_evaluate_once() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        super::register(&db).unwrap();
        assert_eq!(db.query_row("SELECT count(*) FROM range(6000) r(i) WHERE __msduck_cast_char(CASE WHEN i%3=0 THEN NULL WHEN i%3=1 THEN '€é' ELSE 'abcdef' END,4) IS DISTINCT FROM CASE WHEN i%3=0 THEN NULL WHEN i%3=1 THEN '€é  ' ELSE 'abcd' END",[],|r|r.get::<_,i64>(0)).unwrap(),0);
        db.execute_batch("CREATE SEQUENCE char_calls").unwrap();
        assert_eq!(db.query_row("SELECT count(*) FROM range(6000) WHERE length(__msduck_cast_char(nextval('char_calls'),5))=5",[],|r|r.get::<_,i64>(0)).unwrap(),6000);
        assert_eq!(
            db.query_row("SELECT currval('char_calls')", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            6000
        );
        let mut bytes = vec![];
        crate::engine::encode_value(
            &mut bytes,
            &crate::tds::Type::Char(4),
            &duckdb::types::Value::Text("€é  ".into()),
        )
        .unwrap();
        assert_eq!(bytes, vec![4, 0, 0x80, 0xe9, 32, 32]);
    }
}
