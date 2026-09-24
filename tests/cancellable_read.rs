use duckdb::{
    Connection,
    core::{DataChunkHandle, LogicalTypeId},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

struct CancelMarker;
impl VScalar for CancelMarker {
    type State = Arc<AtomicBool>;
    fn volatile() -> bool {
        true
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![LogicalTypeId::Bigint.into()],
            LogicalTypeId::Bigint.into(),
        )]
    }
    fn invoke(
        state: &Self::State,
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Actual native execution, not elapsed time, triggers the cancellation request.
        state.store(true, Ordering::Release);
        unsafe {
            output
                .flat_vector()
                .as_mut_slice_with_len::<i64>(input.len())
                .fill(1);
        }
        Ok(())
    }
}

#[test]
fn rust_read_adapter_preserves_results_errors_and_cancelled_transactions() {
    const CHILD: &str = "MSDUCK_CANCELLABLE_READ_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "rust_read_adapter_preserves_results_errors_and_cancelled_transactions",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success());
                return;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("read adapter probe timed out");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    let db = Connection::open_in_memory().unwrap();
    let flag = AtomicBool::new(false);
    for sql in [
        "SELECT 7::INTEGER AS n, NULL::VARCHAR AS s",
        "SELECT 7::INTEGER AS n, NULL::VARCHAR AS s WHERE false",
    ] {
        let expected: Vec<_> = db.prepare(sql).unwrap().query_arrow([]).unwrap().collect();
        let mut statement = db.prepare(sql).unwrap();
        let arrow = statement
            .query_arrow_cancellable_read([], &flag)
            .unwrap()
            .unwrap();
        assert_eq!(arrow.get_schema().fields().len(), 2);
        assert_eq!(arrow.collect::<Vec<_>>(), expected);
    }
    let mut parameterized = db.prepare("SELECT ?::INTEGER AS n").unwrap();
    flag.store(true, Ordering::Release);
    assert!(
        parameterized
            .query_arrow_cancellable_read([1], &flag)
            .unwrap()
            .is_none()
    );
    flag.store(false, Ordering::Release);
    let actual: Vec<_> = parameterized
        .query_arrow_cancellable_read([2], &flag)
        .unwrap()
        .unwrap()
        .collect();
    let expected: Vec<_> = db
        .prepare("SELECT 2::INTEGER AS n")
        .unwrap()
        .query_arrow([])
        .unwrap()
        .collect();
    assert_eq!(actual, expected);
    drop(parameterized);
    db.execute_batch("CREATE TABLE write_probe(n INTEGER)")
        .unwrap();
    let error = db
        .prepare("INSERT INTO write_probe VALUES(1)")
        .unwrap()
        .query_arrow_cancellable_read([], &flag)
        .err()
        .unwrap();
    assert!(matches!(error, duckdb::Error::InvalidQuery));
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM write_probe", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    let error = db
        .prepare("SELECT CAST(i::VARCHAR || 'x' AS INTEGER) FROM range(10000) t(i)")
        .unwrap()
        .query_arrow_cancellable_read([], &flag)
        .err()
        .unwrap();
    assert!(error.to_string().contains("Conversion Error"));

    for threads in [1, 4] {
        let db = Connection::open_in_memory().unwrap();
        let observer = db.try_clone().unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        db.register_scalar_function_with_state::<CancelMarker>("cancel_marker", &cancel)
            .unwrap();
        db.execute_batch(&format!("SET threads={threads}; CREATE TABLE preserved(n INTEGER); BEGIN; INSERT INTO preserved VALUES(1)")).unwrap();
        let mut statement = db.prepare("SELECT SUM(CAST(cancel_marker(a.i) AS DOUBLE)*b.i) FROM range(1000000) a(i) CROSS JOIN range(1000000) b(i)").unwrap();
        assert!(
            statement
                .query_arrow_cancellable_read([], &cancel)
                .unwrap()
                .is_none()
        );
        assert!(cancel.load(Ordering::Acquire));
        drop(statement);
        assert_eq!(
            db.query_row("SELECT SUM(n) FROM preserved", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            observer
                .query_row("SELECT COUNT(*) FROM preserved", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        db.execute_batch("INSERT INTO preserved VALUES(2); COMMIT")
            .unwrap();
        assert_eq!(
            observer
                .query_row("SELECT SUM(n) FROM preserved", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            3
        );
        eprintln!("Rust cancellable Arrow read preserved transaction with {threads} threads");
    }
}
