//! Worker-owned pending reads using the private bundled cc cancellation ABI.
use crate::{
    Error,
    error::{duckdb_failure_from_message, result_from_duckdb_result},
    ffi,
};
use std::{
    ffi::CStr,
    ptr,
    sync::atomic::{AtomicBool, Ordering},
};

/// A query error may follow the ordinary SQL error path. A failed cancellation
/// drain has no established reuse guarantee, so its connection must be closed.
#[derive(Debug)]
pub enum CancellableReadError {
    Query(Error),
    ConnectionUnusable(Error),
}
impl std::fmt::Display for CancellableReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Query(error) | Self::ConnectionUnusable(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for CancellableReadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Query(error) | Self::ConnectionUnusable(error) => Some(error),
        }
    }
}
impl From<Error> for CancellableReadError {
    fn from(error: Error) -> Self {
        Self::Query(error)
    }
}
type Result<T> = std::result::Result<T, CancellableReadError>;

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
                Err(CancellableReadError::ConnectionUnusable(self.error()))
            }
            _ => Err(CancellableReadError::ConnectionUnusable(
                Error::InvalidQuery,
            )),
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
        return Err(Error::InvalidQuery.into());
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
        return Err(pending.error().into());
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
                return Err(pending.error().into());
            }
            _ => {
                return Err(CancellableReadError::ConnectionUnusable(
                    duckdb_failure_from_message("unknown pending read execution state"),
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_native_pending_result_requires_connection_disposal() {
        // Exercise a real native drain failure: replacing an active result makes
        // its old pending handle stale. The cancellation API must preserve the
        // native diagnostic and never report successful cancellation/reuse.
        unsafe {
            let mut database = ptr::null_mut();
            let mut connection = ptr::null_mut();
            let mut statement = ptr::null_mut();
            assert_eq!(
                ffi::duckdb_open(ptr::null(), &mut database),
                ffi::DuckDBSuccess
            );
            assert_eq!(
                ffi::duckdb_connect(database, &mut connection),
                ffi::DuckDBSuccess
            );
            assert_eq!(
                ffi::duckdb_prepare(connection, c"SELECT 1".as_ptr(), &mut statement),
                ffi::DuckDBSuccess
            );
            let mut pending = Pending {
                ptr: ptr::null_mut(),
                active: false,
            };
            assert_eq!(
                ffi::duckdb_pending_prepared(statement, &mut pending.ptr),
                ffi::DuckDBSuccess
            );
            pending.active = true;
            let mut replacement = std::mem::zeroed();
            assert_eq!(
                ffi::duckdb_query(connection, c"SELECT 42".as_ptr(), &mut replacement),
                ffi::DuckDBSuccess
            );
            let error = pending.cancel().unwrap_err();
            assert!(matches!(error, CancellableReadError::ConnectionUnusable(_)));
            assert!(!error.to_string().is_empty());
            assert_eq!(ffi::duckdb_value_int64(&mut replacement, 0, 0), 42);
            drop(pending);
            ffi::duckdb_destroy_result(&mut replacement);
            ffi::duckdb_destroy_prepare(&mut statement);
            ffi::duckdb_disconnect(&mut connection);
            ffi::duckdb_close(&mut database);
        }
    }
}
