//! Native DECIMAL division; SQL lowering supplies result precision explicitly.
use duckdb::{Connection, core::LogicalTypeId as Id, ffi::*};
use msduck_core::{
    decimal_arithmetic::{Error, divide},
    types::DecimalType,
    value::Decimal,
};
use std::{
    ffi::{CStr, CString, c_void},
    panic::{AssertUnwindSafe, catch_unwind},
};

const INTERNAL: &CStr = c"Invalid exact decimal division declaration.";
const OVERFLOW: &CStr = c"Arithmetic overflow error converting expression to data type numeric.";
const ZERO: &CStr = c"Divide by zero error encountered.";

unsafe extern "C" fn bind(info: duckdb_bind_info) {
    let result = catch_unwind(AssertUnwindSafe(|| unsafe {
        for index in 0..2 {
            let mut expr = duckdb_scalar_function_bind_get_argument(info, index);
            let mut kind = duckdb_expression_return_type(expr);
            let valid = duckdb_get_type_id(kind) == Id::Decimal as u32;
            duckdb_destroy_logical_type(&mut kind);
            duckdb_destroy_expression(&mut expr);
            if !valid {
                duckdb_scalar_function_bind_set_error(info, INTERNAL.as_ptr());
                return;
            }
        }
    }));
    if result.is_err() {
        unsafe {
            duckdb_scalar_function_bind_set_error(info, INTERNAL.as_ptr());
        }
    }
}

struct Input {
    data: *mut c_void,
    validity: *mut u64,
    precision: u8,
    scale: u8,
}
impl Input {
    unsafe fn new(vector: duckdb_vector) -> Result<Self, &'static CStr> {
        unsafe {
            let mut kind = duckdb_vector_get_column_type(vector);
            let valid = duckdb_get_type_id(kind) == Id::Decimal as u32;
            let precision = duckdb_decimal_width(kind);
            let scale = duckdb_decimal_scale(kind);
            duckdb_destroy_logical_type(&mut kind);
            if !valid || DecimalType::new(precision, scale).is_err() {
                return Err(INTERNAL);
            }
            Ok(Self {
                data: duckdb_vector_get_data(vector),
                validity: duckdb_vector_get_validity(vector),
                precision,
                scale,
            })
        }
    }
    unsafe fn valid(&self, row: idx_t) -> bool {
        self.validity.is_null() || unsafe { duckdb_validity_row_is_valid(self.validity, row) }
    }
    unsafe fn value(&self, row: usize) -> Result<Decimal, &'static CStr> {
        let coefficient = unsafe {
            match self.precision {
                1..=4 => i128::from(self.data.cast::<i16>().add(row).read_unaligned()),
                5..=9 => i128::from(self.data.cast::<i32>().add(row).read_unaligned()),
                10..=18 => i128::from(self.data.cast::<i64>().add(row).read_unaligned()),
                _ => {
                    let raw = self.data.cast::<duckdb_hugeint>().add(row).read_unaligned();
                    (i128::from(raw.upper) << 64) | i128::from(raw.lower)
                }
            }
        };
        Decimal::new(self.precision, self.scale, coefficient).map_err(|_| OVERFLOW)
    }
}
unsafe extern "C" fn invoke(
    info: duckdb_function_info,
    input: duckdb_data_chunk,
    output: duckdb_vector,
) {
    let result = catch_unwind(AssertUnwindSafe(|| unsafe {
        let scale = *duckdb_scalar_function_get_extra_info(info).cast::<u8>();
        let left = Input::new(duckdb_data_chunk_get_vector(input, 0))?;
        let right = Input::new(duckdb_data_chunk_get_vector(input, 1))?;
        let precision_vector = duckdb_data_chunk_get_vector(input, 2);
        let precisions = duckdb_vector_get_data(precision_vector).cast::<i32>();
        let precision_validity = duckdb_vector_get_validity(precision_vector);
        duckdb_vector_ensure_validity_writable(output);
        let validity = duckdb_vector_get_validity(output);
        let data = duckdb_vector_get_data(output).cast::<duckdb_hugeint>();
        for row in 0..duckdb_data_chunk_get_size(input) {
            let valid = left.valid(row)
                && right.valid(row)
                && (precision_validity.is_null()
                    || duckdb_validity_row_is_valid(precision_validity, row));
            duckdb_validity_set_row_validity(validity, row, valid);
            if !valid {
                continue;
            }
            let precision = u8::try_from(precisions.add(row as usize).read_unaligned())
                .map_err(|_| INTERNAL)?;
            let kind = DecimalType::new(precision, scale).map_err(|_| INTERNAL)?;
            let value = divide(left.value(row as usize)?, right.value(row as usize)?, kind)
                .map_err(|error| match error {
                    Error::DivideByZero => ZERO,
                    Error::Overflow => OVERFLOW,
                })?
                .coefficient();
            data.add(row as usize).write_unaligned(duckdb_hugeint {
                lower: value as u64,
                upper: (value >> 64) as i64,
            });
        }
        Ok::<_, &'static CStr>(())
    }));
    let error = match result {
        Ok(Ok(())) => return,
        Ok(Err(error)) => error,
        Err(_) => INTERNAL,
    };
    unsafe {
        duckdb_scalar_function_set_error(info, error.as_ptr());
    }
}
unsafe extern "C" fn destroy(pointer: *mut c_void) {
    unsafe {
        drop(Box::from_raw(pointer.cast::<u8>()));
    }
}
pub fn register(db: &Connection) -> duckdb::Result<()> {
    for scale in 0..=38 {
        let name = CString::new(format!("__msduck_decimal_divide_{scale}")).unwrap();
        unsafe {
            let mut function = duckdb_create_scalar_function();
            let mut any = duckdb_create_logical_type(Id::Any as u32);
            let mut integer = duckdb_create_logical_type(Id::Integer as u32);
            let mut decimal = duckdb_create_decimal_type(38, scale);
            if function.is_null() || any.is_null() || integer.is_null() || decimal.is_null() {
                duckdb_destroy_scalar_function(&mut function);
                duckdb_destroy_logical_type(&mut any);
                duckdb_destroy_logical_type(&mut integer);
                duckdb_destroy_logical_type(&mut decimal);
                return Err(duckdb::Error::DuckDBFailure(
                    duckdb::ffi::Error::new(DuckDBError),
                    Some("could not allocate decimal division definition".into()),
                ));
            }
            duckdb_scalar_function_set_name(function, name.as_ptr());
            duckdb_scalar_function_add_parameter(function, any);
            duckdb_scalar_function_add_parameter(function, any);
            duckdb_scalar_function_add_parameter(function, integer);
            duckdb_scalar_function_set_return_type(function, decimal);
            duckdb_scalar_function_set_special_handling(function);
            duckdb_scalar_function_set_extra_info(
                function,
                Box::into_raw(Box::new(scale)).cast(),
                Some(destroy),
            );
            duckdb_scalar_function_set_bind(function, Some(bind));
            duckdb_scalar_function_set_function(function, Some(invoke));
            let result = db.register_scalar_function_raw(function);
            duckdb_destroy_scalar_function(&mut function);
            duckdb_destroy_logical_type(&mut any);
            duckdb_destroy_logical_type(&mut integer);
            duckdb_destroy_logical_type(&mut decimal);
            result?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn session_lowering_keeps_exact_values_and_declared_storage() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        for (i, (expr, expected, kind)) in [
            (
                "CAST(2 AS DECIMAL(5,2))/CAST(3 AS DECIMAL(5,2))",
                Some("0.66666666"),
                "DECIMAL(13,8)",
            ),
            (
                "CAST(-2 AS DECIMAL(38,6))/CAST(3 AS DECIMAL(38,6))",
                Some("-0.666666"),
                "DECIMAL(38,6)",
            ),
            (
                "CAST(NULL AS DECIMAL(5,2))/CAST(0 AS DECIMAL(5,2))",
                None,
                "DECIMAL(13,8)",
            ),
            (
                "CAST(2 AS DECIMAL(5,2))/3",
                Some("0.6666666666666"),
                "DECIMAL(16,13)",
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let sql = format!("SELECT {expr} AS d INTO dbo.division_{i}");
            assert!(
                session
                    .batch_response(&sql, &Default::default(), false, None)
                    .1,
                "{sql}"
            );
            let row: (Option<String>, String) = session
                .db
                .query_row(
                    &format!("SELECT CAST(d AS VARCHAR),typeof(d) FROM dbo.division_{i}"),
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!(row, (expected.map(str::to_owned), kind.into()), "{expr}");
        }
        assert!(
            !session
                .batch_response(
                    "SELECT CAST(1 AS DECIMAL(5,2))/CAST(0 AS DECIMAL(5,2))",
                    &Default::default(),
                    false,
                    None
                )
                .1
        );
        assert!(
            session
                .batch_response("SELECT 1", &Default::default(), false, None)
                .1
        );
        for sql in [
            "SELECT CAST('99999999999999999999999999999999999999' AS DECIMAL(38,0))/CAST(1 AS DECIMAL(1,0))",
            "SELECT AVG(CAST('99999999999999999999999999999999999999' AS DECIMAL(38,0)))",
        ] {
            let (bytes, success) = session.batch_response(sql, &Default::default(), false, None);
            assert!(!success);
            let mut expected = Vec::new();
            crate::tds::sql_error(
                &mut expected,
                &msduck_core::diagnostic::SqlError::new(8115, 2, OVERFLOW.to_str().unwrap()),
            );
            assert!(
                bytes.windows(expected.len()).any(|part| part == expected),
                "{sql}"
            );
        }
    }
    #[test]
    fn native_widths_signs_nulls_and_single_evaluation() {
        let db = Connection::open_in_memory().unwrap();
        register(&db).unwrap();
        for p in [4, 9, 18, 38] {
            let sql = format!(
                "SELECT count(*) FROM (SELECT CAST(CASE WHEN i%17=0 THEN NULL ELSE i%3-1 END AS DECIMAL({p},2)) a,CAST(3 AS DECIMAL({p},2)) b FROM range(6000) t(i)) WHERE __msduck_decimal_divide_6(a,b,38) IS DISTINCT FROM CASE WHEN a IS NULL THEN NULL WHEN a<0 THEN CAST('-0.333333' AS DECIMAL(38,6)) WHEN a>0 THEN CAST('0.333333' AS DECIMAL(38,6)) ELSE CAST(0 AS DECIMAL(38,6)) END"
            );
            assert_eq!(
                db.query_row(&sql, [], |r| r.get::<_, i64>(0)).unwrap(),
                0,
                "precision {p}"
            );
        }
        let null: Option<String> = db.query_row("SELECT CAST(__msduck_decimal_divide_8(CAST(NULL AS DECIMAL(5,2)),CAST(0 AS DECIMAL(5,2)),13) AS VARCHAR)",[],|r|r.get(0)).unwrap();
        assert_eq!(null, None);
        db.execute_batch("CREATE SEQUENCE left_calls; CREATE SEQUENCE right_calls")
            .unwrap();
        let count: i64 = db.query_row("SELECT count(*) FROM (SELECT __msduck_decimal_divide_6(CAST(nextval('left_calls') AS DECIMAL(18,0)),CAST(nextval('right_calls') AS DECIMAL(18,0)),38) a FROM range(6000)) WHERE a<>1",[],|r|r.get(0)).unwrap();
        assert_eq!(count, 0);
        for name in ["left_calls", "right_calls"] {
            assert_eq!(
                db.query_row(&format!("SELECT currval('{name}')"), [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                6000
            );
        }
    }
    #[test]
    fn invalid_bindings_zero_divisors_and_precision_overflow() {
        let db = Connection::open_in_memory().unwrap();
        register(&db).unwrap();
        for (sql, message) in [
            (
                "SELECT __msduck_decimal_divide_6(CAST(1 AS DECIMAL(5,2)),CAST(0 AS DECIMAL(5,2)),38)",
                ZERO,
            ),
            (
                "SELECT __msduck_decimal_divide_6(CAST('99999999999999999999999999999999999999' AS DECIMAL(38,0)),CAST(1 AS DECIMAL(1,0)),38)",
                OVERFLOW,
            ),
            (
                "SELECT __msduck_decimal_divide_6(CAST(10 AS DECIMAL(5,2)),CAST(1 AS DECIMAL(5,2)),7)",
                OVERFLOW,
            ),
        ] {
            assert!(
                db.query_row(sql, [], |r| r.get::<_, String>(0))
                    .unwrap_err()
                    .to_string()
                    .contains(message.to_str().unwrap())
            );
        }
        assert!(
            db.prepare(
                "SELECT __msduck_decimal_divide_6('x',CAST(1 AS DECIMAL(5,2)),38) WHERE false"
            )
            .is_err()
        );
    }
}
