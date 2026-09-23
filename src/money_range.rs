//! Range checks after operands have been rounded to currency scale four.
use duckdb::{
    core::{DataChunkHandle, LogicalTypeHandle},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use msduck_core::money::MoneyType;

pub struct Check<const SMALL: bool, const TRY: bool = false>;
impl<const SMALL: bool, const TRY: bool> VScalar for Check<SMALL, TRY> {
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
            // The exact input signature is DECIMAL(38,4), physically HUGEINT.
            // Outputs use HUGEINT for MONEY and i64 for SMALLMONEY. The scalar
            // wrapper flattens both vectors, and all accesses are chunk-bounded.
            let value =
                unsafe { source.as_slice_with_len::<duckdb::ffi::duckdb_hugeint>(len)[index] };
            let scaled = (i128::from(value.upper) << 64) | i128::from(value.lower);
            let kind = if SMALL {
                MoneyType::SmallMoney
            } else {
                MoneyType::Money
            };
            if let Err(error) = kind.check_scaled(scaled) {
                if TRY {
                    result.set_null(index);
                    continue;
                }
                return Err(error.into());
            }
            if SMALL {
                // The core check proved that the coefficient fits even in i32.
                unsafe {
                    result.as_mut_slice_with_len::<i64>(len)[index] = scaled as i64;
                }
            } else {
                unsafe {
                    result.as_mut_slice_with_len::<duckdb::ffi::duckdb_hugeint>(len)[index] = value;
                }
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        let width = if SMALL { 10 } else { 19 };
        vec![ScalarFunctionSignature::exact(
            vec![LogicalTypeHandle::decimal(38, 4)],
            LogicalTypeHandle::decimal(width, 4),
        )]
    }
}

/// Parse text independently of DuckDB's decimal lexer and precision limit.
struct Text<const SMALL: bool, const TRY: bool>;
impl<const SMALL: bool, const TRY: bool> VScalar for Text<SMALL, TRY> {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let source = input.flat_vector(0);
        let mut result = output.flat_vector();
        for row in 0..len {
            if source.row_is_null(row as u64) {
                result.set_null(row);
                continue;
            }
            // The exact VARCHAR signature and flattened chunk bound this read.
            let mut raw =
                unsafe { source.as_slice_with_len::<duckdb::ffi::duckdb_string_t>(len)[row] };
            let bytes = unsafe {
                std::slice::from_raw_parts(
                    duckdb::ffi::duckdb_string_t_data(&mut raw).cast::<u8>(),
                    duckdb::ffi::duckdb_string_t_length(raw) as usize,
                )
            };
            let value =
                msduck_core::money::parse_text(std::str::from_utf8(bytes)?).and_then(|value| {
                    if SMALL {
                        MoneyType::SmallMoney.check_scaled(i128::from(value))?;
                    }
                    Ok(value)
                });
            match value {
                Ok(value) if SMALL => unsafe {
                    result.as_mut_slice_with_len::<i64>(len)[row] = value;
                },
                Ok(value) => unsafe {
                    result.as_mut_slice_with_len::<duckdb::ffi::duckdb_hugeint>(len)[row] =
                        duckdb::ffi::duckdb_hugeint {
                            lower: value as u64,
                            upper: if value < 0 { -1 } else { 0 },
                        };
                },
                Err(_) if TRY => result.set_null(row),
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![duckdb::core::LogicalTypeId::Varchar.into()],
            LogicalTypeHandle::decimal(if SMALL { 10 } else { 19 }, 4),
        )]
    }
}

pub fn register(db: &duckdb::Connection) -> duckdb::Result<()> {
    db.register_scalar_function::<Check<false>>("__msduck_money_range")?;
    db.register_scalar_function::<Check<true>>("__msduck_smallmoney_range")?;
    db.register_scalar_function::<Check<false, true>>("__msduck_try_money_range")?;
    db.register_scalar_function::<Check<true, true>>("__msduck_try_smallmoney_range")?;
    db.register_scalar_function::<Text<false, false>>("__msduck_money_text")?;
    db.register_scalar_function::<Text<true, false>>("__msduck_smallmoney_text")?;
    db.register_scalar_function::<Text<false, true>>("__msduck_try_money_text")?;
    db.register_scalar_function::<Text<true, true>>("__msduck_try_smallmoney_text")?;
    for name in ["money", "smallmoney", "try_money", "try_smallmoney"] {
        let cast = if name.starts_with("try_") {
            "TRY_CAST"
        } else {
            "CAST"
        };
        // typeof is a bind-time property; only the selected branch evaluates
        // the operand. Preserve numeric conversion and its error identity.
        db.execute_batch(&format!("CREATE OR REPLACE MACRO main.__msduck_{name}_convert(value) AS CASE WHEN typeof(value) = 'VARCHAR' THEN __msduck_{name}_text(CAST(value AS VARCHAR)) ELSE __msduck_{name}_range({cast}(value AS DECIMAL(38,4))) END"))?;
    }
    Ok(())
}

pub fn diagnostic(message: &str) -> Option<msduck_core::diagnostic::SqlError> {
    let text = message
        .strip_prefix("Invalid Input Error: ")
        .unwrap_or(message);
    let number = match text {
        msduck_core::money::TEXT_SYNTAX => 235,
        msduck_core::money::TEXT_OVERFLOW => 236,
        _ => return None,
    };
    Some(msduck_core::diagnostic::SqlError::new(number, 1, text))
}

#[cfg(test)]
mod tests {
    #[test]
    fn currency_conversion_evaluates_volatile_inputs_once_across_chunks() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        session
            .db
            .execute_batch("CREATE SEQUENCE money_calls START 214740")
            .unwrap();
        let (_, ok) = session.batch_response(
            "SELECT TRY_CAST(nextval('money_calls') AS SMALLMONEY) AS n INTO money_results FROM range(6000)",
            &Default::default(), false, None,
        );
        assert!(ok);
        assert_eq!(session.db.query_row(
            "SELECT count(*), count(n), CAST(min(n) AS BIGINT), CAST(max(n) AS BIGINT) FROM money_results",
            [], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?)),
        ).unwrap(), (6000, 9, 214740, 214748));
        assert_eq!(
            session
                .db
                .query_row("SELECT currval('money_calls')", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            220739
        );
        let (_, ok) = session.batch_response(
            "SELECT CAST(nextval('money_calls') AS MONEY) FROM range(6000)",
            &Default::default(),
            false,
            None,
        );
        assert!(ok);
        assert_eq!(
            session
                .db
                .query_row("SELECT currval('money_calls')", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            226739
        );
    }
    #[test]
    fn currency_text_producers_evaluate_once_across_chunks() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        session
            .db
            .execute_batch("CREATE SEQUENCE money_text_calls START 1")
            .unwrap();
        let (_, ok) = session.batch_response(
            "SELECT CAST('$' || CAST(nextval('money_text_calls') AS VARCHAR(20)) AS MONEY) AS n INTO money_text_results FROM range(6000)",
            &Default::default(), false, None,
        );
        assert!(ok);
        assert_eq!(
            session
                .db
                .query_row(
                    "SELECT CAST(sum(n) AS BIGINT) FROM money_text_results",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            18_003_000
        );
        assert_eq!(
            session
                .db
                .query_row("SELECT currval('money_text_calls')", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            6000
        );
        let (_, ok) = session.batch_response(
            "SELECT TRY_CAST(CASE WHEN nextval('money_text_calls')%2=0 THEN '$1.25' ELSE 'bad' END AS SMALLMONEY) AS n INTO money_text_try FROM range(6000)",
            &Default::default(), false, None,
        );
        assert!(ok);
        assert_eq!(
            session
                .db
                .query_row("SELECT count(n) FROM money_text_try", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            3000
        );
        assert_eq!(
            session
                .db
                .query_row("SELECT currval('money_text_calls')", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            12000
        );
    }
    #[test]
    fn conditional_currency_conversions_preserve_branch_evaluation_across_chunks() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        session
            .db
            .execute_batch("CREATE SEQUENCE money_condition; CREATE SEQUENCE money_branch")
            .unwrap();
        let (_, ok) = session.batch_response(
            "SELECT CASE WHEN nextval('money_condition')%2=0 THEN '$' || CAST(nextval('money_branch') AS VARCHAR(30)) ELSE CAST(NULL AS MONEY) END AS m INTO money_branch_results FROM range(6000)",
            &Default::default(), false, None,
        );
        assert!(ok);
        assert_eq!(
            session
                .db
                .query_row(
                    "SELECT count(m),CAST(sum(m) AS BIGINT) FROM money_branch_results",
                    [],
                    |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
                )
                .unwrap(),
            (3000, 4501500)
        );
        for (name, count) in [("money_condition", 6000), ("money_branch", 3000)] {
            assert_eq!(
                session
                    .db
                    .query_row(&format!("SELECT currval('{name}')"), [], |r| r
                        .get::<_, i64>(0))
                    .unwrap(),
                count
            );
        }
    }
    #[test]
    fn currency_comparison_evaluates_each_operand_once_across_chunks() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        session
            .db
            .execute_batch("CREATE SEQUENCE money_left; CREATE SEQUENCE money_right")
            .unwrap();
        let (_, ok) = session.batch_response(
            "SELECT COUNT(*) AS n INTO money_comparison_count FROM range(6000) WHERE CAST(nextval('money_left') AS MONEY) = '$' || CAST(nextval('money_right') AS VARCHAR(30))",
            &Default::default(), false, None,
        );
        assert!(ok);
        assert_eq!(
            session
                .db
                .query_row("SELECT n FROM money_comparison_count", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            6000
        );
        for name in ["money_left", "money_right"] {
            assert_eq!(
                session
                    .db
                    .query_row(&format!("SELECT currval('{name}')"), [], |r| r
                        .get::<_, i64>(0))
                    .unwrap(),
                6000
            );
        }
    }
}
