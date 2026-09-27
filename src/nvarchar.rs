//! Bound Unicode text by UTF-16 storage units.
use duckdb::{
    core::{DataChunkHandle, Inserter, LogicalTypeId as Id},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};

use msduck_core::character::{CastInput, CharacterType, Error, Family, Length};
use std::borrow::Cow;

// DuckDB's DECIMAL(38,38) text can start at the decimal point, while SQL
// Server's Unicode conversions include the zero as a counted UTF-16 unit.
fn decimal_display(text: &str) -> Cow<'_, str> {
    if text.starts_with('.') {
        Cow::Owned(format!("0{text}"))
    } else if let Some(fraction) = text.strip_prefix("-.") {
        Cow::Owned(format!("-0.{fraction}"))
    } else {
        Cow::Borrowed(text)
    }
}

struct Width<const CAST: bool, const TRY: bool>;
impl<const CAST: bool, const TRY: bool> VScalar for Width<CAST, TRY> {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let source = input.flat_vector(0);
        let widths = input.flat_vector(1);
        let strict = CAST.then(|| input.flat_vector(2));
        let mut result = output.flat_vector();
        for row in 0..len {
            if source.row_is_null(row as u64) || widths.row_is_null(row as u64) {
                result.set_null(row);
                continue;
            }
            let width = unsafe { widths.as_slice_with_len::<i32>(len)[row] };
            let length = Length::Bounded(width.try_into().map_err(|_| "invalid NVARCHAR width")?);
            let target = CharacterType::new(Family::Nvarchar, length)
                .map_err(|_| "invalid NVARCHAR width")?;
            let mut value =
                unsafe { source.as_slice_with_len::<duckdb::ffi::duckdb_string_t>(len)[row] };
            let bytes = unsafe {
                std::slice::from_raw_parts(
                    duckdb::ffi::duckdb_string_t_data(&mut value).cast::<u8>(),
                    duckdb::ffi::duckdb_string_t_length(value) as usize,
                )
            };
            let text = std::str::from_utf8(bytes)?;
            let mode = strict.as_ref().map_or(0, |strict| {
                if strict.row_is_null(row as u64) {
                    0
                } else {
                    unsafe { strict.as_slice_with_len::<i32>(len)[row] }
                }
            });
            let formatted = if mode == 2 {
                decimal_display(text)
            } else {
                Cow::Borrowed(text)
            };
            match target.cast(
                &formatted,
                if mode != 0 {
                    CastInput::OtherNumeric
                } else {
                    CastInput::Text
                },
            ) {
                Ok(value) => result.insert(row, value.as_ref()),
                Err(Error::NumericOverflow(_)) if TRY => result.set_null(row),
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            if CAST {
                vec![Id::Varchar.into(), Id::Integer.into(), Id::Integer.into()]
            } else {
                vec![Id::Varchar.into(), Id::Integer.into()]
            },
            Id::Varchar.into(),
        )]
    }
}
pub fn register(db: &duckdb::Connection) -> duckdb::Result<()> {
    db.register_scalar_function::<Width<false, false>>("__msduck_nvarchar_width")?;
    db.register_scalar_function::<Width<true, false>>("__msduck_nvarchar_limit")?;
    db.register_scalar_function::<Width<true, true>>("__msduck_nvarchar_try_limit")?;
    for (name, function, cast) in [
        ("cast", "__msduck_nvarchar_limit", "CAST"),
        ("try", "__msduck_nvarchar_try_limit", "TRY_CAST"),
    ] {
        db.execute_batch(&format!("CREATE OR REPLACE MACRO main.__msduck_{name}_nvarchar(value,width) AS {function}({cast}(value AS VARCHAR),width,CASE WHEN starts_with(typeof(value),'DECIMAL(') THEN 2 WHEN typeof(value) IN ('TINYINT','UTINYINT','SMALLINT','INTEGER','BIGINT','FLOAT','DOUBLE') THEN 1 ELSE 0 END)"))?;
    }
    Ok(())
}

pub use msduck_sql::expression_metadata::character::nvarchar_cast_width as width;

pub fn lower(expr: &mut sqlparser::ast::Expr) -> Result<(), String> {
    use sqlparser::ast::*;
    let (value, kind, trying) = match expr {
        Expr::Cast {
            expr: value,
            data_type: kind @ DataType::Nvarchar(_),
            kind: cast,
            format: None,
        } => (
            value,
            kind,
            matches!(cast, CastKind::TryCast | CastKind::SafeCast),
        ),
        Expr::Convert {
            expr: value,
            data_type: Some(kind @ DataType::Nvarchar(_)),
            is_try,
            styles,
            charset: None,
            ..
        } if styles.is_empty() => (value, kind, *is_try),
        _ => return Ok(()),
    };
    let Some(width) = width(kind)? else {
        return Ok(());
    };
    *expr = crate::engine::binary_function(
        if trying {
            "__msduck_try_nvarchar"
        } else {
            "__msduck_cast_nvarchar"
        },
        *value.clone(),
        Expr::Value(Value::Number(width.to_string(), false).into()),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn captured_decimal_unicode_values_widths_and_single_evaluation() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        let positive = format!("0.{}1", "0".repeat(37));
        let negative = format!("-{positive}");
        let zero = format!("0.{}", "0".repeat(38));
        let decimal = |value: &str| format!("CAST('{value}' AS DECIMAL(38,38))");
        let read = |sql: &str| {
            db.query_row(sql, [], |row| row.get::<_, Option<String>>(0))
                .unwrap()
        };
        assert_eq!(
            read("SELECT __msduck_cast_nvarchar(CAST('0' AS DECIMAL(38,38)),40)"),
            Some(zero)
        );
        assert_eq!(
            read(&format!(
                "SELECT __msduck_cast_nvarchar({},40)",
                decimal(&positive)
            )),
            Some(positive.clone())
        );
        assert_eq!(
            read(&format!(
                "SELECT __msduck_cast_nvarchar({},41)",
                decimal(&negative)
            )),
            Some(negative)
        );
        assert_eq!(
            read(&format!(
                "SELECT __msduck_nchar_width(__msduck_cast_nvarchar({},41),41)",
                decimal(&positive)
            )),
            Some(format!("{positive} "))
        );
        assert_eq!(
            read(&format!(
                "SELECT __msduck_try_nvarchar({},39)",
                decimal(&positive)
            )),
            None
        );
        assert!(
            db.query_row(
                &format!("SELECT __msduck_cast_nvarchar({},39)", decimal(&positive)),
                [],
                |row| row.get::<_, String>(0)
            )
            .is_err()
        );
        assert_eq!(
            read("SELECT __msduck_cast_nvarchar(CAST(NULL AS DECIMAL(38,38)),40)"),
            None
        );
        assert_eq!(
            read("SELECT __msduck_cast_nvarchar(CAST(0.5 AS DOUBLE),40)"),
            Some("0.5".into())
        );
        assert_eq!(
            read("SELECT __msduck_cast_nvarchar('.25',40)"),
            Some(".25".into())
        );
        db.execute_batch("CREATE SEQUENCE decimal_unicode_calls")
            .unwrap();
        assert_eq!(
            read(
                "SELECT __msduck_cast_nvarchar(CAST(nextval('decimal_unicode_calls') AS DECIMAL(38,2)),40)"
            ),
            Some("1.00".into())
        );
        assert_eq!(
            db.query_row("SELECT currval('decimal_unicode_calls')", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn captured_decimal_avg_uses_unicode_session_lowering() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        let sql = "SELECT CONVERT(NVARCHAR(40),AVG(d)) AS exact_value INTO dbo.decimal_unicode FROM (VALUES (CAST('0.00000000000000000000000000000000000001' AS DECIMAL(38,38))),(CAST('0' AS DECIMAL(38,38))),(CAST('0' AS DECIMAL(38,38)))) t(d)";
        assert!(
            session
                .batch_response(sql, &Default::default(), false, None)
                .1
        );
        let value: String = session
            .db
            .query_row("SELECT exact_value FROM dbo.decimal_unicode", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(value, format!("0.{}", "0".repeat(38)));
    }

    #[test]
    fn casts_check_widths_and_evaluate_once_across_chunks() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        super::register(&db).unwrap();
        let bad: i64 = db.query_row("SELECT count(*) FROM range(6000) r(i) WHERE __msduck_try_nvarchar(CASE WHEN i%3=0 THEN NULL WHEN i%3=1 THEN 42 ELSE -42 END,2) IS DISTINCT FROM CASE WHEN i%3=1 THEN '42' ELSE NULL END", [], |r| r.get(0)).unwrap();
        assert_eq!(bad, 0);
        db.execute_batch("CREATE SEQUENCE nvarchar_calls START 1")
            .unwrap();
        let count: i64 = db.query_row("SELECT count(__msduck_cast_nvarchar(nextval('nvarchar_calls'),4)) FROM range(6000)", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 6000);
        let calls: i64 = db
            .query_row("SELECT currval('nvarchar_calls')", [], |r| r.get(0))
            .unwrap();
        assert_eq!(calls, 6000);
    }

    #[test]
    fn widths_count_utf16_units_across_chunks() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        super::register(&db).unwrap();
        let bad:i64=db.query_row("SELECT count(*) FROM range(6000) r(i) WHERE __msduck_nvarchar_width(CASE WHEN i%3=0 THEN NULL WHEN i%3=1 THEN 'A🦆BC' ELSE 'abcdef' END,3) IS DISTINCT FROM CASE WHEN i%3=0 THEN NULL WHEN i%3=1 THEN 'A🦆' ELSE 'abc' END",[],|r|r.get(0)).unwrap();
        assert_eq!(bad, 0);
        assert!(
            db.query_row("SELECT __msduck_nvarchar_width('A🦆',2)", [], |r| r
                .get::<_, String>(0))
                .is_err()
        );
    }
}
