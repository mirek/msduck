//! Bounded integer SUM/AVG state for DuckDB's aggregate C API.
use duckdb::{Connection, ffi::*};
use std::{
    ffi::CStr,
    panic::{AssertUnwindSafe, catch_unwind},
};

use msduck_core::bounded_aggregate::{State, add};

fn protect(info: duckdb_function_info, action: impl FnOnce()) {
    if catch_unwind(AssertUnwindSafe(action)).is_err() {
        unsafe {
            duckdb_aggregate_function_set_error(info, c"Rust aggregate callback panicked".as_ptr());
        }
    }
}
unsafe fn overflow<const BIG: bool, const MONEY: bool>(info: duckdb_function_info) {
    let message = if MONEY {
        c"Arithmetic overflow error converting expression to data type money."
    } else if BIG {
        c"Arithmetic overflow error converting expression to data type bigint."
    } else {
        c"Arithmetic overflow error converting expression to data type int."
    };
    unsafe {
        duckdb_aggregate_function_set_error(info, message.as_ptr());
    }
}
unsafe extern "C" fn state_size(_: duckdb_function_info) -> idx_t {
    std::mem::size_of::<State>() as idx_t
}
unsafe extern "C" fn init(_: duckdb_function_info, state: duckdb_aggregate_state) {
    // DuckDB allocates the state_size bytes, aligned for aggregate state data.
    unsafe {
        state.cast::<State>().write(State::default());
    }
}
unsafe extern "C" fn update<const BIG: bool, const MONEY: bool>(
    info: duckdb_function_info,
    input: duckdb_data_chunk,
    states: *mut duckdb_aggregate_state,
) {
    protect(info, || unsafe {
        // DuckDB's C API flattens input vectors and provides one state pointer
        // per input row; states may repeat for rows belonging to the same group.
        let len = duckdb_data_chunk_get_size(input);
        let vector = duckdb_data_chunk_get_vector(input, 0);
        let data = duckdb_vector_get_data(vector);
        let validity = duckdb_vector_get_validity(vector);
        for index in 0..len as usize {
            if !validity.is_null() && !duckdb_validity_row_is_valid(validity, index as idx_t) {
                continue;
            }
            let value = if MONEY {
                let raw = *data.cast::<duckdb_hugeint>().add(index);
                let scaled = (i128::from(raw.upper) << 64) | i128::from(raw.lower);
                let Ok(value) = i64::try_from(scaled) else {
                    (*states.add(index)).cast::<State>().write(State {
                        failed: true,
                        ..State::default()
                    });
                    continue;
                };
                value
            } else if BIG {
                *data.cast::<i64>().add(index)
            } else {
                i64::from(*data.cast::<i32>().add(index))
            };
            let state = (*states.add(index)).cast::<State>();
            if let Some(next) = add::<BIG>(state.read(), value, 1) {
                state.write(next);
            } else {
                state.write(State {
                    failed: true,
                    ..State::default()
                });
            }
        }
    });
}
unsafe extern "C" fn combine<const BIG: bool>(
    info: duckdb_function_info,
    source: *mut duckdb_aggregate_state,
    target: *mut duckdb_aggregate_state,
    count: idx_t,
) {
    protect(info, || unsafe {
        for index in 0..count as usize {
            let source = (*source.add(index)).cast::<State>().read();
            let target = (*target.add(index)).cast::<State>();
            if let Some(next) = (!source.failed)
                .then(|| add::<BIG>(target.read(), source.sum, source.count))
                .flatten()
            {
                target.write(next);
            } else {
                target.write(State {
                    failed: true,
                    ..State::default()
                });
            }
        }
    });
}
unsafe extern "C" fn finalize<const BIG: bool, const AVG: bool, const MONEY: bool>(
    info: duckdb_function_info,
    states: *mut duckdb_aggregate_state,
    output: duckdb_vector,
    count: idx_t,
    offset: idx_t,
) {
    protect(info, || unsafe {
        duckdb_vector_ensure_validity_writable(output);
        let validity = duckdb_vector_get_validity(output);
        let data = duckdb_vector_get_data(output);
        for index in 0..count as usize {
            let state = (*states.add(index)).cast::<State>().read();
            // Window segment trees can build states that no output frame uses.
            // Keep overflow sticky in each state, but report only finalized ones.
            let row = offset as usize + index;
            let value = match state.value(AVG) {
                Err(_) => {
                    overflow::<BIG, MONEY>(info);
                    return;
                }
                Ok(None) => {
                    duckdb_validity_set_row_invalid(validity, row as idx_t);
                    continue;
                }
                Ok(Some(value)) => value,
            };
            duckdb_validity_set_row_valid(validity, row as idx_t);
            if MONEY {
                data.cast::<duckdb_hugeint>()
                    .add(row)
                    .write(duckdb_hugeint {
                        lower: value as u64,
                        upper: if value < 0 { -1 } else { 0 },
                    });
            } else if BIG {
                data.cast::<i64>().add(row).write(value);
            } else {
                data.cast::<i32>().add(row).write(value as i32);
            }
        }
    });
}

fn register_one<const BIG: bool, const AVG: bool, const MONEY: bool>(
    db: &Connection,
    name: &CStr,
) -> duckdb::Result<()> {
    // All callbacks are static and state is plain, trivially dropped data.
    // Registration copies the definition; destroy our construction handles on
    // both success and failure. No raw connection escapes the Rust wrapper.
    unsafe {
        let mut function = duckdb_create_aggregate_function();
        let mut kind = if MONEY {
            duckdb_create_decimal_type(19, 4)
        } else {
            duckdb_create_logical_type(if BIG {
                DUCKDB_TYPE_DUCKDB_TYPE_BIGINT
            } else {
                DUCKDB_TYPE_DUCKDB_TYPE_INTEGER
            })
        };
        duckdb_aggregate_function_set_name(function, name.as_ptr());
        duckdb_aggregate_function_add_parameter(function, kind);
        duckdb_aggregate_function_set_return_type(function, kind);
        duckdb_aggregate_function_set_functions(
            function,
            Some(state_size),
            Some(init),
            Some(update::<BIG, MONEY>),
            Some(combine::<BIG>),
            Some(finalize::<BIG, AVG, MONEY>),
        );
        let result = if function.is_null() || kind.is_null() {
            Err(duckdb::Error::DuckDBFailure(
                duckdb::ffi::Error::new(DuckDBError),
                Some("could not allocate aggregate definition".into()),
            ))
        } else {
            db.register_aggregate_function(function)
        };
        duckdb_destroy_logical_type(&mut kind);
        duckdb_destroy_aggregate_function(&mut function);
        result
    }
}
pub fn register(db: &Connection) -> duckdb::Result<()> {
    register_one::<false, false, false>(db, c"__msduck_sum_int")?;
    register_one::<true, false, false>(db, c"__msduck_sum_big")?;
    register_one::<false, true, false>(db, c"__msduck_avg_int")?;
    register_one::<true, true, false>(db, c"__msduck_avg_big")?;
    register_one::<true, false, true>(db, c"__msduck_sum_money")?;
    register_one::<true, true, true>(db, c"__msduck_avg_money")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn currency_chunks_exact_coefficients_and_single_evaluation() {
        let db = Connection::open_in_memory().unwrap();
        register(&db).unwrap();
        db.execute_batch("CREATE SEQUENCE money_aggregate_calls")
            .unwrap();
        let actual: (String, String) = db.query_row(
            "SELECT CAST(__msduck_sum_money(m) AS VARCHAR),CAST(__msduck_avg_money(m) AS VARCHAR) FROM (SELECT CAST(CASE WHEN nextval('money_aggregate_calls')%17=0 THEN NULL ELSE i-3000 END AS DECIMAL(19,4)) m FROM range(6000) r(i)) s", [],
            |row| Ok((row.get(0)?,row.get(1)?)),
        ).unwrap();
        let values = (0..6000i64)
            .filter(|i| (i + 1) % 17 != 0)
            .collect::<Vec<_>>();
        let sum = values.iter().map(|i| i - 3000).sum::<i64>();
        let avg = sum * 10000 / values.len() as i64;
        assert_eq!(actual.0, format!("{sum}.0000"));
        assert_eq!(
            actual.1,
            format!(
                "{}{}.{:04}",
                if avg < 0 { "-" } else { "" },
                avg.abs() / 10000,
                avg.abs() % 10000
            )
        );
        assert_eq!(
            db.query_row("SELECT currval('money_aggregate_calls')", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            6000
        );
        // More than one vector of identical whole-partition window states.
        let wrong: i64 = db.query_row("SELECT count(*) FROM (SELECT __msduck_sum_money(CAST(i%2 AS DECIMAL(19,4))) OVER() s,__msduck_avg_money(CAST(i%2 AS DECIMAL(19,4))) OVER() a FROM range(6000) r(i)) WHERE s<>3000 OR a<>0.5",[],|r|r.get(0)).unwrap();
        assert_eq!(wrong, 0);
    }
    #[test]
    fn whole_partition_windows_flatten_constant_state_vectors() {
        let db = Connection::open_in_memory().unwrap();
        register(&db).unwrap();
        let mut statement = db.prepare("SELECT __msduck_sum_int(n) OVER (), __msduck_avg_int(n) OVER (), __msduck_sum_big(CAST(n AS BIGINT)) OVER (PARTITION BY n % 2) FROM (SELECT CAST(i AS INT) n FROM range(6000) r(i)) s ORDER BY n").unwrap();
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i32>(0)?,
                    row.get::<_, i32>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .unwrap()
            .collect::<duckdb::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(rows.len(), 6000);
        for (n, row) in rows.into_iter().enumerate() {
            assert_eq!(
                row,
                (
                    17_997_000,
                    2999,
                    if n % 2 == 0 { 8_997_000 } else { 9_000_000 }
                )
            );
        }
        let mut statement = db.prepare("SELECT __msduck_sum_int(__msduck_sum_int(n)) OVER () FROM (VALUES (1,1),(1,2),(2,4)) s(g,n) GROUP BY g").unwrap();
        let rows = statement
            .query_map([], |row| row.get::<_, i32>(0))
            .unwrap()
            .collect::<duckdb::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(rows, vec![7, 7]);
    }
}

#[cfg(test)]
mod execution_tests {
    #[test]
    fn native_groups_windows_parallel_combine_and_single_evaluation() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        db.execute_batch("SET threads=4").unwrap();
        let wrong: i64 = db.query_row("SELECT count(*) FROM (SELECT n%10000 AS g,__msduck_sum_int(CAST(n%37 AS INT)) s,__msduck_avg_big(n%37) a,CAST(sum(n%37) AS INT) expected_s,sum(n%37)//count(*) expected_a FROM range(200000) r(n) GROUP BY g) WHERE s<>expected_s OR a<>expected_a", [], |r| r.get(0)).unwrap();
        assert_eq!(wrong, 0);
        let wrong: i64 = db.query_row("SELECT count(*) FROM (SELECT __msduck_sum_int(2147483647) OVER (ORDER BY n ROWS BETWEEN CURRENT ROW AND CURRENT ROW) s FROM range(6000) r(n)) WHERE s<>2147483647", [], |r| r.get(0)).unwrap();
        assert_eq!(wrong, 0);
        for (function, expected) in [("sum", 18003000), ("avg", 3000)] {
            db.execute_batch("CREATE OR REPLACE SEQUENCE evals START 1")
                .unwrap();
            let value: i64 = db
                .query_row(
                    &format!("SELECT __msduck_{function}_big(nextval('evals')) FROM range(6000)"),
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(value, expected);
            let count: i64 = db
                .query_row("SELECT currval('evals')", [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 6000);
        }
        db.execute_batch("SET threads=1").unwrap();
        let error = db
            .query_row(
                "SELECT __msduck_sum_int(n) FROM (VALUES (2147483647),(1),(-1)) s(n)",
                [],
                |r| r.get::<_, i32>(0),
            )
            .unwrap_err();
        assert!(error.to_string().contains("Arithmetic overflow"));
    }
}
