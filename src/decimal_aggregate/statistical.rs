//! Sequential statistical evaluation over DuckDB-owned typed input lists.
use duckdb::{Connection, core::LogicalTypeId as Id, ffi::*};
use msduck_core::bounded_aggregate::{FloatStatsState, Statistic};
use std::{
    ffi::{CStr, c_void},
    panic::{AssertUnwindSafe, catch_unwind},
};

const CAPACITY: idx_t = 1024;
const INTERNAL: &CStr = c"Invalid native statistical list representation.";
const OVERFLOW: &CStr = c"Arithmetic overflow error converting expression to data type float.";

struct Scratch {
    vector: duckdb_vector,
    selection: duckdb_selection_vector,
}
impl Scratch {
    unsafe fn new() -> Result<Self, &'static CStr> {
        unsafe {
            let mut kind = duckdb_create_logical_type(Id::Double as u32);
            if kind.is_null() {
                return Err(INTERNAL);
            }
            let vector = duckdb_create_vector(kind, CAPACITY);
            duckdb_destroy_logical_type(&mut kind);
            let scratch = Self {
                vector,
                selection: duckdb_create_selection_vector(CAPACITY),
            };
            if scratch.vector.is_null() || scratch.selection.is_null() {
                return Err(INTERNAL);
            }
            Ok(scratch)
        }
    }

    unsafe fn accumulate(
        &mut self,
        child: duckdb_vector,
        entry: duckdb_list_entry,
        child_size: idx_t,
    ) -> Result<FloatStatsState, &'static CStr> {
        let end = entry.offset.checked_add(entry.length).ok_or(INTERNAL)?;
        if end > child_size {
            return Err(INTERNAL);
        }
        let mut state = FloatStatsState::default();
        let mut at = entry.offset;
        while at < end {
            let count = CAPACITY.min(end - at);
            unsafe {
                let selection = duckdb_selection_vector_get_data_ptr(self.selection);
                if selection.is_null() {
                    return Err(INTERNAL);
                }
                for index in 0..count {
                    selection
                        .add(index as usize)
                        .write_unaligned((at + index).try_into().map_err(|_| INTERNAL)?);
                }
                // Parent Flatten does not recursively flatten a constant LIST's
                // child. Copy resolves child constant/dictionary selections into
                // this bounded flat DOUBLE vector before any raw read.
                duckdb_vector_copy_sel(child, self.vector, self.selection, count, 0, 0);
                let data = duckdb_vector_get_data(self.vector).cast::<f64>();
                let validity = duckdb_vector_get_validity(self.vector);
                if data.is_null() {
                    return Err(INTERNAL);
                }
                for index in 0..count {
                    if validity.is_null() || duckdb_validity_row_is_valid(validity, index) {
                        state.push(data.add(index as usize).read_unaligned());
                    }
                }
            }
            at += count;
        }
        Ok(state)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        unsafe {
            if !self.vector.is_null() {
                duckdb_destroy_vector(&mut self.vector);
            }
            if !self.selection.is_null() {
                duckdb_destroy_selection_vector(self.selection);
            }
        }
    }
}

unsafe extern "C" fn invoke(
    info: duckdb_function_info,
    input: duckdb_data_chunk,
    output: duckdb_vector,
) {
    let result = catch_unwind(AssertUnwindSafe(|| unsafe {
        let statistic = *duckdb_scalar_function_get_extra_info(info).cast::<Statistic>();
        let source = duckdb_data_chunk_get_vector(input, 0);
        let entries = duckdb_vector_get_data(source).cast::<duckdb_list_entry>();
        let validity = duckdb_vector_get_validity(source);
        let child = duckdb_list_vector_get_child(source);
        let child_size = duckdb_list_vector_get_size(source);
        if entries.is_null() || child.is_null() {
            return Err(INTERNAL);
        }
        let mut scratch = Scratch::new()?;
        duckdb_vector_ensure_validity_writable(output);
        let output_validity = duckdb_vector_get_validity(output);
        let destination = duckdb_vector_get_data(output).cast::<f64>();
        for row in 0..duckdb_data_chunk_get_size(input) {
            if !validity.is_null() && !duckdb_validity_row_is_valid(validity, row) {
                duckdb_validity_set_row_invalid(output_validity, row);
                continue;
            }
            let state = scratch.accumulate(
                child,
                entries.add(row as usize).read_unaligned(),
                child_size,
            )?;
            match state.value(statistic).map_err(|_| OVERFLOW)? {
                Some(value) => {
                    destination.add(row as usize).write_unaligned(value);
                    duckdb_validity_set_row_valid(output_validity, row);
                }
                None => duckdb_validity_set_row_invalid(output_validity, row),
            }
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
    unsafe { drop(Box::from_raw(pointer.cast::<Statistic>())) }
}

pub(super) fn register(db: &Connection) -> duckdb::Result<()> {
    for (name, statistic) in [
        (c"__msduck_list_stddev_samp", Statistic::Stdev),
        (c"__msduck_list_stddev_pop", Statistic::Stdevp),
        (c"__msduck_list_var_samp", Statistic::Var),
        (c"__msduck_list_var_pop", Statistic::Varp),
    ] {
        unsafe {
            let mut function = duckdb_create_scalar_function();
            let mut double = duckdb_create_logical_type(Id::Double as u32);
            let mut list = if double.is_null() {
                std::ptr::null_mut()
            } else {
                duckdb_create_list_type(double)
            };
            if function.is_null() || double.is_null() || list.is_null() {
                duckdb_destroy_scalar_function(&mut function);
                duckdb_destroy_logical_type(&mut double);
                duckdb_destroy_logical_type(&mut list);
                return Err(duckdb::Error::DuckDBFailure(
                    duckdb::ffi::Error::new(DuckDBError),
                    Some("could not allocate statistical scalar definition".into()),
                ));
            }
            duckdb_scalar_function_set_name(function, name.as_ptr());
            duckdb_scalar_function_add_parameter(function, list);
            duckdb_scalar_function_set_return_type(function, double);
            duckdb_scalar_function_set_special_handling(function);
            duckdb_scalar_function_set_extra_info(
                function,
                Box::into_raw(Box::new(statistic)).cast(),
                Some(destroy),
            );
            duckdb_scalar_function_set_function(function, Some(invoke));
            let result = db.register_scalar_function_raw(function);
            duckdb_destroy_scalar_function(&mut function);
            duckdb_destroy_logical_type(&mut double);
            duckdb_destroy_logical_type(&mut list);
            result?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scratch_copies_constant_and_dictionary_children_and_checks_bounds() {
        unsafe {
            let mut kind = duckdb_create_logical_type(Id::Double as u32);
            let source = Scratch {
                vector: duckdb_create_vector(kind, 6000),
                selection: duckdb_create_selection_vector(6000),
            };
            duckdb_destroy_logical_type(&mut kind);
            let data = duckdb_vector_get_data(source.vector).cast::<f64>();
            duckdb_vector_ensure_validity_writable(source.vector);
            let validity = duckdb_vector_get_validity(source.vector);
            let selection = duckdb_selection_vector_get_data_ptr(source.selection);
            let mut expected = FloatStatsState::default();
            for index in 0..6000 {
                data.add(index).write_unaligned((index % 3) as f64);
                duckdb_validity_set_row_validity(validity, index as idx_t, index % 17 != 0);
                selection.add(index).write_unaligned((5999 - index) as u32);
                let reversed = 5999 - index;
                if reversed % 17 != 0 {
                    expected.push((reversed % 3) as f64);
                }
            }
            duckdb_slice_vector(source.vector, source.selection, 6000);
            let mut scratch = Scratch::new().unwrap();
            let entry = duckdb_list_entry {
                offset: 0,
                length: 6000,
            };
            let state = scratch.accumulate(source.vector, entry, 6000).unwrap();
            assert_eq!(state.count, expected.count);
            assert_eq!(state.value(Statistic::Var), expected.value(Statistic::Var));

            let mut value = duckdb_create_double(2.0);
            duckdb_vector_reference_value(source.vector, value);
            duckdb_destroy_value(&mut value);
            let state = scratch.accumulate(source.vector, entry, 6000).unwrap();
            assert_eq!(state.count, 6000);
            assert_eq!(state.value(Statistic::Varp), Ok(Some(0.0)));
            for invalid in [
                duckdb_list_entry {
                    offset: 6000,
                    length: 1,
                },
                duckdb_list_entry {
                    offset: u64::MAX,
                    length: 2,
                },
            ] {
                assert!(scratch.accumulate(source.vector, invalid, 6000).is_err());
            }
        }
    }

    #[test]
    fn list_statistics_keep_empty_null_and_singleton_results() {
        let db = Connection::open_in_memory().unwrap();
        register(&db).unwrap();
        for (input, sample, population) in [
            ("[]::DOUBLE[]", None, None),
            ("NULL::DOUBLE[]", None, None),
            ("[NULL,NULL]::DOUBLE[]", None, None),
            ("[NULL,42]::DOUBLE[]", None, Some(0.0)),
            (
                "[1,2,2]::DOUBLE[]",
                Some(f64::from_bits(0x3fd5555555555550)),
                Some(f64::from_bits(0x3fcc71c71c71c715)),
            ),
        ] {
            let row: (Option<f64>, Option<f64>, String) = db.query_row(
                &format!("SELECT __msduck_list_var_samp({input}),__msduck_list_var_pop({input}),typeof(__msduck_list_var_pop({input}))"),
                [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
            ).unwrap();
            assert_eq!(row, (sample, population, "DOUBLE".into()), "{input}");
        }
    }

    #[test]
    fn session_lowering_preserves_values_evaluation_and_overflow_diagnostics() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        for (index, (sql, expected)) in [
            ("SELECT VAR(v) AS v INTO dbo.stats_0 FROM (VALUES(1),(2),(2)) d(v)", 0x3fd5555555555550),
            ("SELECT VAR(DISTINCT v) AS v INTO dbo.stats_1 FROM (VALUES(CAST(9007199254740992 AS BIGINT)),(CAST(9007199254740993 AS BIGINT)),(CAST(9007199254740993 AS BIGINT))) d(v)", 0),
        ].into_iter().enumerate() {
            assert!(session.batch_response(sql, &Default::default(), false, None).1, "{sql}");
            let value: f64 = session.db.query_row(&format!("SELECT v FROM dbo.stats_{index}"), [], |r| r.get(0)).unwrap();
            assert_eq!(value.to_bits(), expected, "{sql}");
        }
        session
            .db
            .execute_batch("CREATE SEQUENCE stat_calls")
            .unwrap();
        let sql = "SELECT VAR(CASE WHEN nextval('stat_calls')%2=0 THEN NULL ELSE v END) OVER(ORDER BY v ROWS UNBOUNDED PRECEDING) FROM (VALUES(1),(2),(3),(4)) d(v)";
        assert!(
            session
                .batch_response(sql, &Default::default(), false, None)
                .1
        );
        let calls: i64 = session
            .db
            .query_row("SELECT currval('stat_calls')", [], |r| r.get(0))
            .unwrap();
        assert_eq!(calls, 4);
        let (tokens, success) = session.batch_response(
            "SELECT VAR(v) FROM (VALUES(CAST('1e308' AS FLOAT)),(CAST('-1e308' AS FLOAT))) d(v)",
            &Default::default(),
            false,
            None,
        );
        assert!(!success);
        let mut error = Vec::new();
        crate::tds::sql_error(
            &mut error,
            &msduck_core::diagnostic::SqlError::new(8115, 2, OVERFLOW.to_str().unwrap()),
        );
        assert!(tokens.windows(error.len()).any(|part| part == error));
        assert!(
            session
                .batch_response("SELECT 1", &Default::default(), false, None)
                .1
        );
    }
}
