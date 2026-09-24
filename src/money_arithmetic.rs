//! Native vector adapter for the deterministic currency arithmetic rules.
use duckdb::{
    Connection,
    core::{DataChunkHandle, LogicalTypeHandle},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use msduck_core::money::{MoneyType, Operation, calculate};

struct Binary<const SMALL: bool, const OP: u8>;
impl<const SMALL: bool, const OP: u8> VScalar for Binary<SMALL, OP> {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let left = input.flat_vector(0);
        let right = input.flat_vector(1);
        let mut out = output.flat_vector();
        let kind = if SMALL {
            MoneyType::SmallMoney
        } else {
            MoneyType::Money
        };
        let operation = match OP {
            0 => Operation::Add,
            1 => Operation::Subtract,
            2 => Operation::Multiply,
            3 => Operation::Divide,
            _ => Operation::Modulo,
        };
        for index in 0..len {
            if left.row_is_null(index as u64) || right.row_is_null(index as u64) {
                out.set_null(index);
                continue;
            }
            let read =
                |v: &duckdb::core::FlatVector<'_>| -> Result<i64, Box<dyn std::error::Error>> {
                    let coefficient = if SMALL {
                        i128::from(unsafe { v.as_slice_with_len::<i64>(len)[index] })
                    } else {
                        let raw = unsafe {
                            v.as_slice_with_len::<duckdb::ffi::duckdb_hugeint>(len)[index]
                        };
                        (i128::from(raw.upper) << 64) | i128::from(raw.lower)
                    };
                    kind.check_scaled(coefficient)?;
                    Ok(coefficient as i64)
                };
            let value = calculate(kind, operation, read(&left)?, read(&right)?)?;
            // Signatures fix physical widths; validity and bounds were checked.
            unsafe {
                if SMALL {
                    out.as_mut_slice_with_len::<i64>(len)[index] = value;
                } else {
                    out.as_mut_slice_with_len::<duckdb::ffi::duckdb_hugeint>(len)[index] =
                        duckdb::ffi::duckdb_hugeint {
                            lower: value as u64,
                            upper: if value < 0 { -1 } else { 0 },
                        };
                }
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        let kind = || LogicalTypeHandle::decimal(if SMALL { 10 } else { 19 }, 4);
        vec![ScalarFunctionSignature::exact(vec![kind(), kind()], kind())]
    }
}
pub fn register(db: &Connection) -> duckdb::Result<()> {
    macro_rules! one {
        ($small:literal,$family:literal,$op:literal,$name:literal) => {
            db.register_scalar_function::<Binary<$small, $op>>(concat!(
                "__msduck_",
                $family,
                "_",
                $name
            ))?
        };
    }
    one!(false, "money", 0, "add");
    one!(false, "money", 1, "subtract");
    one!(false, "money", 2, "multiply");
    one!(false, "money", 3, "divide");
    one!(false, "money", 4, "modulo");
    one!(true, "smallmoney", 0, "add");
    one!(true, "smallmoney", 1, "subtract");
    one!(true, "smallmoney", 2, "multiply");
    one!(true, "smallmoney", 3, "divide");
    one!(true, "smallmoney", 4, "modulo");
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn chunked_currency_operands_are_evaluated_once_with_nulls() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        session
            .db
            .execute_batch("CREATE SEQUENCE currency_left; CREATE SEQUENCE currency_right")
            .unwrap();
        let sql = "SELECT i,CAST(CASE WHEN nextval('currency_left')%17=0 THEN NULL ELSE 1 END AS MONEY)+CAST(nextval('currency_right') AS MONEY) AS n INTO currency_chunks FROM range(6000) r(i)";
        assert!(
            session
                .batch_response(sql, &Default::default(), false, None)
                .1
        );
        let wrong:i64=session.db.query_row("SELECT count(*) FROM currency_chunks WHERE n IS DISTINCT FROM CASE WHEN (i+1)%17=0 THEN NULL ELSE i+2 END",[],|r|r.get(0)).unwrap();
        assert_eq!(wrong, 0);
        for sequence in ["currency_left", "currency_right"] {
            assert_eq!(
                session
                    .db
                    .query_row(&format!("SELECT currval('{sequence}')"), [], |r| r
                        .get::<_, i64>(0))
                    .unwrap(),
                6000
            );
        }
    }
}
