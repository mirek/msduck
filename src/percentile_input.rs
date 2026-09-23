//! Binding-time numeric validation and single-evaluation DOUBLE conversion.
use duckdb::{Connection, core::LogicalTypeId as Id, ffi::*};
use std::panic::{AssertUnwindSafe, catch_unwind};

const INVALID: &std::ffi::CStr = c"PERCENTILE_CONT requires a numeric ORDER BY expression.";
const INTERNAL: &std::ffi::CStr = c"PERCENTILE_CONT numeric conversion callback failed.";

fn numeric(kind: u32) -> bool {
    [
        Id::Boolean,
        Id::Tinyint,
        Id::Smallint,
        Id::Integer,
        Id::Bigint,
        Id::UTinyint,
        Id::USmallint,
        Id::UInteger,
        Id::UBigint,
        Id::Hugeint,
        Id::Float,
        Id::Double,
        Id::Decimal,
        Id::SqlNull,
    ]
    .iter()
    .any(|id| kind == *id as u32)
}

unsafe extern "C" fn bind(info: duckdb_bind_info) {
    let result = catch_unwind(AssertUnwindSafe(|| unsafe {
        let mut argument = duckdb_scalar_function_bind_get_argument(info, 0);
        let mut kind = duckdb_expression_return_type(argument);
        let valid = numeric(duckdb_get_type_id(kind));
        duckdb_destroy_logical_type(&mut kind);
        duckdb_destroy_expression(&mut argument);
        if !valid {
            duckdb_scalar_function_bind_set_error(info, INVALID.as_ptr());
        }
    }));
    if result.is_err() {
        unsafe {
            duckdb_scalar_function_bind_set_error(info, INTERNAL.as_ptr());
        }
    }
}

// The pinned C API scalar wrapper flattens every input vector before invoking
// callbacks. Only the physical layout matching the logical type is read, and
// rows are bounded by this input chunk. Decimal widths select their own layout.
unsafe extern "C" fn invoke(
    info: duckdb_function_info,
    input: duckdb_data_chunk,
    output: duckdb_vector,
) {
    let result = catch_unwind(AssertUnwindSafe(|| unsafe {
        let len = duckdb_data_chunk_get_size(input);
        let source = duckdb_data_chunk_get_vector(input, 0);
        let mut logical = duckdb_vector_get_column_type(source);
        let kind = duckdb_get_type_id(logical);
        let (physical, width, scale) = if kind == Id::Decimal as u32 {
            (
                duckdb_decimal_internal_type(logical),
                duckdb_decimal_width(logical),
                duckdb_decimal_scale(logical),
            )
        } else {
            (kind, 0, 0)
        };
        duckdb_destroy_logical_type(&mut logical);
        if !numeric(kind) {
            return Err(INVALID);
        }
        let data = duckdb_vector_get_data(source);
        let validity = duckdb_vector_get_validity(source);
        duckdb_vector_ensure_validity_writable(output);
        let output_validity = duckdb_vector_get_validity(output);
        let destination = duckdb_vector_get_data(output).cast::<f64>();
        for row in 0..len {
            let valid = validity.is_null() || duckdb_validity_row_is_valid(validity, row);
            duckdb_validity_set_row_validity(output_validity, row, valid);
            if !valid {
                continue;
            }
            let index = row as usize;
            let value = if kind == Id::Decimal as u32 {
                let integer = if physical == Id::Smallint as u32 {
                    *data.cast::<i16>().add(index) as i128
                } else if physical == Id::Integer as u32 {
                    *data.cast::<i32>().add(index) as i128
                } else if physical == Id::Bigint as u32 {
                    *data.cast::<i64>().add(index) as i128
                } else if physical == Id::Hugeint as u32 {
                    let v = *data.cast::<duckdb_hugeint>().add(index);
                    ((v.upper as i128) << 64) | v.lower as i128
                } else {
                    return Err(INTERNAL);
                };
                duckdb_decimal_to_double(duckdb_decimal {
                    width,
                    scale,
                    value: duckdb_hugeint {
                        lower: integer as u64,
                        upper: (integer >> 64) as i64,
                    },
                })
            } else if kind == Id::Boolean as u32 || kind == Id::UTinyint as u32 {
                *data.cast::<u8>().add(index) as f64
            } else if kind == Id::Tinyint as u32 {
                *data.cast::<i8>().add(index) as f64
            } else if kind == Id::Smallint as u32 {
                *data.cast::<i16>().add(index) as f64
            } else if kind == Id::Integer as u32 {
                *data.cast::<i32>().add(index) as f64
            } else if kind == Id::Bigint as u32 {
                *data.cast::<i64>().add(index) as f64
            } else if kind == Id::USmallint as u32 {
                *data.cast::<u16>().add(index) as f64
            } else if kind == Id::UInteger as u32 {
                *data.cast::<u32>().add(index) as f64
            } else if kind == Id::UBigint as u32 {
                *data.cast::<u64>().add(index) as f64
            } else if kind == Id::Hugeint as u32 {
                duckdb_hugeint_to_double(*data.cast::<duckdb_hugeint>().add(index))
            } else if kind == Id::Float as u32 {
                *data.cast::<f32>().add(index) as f64
            } else if kind == Id::Double as u32 {
                *data.cast::<f64>().add(index)
            } else {
                return Err(INTERNAL);
            };
            *destination.add(index) = value;
        }
        Ok(())
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

pub fn register(db: &Connection) -> duckdb::Result<()> {
    unsafe {
        let mut function = duckdb_create_scalar_function();
        let mut any = duckdb_create_logical_type(Id::Any as u32);
        let mut double = duckdb_create_logical_type(Id::Double as u32);
        duckdb_scalar_function_set_name(function, c"__msduck_percentile_input".as_ptr());
        duckdb_scalar_function_add_parameter(function, any);
        duckdb_scalar_function_set_return_type(function, double);
        duckdb_scalar_function_set_special_handling(function);
        duckdb_scalar_function_set_bind(function, Some(bind));
        duckdb_scalar_function_set_function(function, Some(invoke));
        let result = db.register_scalar_function_raw(function);
        duckdb_destroy_scalar_function(&mut function);
        duckdb_destroy_logical_type(&mut any);
        duckdb_destroy_logical_type(&mut double);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn numeric_layouts_nulls_and_binding_are_checked_across_chunks() {
        let db = Connection::open_in_memory().unwrap();
        register(&db).unwrap();
        for kind in [
            "BOOLEAN",
            "TINYINT",
            "SMALLINT",
            "INTEGER",
            "BIGINT",
            "UTINYINT",
            "USMALLINT",
            "UINTEGER",
            "UBIGINT",
            "HUGEINT",
            "FLOAT",
            "DOUBLE",
            "DECIMAL(4,2)",
            "DECIMAL(9,2)",
            "DECIMAL(18,2)",
            "DECIMAL(38,2)",
        ] {
            let sql = format!(
                "SELECT COUNT(*) FROM (SELECT CAST(CASE WHEN i%17=0 THEN NULL ELSE (i%100)/100.0 END AS {kind}) n FROM range(6000) r(i)) s WHERE __msduck_percentile_input(n) IS DISTINCT FROM CAST(n AS DOUBLE)"
            );
            assert_eq!(
                db.query_row(&sql, [], |r| r.get::<_, i64>(0)).unwrap(),
                0,
                "{kind}"
            );
        }
        for kind in [
            "DECIMAL(4,2)",
            "DECIMAL(9,2)",
            "DECIMAL(18,2)",
            "DECIMAL(38,2)",
        ] {
            let sql = format!(
                "SELECT COUNT(*) FROM (SELECT CAST((i-3000)/100.0 AS {kind}) n FROM range(6000) r(i)) s WHERE __msduck_percentile_input(n) IS DISTINCT FROM CAST(n AS DOUBLE)"
            );
            assert_eq!(
                db.query_row(&sql, [], |r| r.get::<_, i64>(0)).unwrap(),
                0,
                "{kind}"
            );
        }
        for value in [
            "123456789012345678901234567890.12",
            "-123456789012345678901234567890.12",
        ] {
            let sql = format!(
                "SELECT __msduck_percentile_input(CAST('{value}' AS DECIMAL(38,2))) = CAST(CAST('{value}' AS DECIMAL(38,2)) AS DOUBLE)"
            );
            assert!(db.query_row(&sql, [], |r| r.get::<_, bool>(0)).unwrap());
        }
        assert_eq!(
            db.query_row("SELECT __msduck_percentile_input(NULL)", [], |r| r
                .get::<_, Option<f64>>(0))
                .unwrap(),
            None
        );
        for kind in [
            "VARCHAR",
            "DATE",
            "TIME",
            "TIMESTAMP",
            "BLOB",
            "UUID",
            "INTEGER[]",
        ] {
            let sql = format!("SELECT __msduck_percentile_input(CAST(NULL AS {kind})) WHERE FALSE");
            assert!(
                db.prepare(&sql)
                    .unwrap_err()
                    .to_string()
                    .contains(INVALID.to_str().unwrap()),
                "{kind}"
            );
        }
        db.execute_batch("CREATE SEQUENCE percentile_once START 1")
            .unwrap();
        let count = db.query_row("SELECT COUNT(__msduck_percentile_input(nextval('percentile_once'))) FROM range(6000)", [], |r| r.get::<_,i64>(0)).unwrap();
        assert_eq!(count, 6000);
        assert_eq!(
            db.query_row("SELECT currval('percentile_once')", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            6000
        );
    }
}
