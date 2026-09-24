//! Native feasibility probe, not the server cancellation implementation.
use duckdb::ffi;
use std::ffi::{CStr, CString};
use std::ptr;
use std::time::{Duration, Instant};

unsafe extern "C" {
    fn msduck_pending_cancel_read_and_drain(result: ffi::duckdb_pending_result) -> i32;
}

#[derive(Default)]
struct FailureGate {
    entered: std::sync::atomic::AtomicBool,
    release: std::sync::atomic::AtomicBool,
}

unsafe extern "C" fn background_failure(
    info: ffi::duckdb_function_info,
    _input: ffi::duckdb_data_chunk,
    _output: ffi::duckdb_vector,
) {
    use std::sync::atomic::Ordering;
    // Extra info owns an Arc until the database drops the registered function.
    let gate =
        unsafe { &*(ffi::duckdb_scalar_function_get_extra_info(info) as *const FailureGate) };
    gate.entered.store(true, Ordering::Release);
    while !gate.release.load(Ordering::Acquire) {
        std::thread::yield_now();
    }
    unsafe {
        ffi::duckdb_scalar_function_set_error(
            info,
            c"msduck deliberate background failure".as_ptr(),
        )
    };
}

unsafe extern "C" fn drop_failure_gate(data: *mut std::ffi::c_void) {
    drop(unsafe { std::sync::Arc::from_raw(data as *const FailureGate) });
}

fn register_failure(connection: &Connection<'_>, gate: std::sync::Arc<FailureGate>) {
    unsafe {
        let mut function = ffi::duckdb_create_scalar_function();
        let mut ty = ffi::duckdb_create_logical_type(ffi::DUCKDB_TYPE_DUCKDB_TYPE_BIGINT);
        ffi::duckdb_scalar_function_set_name(function, c"msduck_gated_failure".as_ptr());
        ffi::duckdb_scalar_function_add_parameter(function, ty);
        ffi::duckdb_scalar_function_set_return_type(function, ty);
        ffi::duckdb_scalar_function_set_volatile(function);
        ffi::duckdb_scalar_function_set_extra_info(
            function,
            std::sync::Arc::into_raw(gate) as *mut _,
            Some(drop_failure_gate),
        );
        ffi::duckdb_scalar_function_set_function(function, Some(background_failure));
        let status = ffi::duckdb_register_scalar_function(connection.0, function);
        ffi::duckdb_destroy_scalar_function(&mut function);
        ffi::duckdb_destroy_logical_type(&mut ty);
        assert_eq!(status, 0);
    }
}

unsafe extern "C" fn read_marker(
    info: ffi::duckdb_function_info,
    input: ffi::duckdb_data_chunk,
    output: ffi::duckdb_vector,
) {
    let observed = unsafe {
        &*(ffi::duckdb_scalar_function_get_extra_info(info) as *const std::sync::atomic::AtomicBool)
    };
    let len = unsafe { ffi::duckdb_data_chunk_get_size(input) } as usize;
    let values = unsafe {
        std::slice::from_raw_parts_mut(ffi::duckdb_vector_get_data(output) as *mut i64, len)
    };
    values.fill(1);
    observed.store(true, std::sync::atomic::Ordering::Release);
}
unsafe extern "C" fn drop_read_marker(data: *mut std::ffi::c_void) {
    drop(unsafe { std::sync::Arc::from_raw(data as *const std::sync::atomic::AtomicBool) });
}
fn register_read_marker(
    connection: &Connection<'_>,
    observed: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    unsafe {
        let mut function = ffi::duckdb_create_scalar_function();
        let mut ty = ffi::duckdb_create_logical_type(ffi::DUCKDB_TYPE_DUCKDB_TYPE_BIGINT);
        ffi::duckdb_scalar_function_set_name(function, c"msduck_read_marker".as_ptr());
        ffi::duckdb_scalar_function_add_parameter(function, ty);
        ffi::duckdb_scalar_function_set_return_type(function, ty);
        ffi::duckdb_scalar_function_set_volatile(function);
        ffi::duckdb_scalar_function_set_extra_info(
            function,
            std::sync::Arc::into_raw(observed) as *mut _,
            Some(drop_read_marker),
        );
        ffi::duckdb_scalar_function_set_function(function, Some(read_marker));
        let status = ffi::duckdb_register_scalar_function(connection.0, function);
        ffi::duckdb_destroy_scalar_function(&mut function);
        ffi::duckdb_destroy_logical_type(&mut ty);
        assert_eq!(status, 0);
    }
}

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
    observed: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}
impl<'a> Pending<'a> {
    fn read(connection: &'a Connection<'a>) -> Self {
        let observed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        register_read_marker(connection, observed.clone());
        let mut pending = Self::sql(
            connection,
            "SELECT SUM(CAST(msduck_read_marker(a.i) AS DOUBLE) * b.i) FROM range(1000000) a(i) CROSS JOIN range(1000000) b(i)",
        );
        pending.observed = Some(observed);
        pending
    }
    fn sql(connection: &'a Connection<'a>, sql: &str) -> Self {
        let mut pending = Self {
            statement: ptr::null_mut(),
            result: ptr::null_mut(),
            _connection: connection,
            observed: None,
        };
        let sql = CString::new(sql).unwrap();
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
                ffi::duckdb_pending_state_DUCKDB_PENDING_RESULT_NOT_READY => {}
                ffi::duckdb_pending_state_DUCKDB_PENDING_NO_TASKS_AVAILABLE => {
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
            worked = self
                .observed
                .as_ref()
                .unwrap()
                .load(std::sync::atomic::Ordering::Acquire);
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
fn explicit_error_drain_preserves_prior_transaction_work() {
    const CHILD: &str = "MSDUCK_PENDING_ERROR_DRAIN_PROBE_CHILD";
    if std::env::var_os(CHILD).is_none() {
        // Native cleanup can block. Isolate the entire probe so a failure
        // cannot strand a Rust test thread or the shared build-machine lock.
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "explicit_error_drain_preserves_prior_transaction_work",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(
                    status.success(),
                    "pending cancellation child failed: {status}"
                );
                return;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!(
                    "pending cancellation probe exceeded 20 seconds; child terminated; inspect phase markers"
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
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
            // This worker has stopped calling pending_execute_task. An interrupt
            // now stops background pipelines; no subsequent task poll enters
            // ExecuteTaskInternal's transaction-invalidating error path.
            assert_eq!(
                unsafe { msduck_pending_cancel_read_and_drain(pending.result) },
                0
            );
            let error = unsafe { CStr::from_ptr(ffi::duckdb_pending_error(pending.result)) };
            assert!(
                error
                    .to_string_lossy()
                    .to_ascii_lowercase()
                    .contains("interrupt")
            );
            drop(pending);
            eprintln!("starting cleanup query: threads={threads}, ending={ending}");
            // InitialCleanup of this query cancels/drains the old executor.
            // The explicit native operation already joined workers and ended the
            // old query; this verifies the same connection remains reusable.
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
    let db = Database::new();
    let connection = db.connect();
    connection.query("SET threads=1");
    eprintln!("stale result rejection");
    let stale = Pending::sql(&connection, "SELECT 1");
    let current = Pending::sql(&connection, "SELECT 42");
    assert_eq!(
        unsafe { msduck_pending_cancel_read_and_drain(stale.result) },
        1
    );
    // Rejecting the stale result must leave the newer request executable.
    let mut current_result = ResultSet(unsafe { std::mem::zeroed() });
    assert_eq!(
        unsafe { ffi::duckdb_execute_pending(current.result, &mut current_result.0) },
        0
    );
    assert_eq!(
        unsafe { ffi::duckdb_value_int64(&mut current_result.0, 0, 0) },
        42
    );
    drop(current_result);
    drop(current);
    drop(stale);

    eprintln!("non-SELECT rejection");
    connection.query("CREATE TABLE write_probe(n INTEGER)");
    let write = Pending::sql(&connection, "INSERT INTO write_probe VALUES (9)");
    assert_eq!(
        unsafe { msduck_pending_cancel_read_and_drain(write.result) },
        2
    );
    let mut result = ResultSet(unsafe { std::mem::zeroed() });
    assert_eq!(
        unsafe { ffi::duckdb_execute_pending(write.result, &mut result.0) },
        0
    );
    drop(result);
    drop(write);
    assert_eq!(connection.scalar("SELECT n FROM write_probe"), 9);

    eprintln!("preexisting error retention");
    let failed = Pending::sql(
        &connection,
        "SELECT CAST(i::VARCHAR || 'x' AS INTEGER) FROM range(1000000) t(i)",
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let state = unsafe { ffi::duckdb_pending_execute_task(failed.result) };
        if state == ffi::duckdb_pending_state_DUCKDB_PENDING_ERROR {
            break;
        }
        assert_ne!(state, ffi::duckdb_pending_state_DUCKDB_PENDING_RESULT_READY);
        assert!(Instant::now() < deadline);
    }
    let before = unsafe { CStr::from_ptr(ffi::duckdb_pending_error(failed.result)) }.to_owned();
    assert!(before.to_string_lossy().contains("Conversion Error"));
    assert_eq!(
        unsafe { msduck_pending_cancel_read_and_drain(failed.result) },
        1
    );
    let after = unsafe { CStr::from_ptr(ffi::duckdb_pending_error(failed.result)) };
    assert_eq!(
        before.as_c_str(),
        after,
        "native error must not become cancellation"
    );
    drop(failed);
    connection.query("SET threads=4");
    eprintln!("background error retention");
    let gate = std::sync::Arc::new(FailureGate::default());
    register_failure(&connection, gate.clone());
    let racing = Pending::sql(
        &connection,
        "SELECT SUM(msduck_gated_failure(i)) FROM range(100000000) t(i)",
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    // Never poll ExecuteTask here: background execution must enter the callback.
    while !gate.entered.load(std::sync::atomic::Ordering::Acquire) {
        assert!(
            Instant::now() < deadline,
            "background callback did not start"
        );
        std::thread::yield_now();
    }
    gate.release
        .store(true, std::sync::atomic::Ordering::Release);
    assert_eq!(
        unsafe { msduck_pending_cancel_read_and_drain(racing.result) },
        1
    );
    let error = unsafe { CStr::from_ptr(ffi::duckdb_pending_error(racing.result)) };
    assert!(
        error
            .to_string_lossy()
            .contains("msduck deliberate background failure"),
        "retained error: {error:?}"
    );
    // The callback ran on a background worker and no result poll transferred
    // its error. The drain itself must retrieve it before destroying executor state.
}
