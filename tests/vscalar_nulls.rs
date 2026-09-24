//! Explicit NULL handling must work for both scalar registration APIs.
use duckdb::{
    core::{DataChunkHandle, LogicalTypeId as Id},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
struct Probe<const SPECIAL: bool, const FAIL: bool = false>;
impl<const SPECIAL: bool, const FAIL: bool> VScalar for Probe<SPECIAL, FAIL> {
    type State = Arc<AtomicUsize>;
    fn special_null_handling() -> bool {
        SPECIAL
    }
    fn volatile() -> bool {
        true
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        [Id::Integer, Id::Bigint]
            .into_iter()
            .map(|kind| ScalarFunctionSignature::exact(vec![kind.into()], Id::Integer.into()))
            .collect()
    }
    fn invoke(
        state: &Self::State,
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let source = input.flat_vector(0);
        let mut result = output.flat_vector();
        for row in 0..len {
            state.fetch_add(1, Ordering::SeqCst);
            if FAIL && source.row_is_null(row as u64) {
                return Err("NULL rejected by opt-in callback".into());
            }
            unsafe {
                result.as_mut_slice_with_len::<i32>(len)[row] =
                    if source.row_is_null(row as u64) { 7 } else { 9 };
            }
        }
        Ok(())
    }
}
#[test]
fn default_and_opt_in_null_handling_cover_every_registration_and_overload() {
    let db = duckdb::Connection::open_in_memory().unwrap();
    let observed = Arc::new(AtomicUsize::new(0));
    let skipped = Arc::new(AtomicUsize::new(0));
    db.register_scalar_function::<Probe<true>>("special")
        .unwrap();
    db.register_scalar_function_with_state::<Probe<true>>("special_state", &observed)
        .unwrap();
    db.register_scalar_function::<Probe<false>>("ordinary")
        .unwrap();
    db.register_scalar_function_with_state::<Probe<false>>("ordinary_state", &skipped)
        .unwrap();
    for kind in ["INTEGER", "BIGINT"] {
        for function in ["special", "special_state"] {
            let value: i32 = db
                .query_row(&format!("SELECT {function}(NULL::{kind})"), [], |r| {
                    r.get(0)
                })
                .unwrap();
            assert_eq!(value, 7);
        }
        for function in ["ordinary", "ordinary_state"] {
            let value: Option<i32> = db
                .query_row(&format!("SELECT {function}(NULL::{kind})"), [], |r| {
                    r.get(0)
                })
                .unwrap();
            assert_eq!(value, None);
        }
    }
    assert_eq!(observed.load(Ordering::SeqCst), 2);
    assert_eq!(skipped.load(Ordering::SeqCst), 0);
    db.register_scalar_function::<Probe<true, true>>("reject_null")
        .unwrap();
    let error = db
        .query_row("SELECT reject_null(NULL::INTEGER)", [], |r| {
            r.get::<_, i32>(0)
        })
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("NULL rejected by opt-in callback")
    );
    assert_eq!(
        db.query_row("SELECT reject_null(1::INTEGER)", [], |r| r.get::<_, i32>(0))
            .unwrap(),
        9
    );
}
