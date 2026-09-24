//! Worker-owned pending reads using the private bundled cc cancellation ABI.
use crate::{
    Error, Result,
    error::{duckdb_failure_from_message, result_from_duckdb_result},
    ffi,
};
use std::{
    ffi::CStr,
    ptr,
    sync::atomic::{AtomicBool, Ordering},
};

unsafe extern "C" {
    fn msduck_prepared_read_eligible(statement: ffi::duckdb_prepared_statement) -> bool;
    fn msduck_pending_cancel_read_and_drain(result: ffi::duckdb_pending_result) -> i32;
}

struct Pending {
    ptr: ffi::duckdb_pending_result,
    active: bool,
}
impl Pending {
    fn error(&self) -> Error {
        let message = unsafe { ffi::duckdb_pending_error(self.ptr) };
        if message.is_null() {
            duckdb_failure_from_message("pending read returned an error without diagnostics")
        } else {
            duckdb_failure_from_message(unsafe { CStr::from_ptr(message) }.to_string_lossy())
        }
    }
    fn cancel(&mut self) -> Result<Option<ffi::duckdb_result>> {
        match unsafe { msduck_pending_cancel_read_and_drain(self.ptr) } {
            0 => {
                self.active = false;
                Ok(None)
            }
            1 => {
                self.active = false;
                Err(self.error())
            }
            _ => Err(Error::InvalidQuery),
        }
    }
}
impl Drop for Pending {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe {
                if self.active {
                    msduck_pending_cancel_read_and_drain(self.ptr);
                }
                ffi::duckdb_destroy_pending(&mut self.ptr);
            }
        }
    }
}

/// The prepared statement is exclusively owned for the duration of this call.
/// The returned result transfers C API ownership to ExecutedResult.
pub(crate) unsafe fn execute(
    statement: ffi::duckdb_prepared_statement,
    cancel: &AtomicBool,
) -> Result<Option<ffi::duckdb_result>> {
    if !unsafe { msduck_prepared_read_eligible(statement) } {
        return Err(Error::InvalidQuery);
    }
    if cancel.load(Ordering::Acquire) {
        return Ok(None);
    }
    let mut pending = Pending {
        ptr: ptr::null_mut(),
        active: false,
    };
    let state = unsafe { ffi::duckdb_pending_prepared(statement, &mut pending.ptr) };
    if state != ffi::DuckDBSuccess {
        return Err(pending.error());
    }
    pending.active = true;
    loop {
        if cancel.load(Ordering::Acquire) {
            return pending.cancel();
        }
        match unsafe { ffi::duckdb_pending_execute_task(pending.ptr) } {
            ffi::duckdb_pending_state_DUCKDB_PENDING_RESULT_READY => {
                if cancel.load(Ordering::Acquire) {
                    return pending.cancel();
                }
                let mut result = unsafe { std::mem::zeroed() };
                let state = unsafe { ffi::duckdb_execute_pending(pending.ptr, &mut result) };
                pending.active = false;
                result_from_duckdb_result(state, &mut result)?;
                return Ok(Some(result));
            }
            ffi::duckdb_pending_state_DUCKDB_PENDING_RESULT_NOT_READY => {}
            ffi::duckdb_pending_state_DUCKDB_PENDING_NO_TASKS_AVAILABLE => std::thread::yield_now(),
            ffi::duckdb_pending_state_DUCKDB_PENDING_ERROR => {
                pending.active = false;
                return Err(pending.error());
            }
            _ => {
                return Err(duckdb_failure_from_message(
                    "unknown pending read execution state",
                ));
            }
        }
    }
}
