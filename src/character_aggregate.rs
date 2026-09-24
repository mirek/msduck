//! Native BIN2 extrema retain the selected UTF16 payload, not a comparison key.
//! Logical collation selection and public SQL lowering belong to the binder.
use duckdb::{Connection, ffi::*};
use std::{
    cmp::Ordering,
    ffi::{CStr, c_void},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering as AtomicOrdering},
    },
};

struct State {
    units: Option<Box<[u16]>>,
    // Shared by states of this registered function, including parallel/window
    // states. Destruction releases retained memory after success or failure.
    budget: Arc<AtomicUsize>,
}
impl State {
    fn replace(&mut self, units: &[u16]) -> Result<(), &'static str> {
        let old = self.units.as_ref().map_or(0, |u| u.len() * 2);
        let new = units.len() * 2;
        let mut copy = Vec::new();
        copy.try_reserve_exact(units.len())
            .map_err(|_| "character aggregate allocation failed")?;
        if new > old {
            self.budget
                .fetch_update(AtomicOrdering::Relaxed, AtomicOrdering::Relaxed, |used| {
                    used.checked_add(new - old)
                        .filter(|n| *n <= crate::unicode_carrier::CHUNK_LIMIT)
                })
                .map_err(|_| "character aggregate retained payload limit exceeded")?;
        } else {
            self.budget.fetch_sub(old - new, AtomicOrdering::Relaxed);
        }
        copy.extend_from_slice(units);
        self.units = Some(copy.into_boxed_slice());
        Ok(())
    }
    fn update<const MAX: bool, const ANSI: bool>(
        &mut self,
        units: &[u16],
    ) -> Result<(), &'static str> {
        if units.len() > crate::unicode_carrier::CELL_LIMIT / 2 {
            return Err("character aggregate value limit exceeded");
        }
        let order = if let Some(previous) = &self.units {
            if ANSI {
                let encode = |u: &[u16]| {
                    String::from_utf16(u)
                        .ok()
                        .and_then(|text| msduck_core::encoding::encode_cp1252(&text).ok())
                        .ok_or("invalid CP1252 character aggregate input")
                };
                msduck_core::bin2::compare_bytes(&encode(units)?, &encode(previous)?)
            } else {
                msduck_core::bin2::compare(units, previous)
            }
        } else {
            if ANSI {
                let text = String::from_utf16(units)
                    .map_err(|_| "invalid CP1252 character aggregate input")?;
                msduck_core::encoding::encode_cp1252(&text)
                    .map_err(|_| "invalid CP1252 character aggregate input")?;
            }
            return self.replace(units);
        };
        if order
            == if MAX {
                Ordering::Greater
            } else {
                Ordering::Less
            }
        {
            self.replace(units)?
        }
        Ok(())
    }
}
impl Drop for State {
    fn drop(&mut self) {
        self.budget.fetch_sub(
            self.units.as_ref().map_or(0, |u| u.len() * 2),
            AtomicOrdering::Relaxed,
        );
    }
}
fn protect(info: duckdb_function_info, action: impl FnOnce() -> Result<(), &'static str>) {
    let message = match catch_unwind(AssertUnwindSafe(action)) {
        Ok(Ok(())) => return,
        Ok(Err(message)) => message,
        Err(_) => "Rust character aggregate callback panicked",
    };
    let message = std::ffi::CString::new(message).expect("static diagnostic has no NUL");
    unsafe {
        duckdb_aggregate_function_set_error(info, message.as_ptr());
    }
}
unsafe extern "C" fn size(_: duckdb_function_info) -> idx_t {
    std::mem::size_of::<State>() as idx_t
}
unsafe extern "C" fn init(info: duckdb_function_info, state: duckdb_aggregate_state) {
    unsafe {
        let budget = &*duckdb_aggregate_function_get_extra_info(info).cast::<Arc<AtomicUsize>>();
        state.cast::<State>().write(State {
            units: None,
            budget: budget.clone(),
        });
    }
}
unsafe extern "C" fn destroy(states: *mut duckdb_aggregate_state, count: idx_t) {
    for index in 0..count as usize {
        unsafe {
            std::ptr::drop_in_place((*states.add(index)).cast::<State>());
        }
    }
}
unsafe extern "C" fn destroy_budget(pointer: *mut c_void) {
    unsafe {
        drop(Box::from_raw(pointer.cast::<Arc<AtomicUsize>>()));
    }
}
unsafe fn valid(vector: duckdb_vector, row: idx_t) -> bool {
    unsafe {
        let mask = duckdb_vector_get_validity(vector);
        mask.is_null() || duckdb_validity_row_is_valid(mask, row)
    }
}
unsafe extern "C" fn update<const MAX: bool, const ANSI: bool>(
    info: duckdb_function_info,
    input: duckdb_data_chunk,
    states: *mut duckdb_aggregate_state,
) {
    protect(info, || unsafe {
        // The aggregate C API supplies flattened input and one state per row.
        // Registration fixes the input to the exact named STRUCT<BLOB> shape.
        let count = duckdb_data_chunk_get_size(input);
        let parent = duckdb_data_chunk_get_vector(input, 0);
        let payload = duckdb_struct_vector_get_child(parent, 0);
        let data = duckdb_vector_get_data(payload).cast::<duckdb_string_t>();
        for row in 0..count {
            if !valid(parent, row) {
                continue;
            }
            if !valid(payload, row) {
                return Err("invalid Unicode carrier: NULL payload");
            }
            let mut value = *data.add(row as usize);
            let bytes = duckdb_string_t_length(value) as usize;
            if bytes > crate::unicode_carrier::CELL_LIMIT {
                return Err("character aggregate value limit exceeded");
            }
            if !bytes.is_multiple_of(2) {
                return Err("invalid Unicode carrier byte length");
            }
            let raw = if bytes == 0 {
                &[]
            } else {
                std::slice::from_raw_parts(duckdb_string_t_data(&mut value).cast::<u8>(), bytes)
            };
            let units: Vec<_> = raw
                .chunks_exact(2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]))
                .collect();
            (&mut *(*states.add(row as usize)).cast::<State>()).update::<MAX, ANSI>(&units)?;
        }
        Ok(())
    });
}
unsafe extern "C" fn combine<const MAX: bool, const ANSI: bool>(
    info: duckdb_function_info,
    source: *mut duckdb_aggregate_state,
    target: *mut duckdb_aggregate_state,
    count: idx_t,
) {
    protect(info, || unsafe {
        for index in 0..count as usize {
            let a = *source.add(index);
            let b = *target.add(index);
            if a == b {
                continue;
            }
            if let Some(units) = &(*a.cast::<State>()).units {
                (&mut *b.cast::<State>()).update::<MAX, ANSI>(units)?;
            }
        }
        Ok(())
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
        let payload = duckdb_struct_vector_get_child(output, 0);
        duckdb_vector_ensure_validity_writable(output);
        duckdb_vector_ensure_validity_writable(payload);
        let parent_mask = duckdb_vector_get_validity(output);
        let child_mask = duckdb_vector_get_validity(payload);
        let mut remaining = crate::unicode_carrier::CHUNK_LIMIT;
        for index in 0..count as usize {
            let row = offset + index as idx_t;
            let state = &*(*states.add(index)).cast::<State>();
            if let Some(units) = &state.units {
                remaining = remaining
                    .checked_sub(units.len() * 2)
                    .ok_or("character aggregate output limit exceeded")?;
                let bytes: Vec<_> = units.iter().flat_map(|u| u.to_le_bytes()).collect();
                duckdb_vector_assign_string_element_len(
                    payload,
                    row,
                    bytes.as_ptr().cast(),
                    bytes.len() as idx_t,
                );
                duckdb_validity_set_row_valid(parent_mask, row);
                duckdb_validity_set_row_valid(child_mask, row);
            } else {
                duckdb_validity_set_row_invalid(parent_mask, row);
                duckdb_validity_set_row_invalid(child_mask, row);
            }
        }
        Ok(())
    });
}
fn register_one<const MAX: bool, const ANSI: bool>(
    db: &Connection,
    name: &CStr,
) -> duckdb::Result<()> {
    unsafe {
        let mut function = duckdb_create_aggregate_function();
        let mut blob = duckdb_create_logical_type(DUCKDB_TYPE_DUCKDB_TYPE_BLOB);
        let mut fields = [blob];
        let mut names = [c"__msduck_utf16le".as_ptr()];
        let mut kind = if blob.is_null() {
            std::ptr::null_mut()
        } else {
            duckdb_create_struct_type(fields.as_mut_ptr(), names.as_mut_ptr(), 1)
        };
        if function.is_null() || blob.is_null() || kind.is_null() {
            duckdb_destroy_logical_type(&mut blob);
            duckdb_destroy_logical_type(&mut kind);
            duckdb_destroy_aggregate_function(&mut function);
            return Err(duckdb::Error::DuckDBFailure(
                duckdb::ffi::Error::new(DuckDBError),
                Some("could not allocate character aggregate definition".into()),
            ));
        }
        duckdb_aggregate_function_set_name(function, name.as_ptr());
        duckdb_aggregate_function_add_parameter(function, kind);
        duckdb_aggregate_function_set_return_type(function, kind);
        duckdb_aggregate_function_set_extra_info(
            function,
            Box::into_raw(Box::new(Arc::new(AtomicUsize::new(0)))).cast(),
            Some(destroy_budget),
        );
        duckdb_aggregate_function_set_functions(
            function,
            Some(size),
            Some(init),
            Some(update::<MAX, ANSI>),
            Some(combine::<MAX, ANSI>),
            Some(finalize),
        );
        duckdb_aggregate_function_set_destructor(function, Some(destroy));
        let result = db.register_aggregate_function(function);
        duckdb_destroy_logical_type(&mut blob);
        duckdb_destroy_logical_type(&mut kind);
        duckdb_destroy_aggregate_function(&mut function);
        result
    }
}
pub fn register(db: &Connection) -> duckdb::Result<()> {
    register_one::<false, false>(db, c"__msduck_min_bin2_unicode")?;
    register_one::<true, false>(db, c"__msduck_max_bin2_unicode")?;
    register_one::<false, true>(db, c"__msduck_min_bin2_ansi")?;
    register_one::<true, true>(db, c"__msduck_max_bin2_ansi")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retained_budget_is_atomic_and_released_after_failure() {
        let initial = crate::unicode_carrier::CHUNK_LIMIT - 2;
        let budget = Arc::new(AtomicUsize::new(initial));
        {
            let mut state = State {
                units: None,
                budget: budget.clone(),
            };
            state.update::<false, false>(&[100]).unwrap();
            assert!(state.update::<false, false>(&[1, 2]).is_err());
            assert_eq!(state.units.as_deref(), Some([100].as_slice()));
            assert_eq!(budget.load(AtomicOrdering::Relaxed), initial + 2);
            state.update::<false, false>(&[]).unwrap();
            assert_eq!(budget.load(AtomicOrdering::Relaxed), initial);
        }
        assert_eq!(budget.load(AtomicOrdering::Relaxed), initial);
    }
}
