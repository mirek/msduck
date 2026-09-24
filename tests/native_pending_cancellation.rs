//! Native feasibility probe, not the server cancellation implementation.
use duckdb::ffi;
use std::ffi::{CStr, CString};
use std::ptr;
use std::time::{Duration, Instant};

struct Database(ffi::duckdb_database);
impl Database {
    fn new() -> Self {
        let mut db = Self(ptr::null_mut());
        // The C API initializes this owned output handle; Drop closes it.
        assert_eq!(unsafe { ffi::duckdb_open(ptr::null(), &mut db.0) }, 0);
        db
    }
    fn connect(&self) -> Connection<'_> {
        let mut conn = Connection(ptr::null_mut(), self);
        assert_eq!(unsafe { ffi::duckdb_connect(self.0, &mut conn.0) }, 0);
        conn
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        unsafe { ffi::duckdb_close(&mut self.0) }
    }
}
struct Connection<'a>(ffi::duckdb_connection, #[allow(dead_code)] &'a Database);
impl Drop for Connection<'_> {
    fn drop(&mut self) {
        unsafe { ffi::duckdb_disconnect(&mut self.0) }
    }
}
struct ResultSet(ffi::duckdb_result);
impl Drop for ResultSet {
    fn drop(&mut self) {
        unsafe { ffi::duckdb_destroy_result(&mut self.0) }
    }
}
impl Connection<'_> {
    fn query(&self, sql: &str) -> ResultSet {
        let sql = CString::new(sql).unwrap();
        // DuckDB permits zero-initialized result storage, including error paths.
        let mut result = ResultSet(unsafe { std::mem::zeroed() });
        let state = unsafe { ffi::duckdb_query(self.0, sql.as_ptr(), &mut result.0) };
        if state != 0 {
            let error = unsafe { ffi::duckdb_result_error(&mut result.0) };
            assert!(!error.is_null());
            panic!(
                "native query failed: {}",
                unsafe { CStr::from_ptr(error) }.to_string_lossy()
            );
        }
        result
    }
    fn scalar(&self, sql: &str) -> i64 {
        let mut result = self.query(sql);
        unsafe {
            assert_eq!(ffi::duckdb_column_count(&mut result.0), 1);
            assert_eq!(ffi::duckdb_row_count(&mut result.0), 1);
            assert!(!ffi::duckdb_value_is_null(&mut result.0, 0, 0));
            ffi::duckdb_value_int64(&mut result.0, 0, 0)
        }
    }
}
struct Pending<'a> {
    statement: ffi::duckdb_prepared_statement,
    result: ffi::duckdb_pending_result,
    _connection: &'a Connection<'a>,
}
impl<'a> Pending<'a> {
    fn read(connection: &'a Connection<'a>) -> Self {
        let mut pending = Self {
            statement: ptr::null_mut(),
            result: ptr::null_mut(),
            _connection: connection,
        };
        let sql = c"SELECT SUM(CAST(a.i AS DOUBLE) * b.i) FROM range(1000000) a(i) CROSS JOIN range(1000000) b(i)";
        unsafe {
            let state = ffi::duckdb_prepare(connection.0, sql.as_ptr(), &mut pending.statement);
            assert_eq!(state, 0, "long SELECT must prepare");
            let state = ffi::duckdb_pending_prepared(pending.statement, &mut pending.result);
            assert_eq!(state, 0, "long SELECT must start pending execution");
        }
        pending
    }
    fn execute_tasks(&self) {
        let start = Instant::now();
        let mut worked = false;
        let mut calls = 0;
        while calls < 16 || !worked {
            calls += 1;
            let state = unsafe { ffi::duckdb_pending_execute_task(self.result) };
            match state {
                ffi::duckdb_pending_state_DUCKDB_PENDING_RESULT_NOT_READY => worked = true,
                ffi::duckdb_pending_state_DUCKDB_PENDING_NO_TASKS_AVAILABLE => {
                    let progress = unsafe { ffi::duckdb_query_progress(self._connection.0) };
                    worked |= progress.rows_processed > 0;
                    std::thread::yield_now();
                }
                ffi::duckdb_pending_state_DUCKDB_PENDING_ERROR => {
                    let error = unsafe { ffi::duckdb_pending_error(self.result) };
                    assert!(!error.is_null());
                    panic!(
                        "pending execution failed: {}",
                        unsafe { CStr::from_ptr(error) }.to_string_lossy()
                    );
                }
                other => {
                    panic!("query completed before abandonment or unknown pending state: {other}")
                }
            }
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "pending task loop exceeded failure bound"
            );
        }
        assert!(worked, "must execute unfinished work before cancellation");
    }
}
impl Drop for Pending<'_> {
    fn drop(&mut self) {
        // These own distinct C API handles. Closing the pending result alone
        // does not prove executor quiescence; the next query performs cleanup.
        unsafe {
            ffi::duckdb_destroy_pending(&mut self.result);
            ffi::duckdb_destroy_prepare(&mut self.statement);
        }
    }
}

#[test]
fn abandoning_pending_read_preserves_prior_transaction_work() {
    for threads in [1, 4] {
        for ending in ["autocommit", "commit", "rollback"] {
            let db = Database::new();
            let connection = db.connect();
            let observer = db.connect();
            connection.query(&format!("SET threads={threads}"));
            connection.query("SET enable_progress_bar=true; SET enable_progress_bar_print=false; SET progress_bar_time=0");
            connection.query("CREATE TABLE preserved(n INTEGER)");
            if ending != "autocommit" {
                connection.query("BEGIN TRANSACTION");
            }
            connection.query("INSERT INTO preserved VALUES (1)");
            assert_eq!(
                observer.scalar("SELECT COUNT(*) FROM preserved"),
                i64::from(ending == "autocommit")
            );
            let pending = Pending::read(&connection);
            eprintln!("servicing pending tasks: threads={threads}, ending={ending}");
            pending.execute_tasks();
            eprintln!("abandoning pending result: threads={threads}, ending={ending}");
            let cleanup = Instant::now();
            drop(pending);
            eprintln!("starting cleanup query: threads={threads}, ending={ending}");
            // InitialCleanup of this query cancels/drains the old executor.
            // No duckdb_interrupt is issued at any point in this probe.
            assert_eq!(connection.scalar("SELECT SUM(n) FROM preserved"), 1);
            assert!(
                cleanup.elapsed() < Duration::from_secs(10),
                "cleanup exceeded failure bound"
            );
            connection.query("INSERT INTO preserved VALUES (2)");
            assert_eq!(connection.scalar("SELECT SUM(n) FROM preserved"), 3);
            assert_eq!(
                observer.scalar("SELECT COUNT(*) FROM preserved"),
                if ending == "autocommit" { 2 } else { 0 }
            );
            if ending == "commit" {
                connection.query("COMMIT");
            } else if ending == "rollback" {
                connection.query("ROLLBACK");
            }
            assert_eq!(
                observer.scalar("SELECT COUNT(*) FROM preserved"),
                if ending == "rollback" { 0 } else { 2 }
            );
            assert_eq!(connection.scalar("SELECT 42"), 42);
            eprintln!(
                "pending read cancellation: threads={threads}, ending={ending}, cleanup={:?}",
                cleanup.elapsed()
            );
        }
    }
}
