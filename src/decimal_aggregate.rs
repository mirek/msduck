//! Native vectors and aggregate lifecycle for exact DECIMAL AVG.
use duckdb::{Connection, ffi::*};
use msduck_core::{decimal_aggregate::State, types::DecimalType};
use std::{
    ffi::{CString, c_void},
    panic::{AssertUnwindSafe, catch_unwind},
};

fn protect(info: duckdb_function_info, action: impl FnOnce()) {
    if catch_unwind(AssertUnwindSafe(action)).is_err() {
        unsafe {
            duckdb_aggregate_function_set_error(info, c"Rust aggregate callback panicked".as_ptr());
        }
    }
}
unsafe extern "C" fn size(_: duckdb_function_info) -> idx_t {
    std::mem::size_of::<State>() as idx_t
}
unsafe extern "C" fn init(_: duckdb_function_info, state: duckdb_aggregate_state) {
    // Do not require DuckDB's state allocation to have Rust i128 alignment.
    unsafe {
        state.cast::<State>().write_unaligned(State::default());
    }
}
unsafe extern "C" fn update(
    info: duckdb_function_info,
    input: duckdb_data_chunk,
    states: *mut duckdb_aggregate_state,
) {
    protect(info, || unsafe {
        let vector = duckdb_data_chunk_get_vector(input, 0);
        let data = duckdb_vector_get_data(vector).cast::<duckdb_hugeint>();
        let validity = duckdb_vector_get_validity(vector);
        for index in 0..duckdb_data_chunk_get_size(input) as usize {
            if !validity.is_null() && !duckdb_validity_row_is_valid(validity, index as idx_t) {
                continue;
            }
            // Registration forces DECIMAL(38,s), whose physical storage is HUGEINT.
            let raw = data.add(index).read_unaligned();
            let coefficient = (i128::from(raw.upper) << 64) | i128::from(raw.lower);
            let pointer = (*states.add(index)).cast::<State>();
            let mut state = pointer.read_unaligned();
            state.push(coefficient);
            pointer.write_unaligned(state);
        }
    });
}
unsafe extern "C" fn combine(
    info: duckdb_function_info,
    source: *mut duckdb_aggregate_state,
    target: *mut duckdb_aggregate_state,
    count: idx_t,
) {
    protect(info, || unsafe {
        for index in 0..count as usize {
            let source = (*source.add(index)).cast::<State>().read_unaligned();
            let pointer = (*target.add(index)).cast::<State>();
            let mut state = pointer.read_unaligned();
            state.combine(source);
            pointer.write_unaligned(state);
        }
    });
}
unsafe extern "C" fn finalize(
    info: duckdb_function_info,
    states: *mut duckdb_aggregate_state,
    output: duckdb_vector,
    count: idx_t,
    offset: idx_t,
) {
    protect(info, || unsafe {
        let input = *duckdb_aggregate_function_get_extra_info(info).cast::<DecimalType>();
        duckdb_vector_ensure_validity_writable(output);
        let validity = duckdb_vector_get_validity(output);
        let data = duckdb_vector_get_data(output).cast::<duckdb_hugeint>();
        for index in 0..count as usize {
            let state = (*states.add(index)).cast::<State>().read_unaligned();
            let row = offset as usize + index;
            match state.average(input) {
                Ok(Some(value)) => {
                    data.add(row).write_unaligned(duckdb_hugeint {
                        lower: value as u64,
                        upper: (value >> 64) as i64,
                    });
                    duckdb_validity_set_row_valid(validity, row as idx_t);
                }
                Ok(None) => duckdb_validity_set_row_invalid(validity, row as idx_t),
                Err(_) => {
                    duckdb_aggregate_function_set_error(
                        info,
                        c"Arithmetic overflow error converting expression to data type numeric."
                            .as_ptr(),
                    );
                    return;
                }
            }
        }
    });
}
unsafe extern "C" fn destroy(pointer: *mut c_void) {
    unsafe {
        drop(Box::from_raw(pointer.cast::<DecimalType>()));
    }
}

pub fn register(db: &Connection) -> duckdb::Result<()> {
    for scale in 0..=38 {
        let input = DecimalType::new(38, scale).expect("valid registered scale");
        let name = CString::new(format!("__msduck_avg_decimal_{scale}")).unwrap();
        unsafe {
            let mut function = duckdb_create_aggregate_function();
            let mut source = duckdb_create_decimal_type(38, scale);
            let mut result = duckdb_create_decimal_type(38, scale.max(6));
            if function.is_null() || source.is_null() || result.is_null() {
                duckdb_destroy_logical_type(&mut source);
                duckdb_destroy_logical_type(&mut result);
                duckdb_destroy_aggregate_function(&mut function);
                return Err(duckdb::Error::DuckDBFailure(
                    duckdb::ffi::Error::new(DuckDBError),
                    Some("could not allocate decimal aggregate definition".into()),
                ));
            }
            duckdb_aggregate_function_set_name(function, name.as_ptr());
            duckdb_aggregate_function_add_parameter(function, source);
            duckdb_aggregate_function_set_return_type(function, result);
            duckdb_aggregate_function_set_extra_info(
                function,
                Box::into_raw(Box::new(input)).cast(),
                Some(destroy),
            );
            duckdb_aggregate_function_set_functions(
                function,
                Some(size),
                Some(init),
                Some(update),
                Some(combine),
                Some(finalize),
            );
            let registered = db.register_aggregate_function(function);
            // DuckDB shares the extra-info owner with the registered definition.
            duckdb_destroy_logical_type(&mut source);
            duckdb_destroy_logical_type(&mut result);
            duckdb_destroy_aggregate_function(&mut function);
            registered?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn conditional_and_arithmetic_arguments_use_exact_session_averages() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        for (index, (argument, expected)) in [
            ("d+CAST(0 AS DECIMAL(5,2))", "0.666666"),
            ("COALESCE(d,CAST(0 AS DECIMAL(5,2)))", "0.666666"),
            (
                "CASE WHEN g=1 THEN d ELSE CAST(0 AS DECIMAL(5,2)) END",
                "0.333333",
            ),
            ("ISNULL(d,CAST(0 AS DECIMAL(5,2)))", "0.666666"),
        ]
        .into_iter()
        .enumerate()
        {
            let sql = format!(
                "SELECT AVG({argument}) AS a INTO dbo.decimal_expression_{index} FROM (VALUES(1,CAST(1 AS DECIMAL(5,2))),(1,CAST(0 AS DECIMAL(5,2))),(2,CAST(1 AS DECIMAL(5,2)))) t(g,d)"
            );
            let response = session.batch_response(&sql, &Default::default(), false, None);
            assert!(response.1, "{sql}");
            let actual: (String, String) = session
                .db
                .query_row(
                    &format!(
                        "SELECT CAST(a AS VARCHAR),typeof(a) FROM dbo.decimal_expression_{index}"
                    ),
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!(
                actual,
                (expected.into(), "DECIMAL(38,6)".into()),
                "{argument}"
            );
        }
    }
    #[test]
    fn exact_scales_distinct_nulls_and_overflow() {
        let db = Connection::open_in_memory().unwrap();
        register(&db).unwrap();
        for scale in 0..=38 {
            let value = if scale == 38 { "0.1" } else { "1" };
            let expected = if scale == 38 {
                format!("0.0{}", "3".repeat(37))
            } else {
                format!("0.{}", "3".repeat(scale.max(6) as usize))
            };
            // Compare exact DECIMAL values. Native DuckDB's VARCHAR rendering
            // omits the leading zero at scale 38; SQL formatting is a separate adapter.
            let sql = format!(
                "SELECT __msduck_avg_decimal_{scale}(d)=CAST('{expected}' AS DECIMAL(38,{})) FROM (VALUES(CAST('{value}' AS DECIMAL(38,{scale}))),(0),(0)) t(d)",
                scale.max(6)
            );
            let exact: bool = db.query_row(&sql, [], |r| r.get(0)).unwrap();
            assert!(exact, "scale {scale}");
        }
        let values: (String, Option<String>, Option<String>) = db.query_row("SELECT CAST(__msduck_avg_decimal_2(DISTINCT d) AS VARCHAR),CAST(__msduck_avg_decimal_2(NULL) AS VARCHAR),CAST(__msduck_avg_decimal_2(d) FILTER(WHERE false) AS VARCHAR) FROM (VALUES(CAST(1 AS DECIMAL(38,2))),(1),(0),(NULL)) t(d)", [], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
        assert_eq!(values, ("0.500000".into(), None, None));
        for sql in [
            "SELECT __msduck_avg_decimal_0(CAST('999999999999999999999999999999999' AS DECIMAL(38,0)))",
            "SELECT __msduck_avg_decimal_6(d) FROM (VALUES(CAST('60000000000000000000000000000000' AS DECIMAL(38,6))),(CAST('60000000000000000000000000000000' AS DECIMAL(38,6)))) t(d)",
        ] {
            assert!(
                db.query_row(sql, [], |r| r.get::<_, String>(0))
                    .unwrap_err()
                    .to_string()
                    .contains("Arithmetic overflow")
            );
        }
    }
    #[test]
    fn chunks_windows_parallel_groups_and_single_evaluation() {
        let db = Connection::open_in_memory().unwrap();
        register(&db).unwrap();
        db.execute_batch("SET threads=4; CREATE SEQUENCE calls")
            .unwrap();
        let value: String = db.query_row("SELECT CAST(__msduck_avg_decimal_2(CAST(nextval('calls') AS DECIMAL(38,2))) AS VARCHAR) FROM range(6000)",[],|r|r.get(0)).unwrap();
        assert_eq!(value, "3000.500000");
        assert_eq!(
            db.query_row("SELECT currval('calls')", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            6000
        );
        let wrong: i64 = db.query_row("SELECT count(*) FROM (SELECT __msduck_avg_decimal_2(CAST(i AS DECIMAL(38,2))) OVER() a FROM range(6000) t(i)) WHERE a<>2999.5",[],|r|r.get(0)).unwrap();
        assert_eq!(wrong, 0);
        let wrong: i64 = db.query_row("SELECT count(*) FROM (SELECT __msduck_avg_decimal_6(CAST('99999999999999999999999999999999.999999' AS DECIMAL(38,6))) OVER(ORDER BY i ROWS BETWEEN CURRENT ROW AND CURRENT ROW) a FROM range(6000) t(i)) WHERE a<>CAST('99999999999999999999999999999999.999999' AS DECIMAL(38,6))", [], |r|r.get(0)).unwrap();
        assert_eq!(wrong, 0);
        let wrong: i64 = db.query_row("SELECT count(*) FROM (SELECT i%10000 g,__msduck_avg_decimal_2(CAST(i AS DECIMAL(38,2))) a FROM range(200000) t(i) GROUP BY g) WHERE a<>g+95000",[],|r|r.get(0)).unwrap();
        assert_eq!(wrong, 0);
    }
}
