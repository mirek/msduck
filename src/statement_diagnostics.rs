//! Explicit execution diagnostics shared by a database's registered callbacks.
//! A scope belongs to one statement, not to a catalog function or worker thread.
use duckdb::{
    Connection,
    core::{DataChunkHandle, LogicalTypeId},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use ring::rand::{SecureRandom, SystemRandom};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

const LIMIT: usize = 4096;
type Ticket = [u8; 16];
type Entries = HashMap<Ticket, Arc<AtomicBool>>;

#[derive(Clone, Default)]
pub struct Registry(Arc<Mutex<Entries>>);

pub struct Scope {
    registry: Registry,
    ticket: Ticket,
    eliminated: Arc<AtomicBool>,
}

impl Registry {
    pub fn begin(&self) -> Result<Scope, &'static str> {
        let mut entries = self.0.lock().map_err(|_| "diagnostic registry poisoned")?;
        if entries.len() >= LIMIT {
            return Err("too many active statement diagnostic contexts");
        }
        let mut ticket = [0; 16];
        SystemRandom::new()
            .fill(&mut ticket)
            .map_err(|_| "diagnostic ticket generation failed")?;
        // A collision must not replace a live statement or trigger an unbounded
        // retry loop. No caller-controlled or process-global counter is used.
        if entries.contains_key(&ticket) {
            return Err("diagnostic ticket collision");
        }
        let eliminated = Arc::new(AtomicBool::new(false));
        entries.insert(ticket, eliminated.clone());
        Ok(Scope {
            registry: self.clone(),
            ticket,
            eliminated,
        })
    }

    fn find(&self, ticket: &Ticket) -> Result<Arc<AtomicBool>, &'static str> {
        self.0
            .lock()
            .map_err(|_| "diagnostic registry poisoned")?
            .get(ticket)
            .cloned()
            .ok_or("unknown statement diagnostic context")
    }

    /// Registration alone does not rewrite SQL or emit warnings. The caller
    /// must retain this registry and open a fresh scope for every execution.
    pub fn register(&self, db: &Connection) -> duckdb::Result<()> {
        count_frame::register(db)?;
        integer_frame::register(db)?;
        db.register_scalar_function_with_state::<Observe>("__msduck_observe_null", self)
    }
}

impl Scope {
    pub fn ticket(&self) -> &[u8; 16] {
        &self.ticket
    }
    pub fn null_eliminated(&self) -> bool {
        self.eliminated.load(Ordering::Acquire)
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        // Cleanup must not panic while unwinding an execution error. A poisoned
        // registry still refuses future use through begin/find.
        self.registry
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&self.ticket);
    }
}

struct Observe;
impl VScalar for Observe {
    type State = Registry;
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![LogicalTypeId::Blob.into(), LogicalTypeId::Boolean.into()],
            LogicalTypeId::Boolean.into(),
        )]
    }
    fn special_null_handling() -> bool {
        true
    }
    fn volatile() -> bool {
        true
    }
    fn invoke(
        registry: &Registry,
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let tickets = input.flat_vector(0);
        let flags = input.flat_vector(1);
        let mut result = output.flat_vector();
        let mut cached: Option<(Ticket, Arc<AtomicBool>)> = None;
        for row in 0..input.len() {
            if tickets.row_is_null(row as u64) {
                return Err("NULL diagnostic ticket".into());
            }
            // The exact BLOB signature uses duckdb_string_t. Validate the
            // length before accessing its payload and retain no native pointer.
            let mut value = unsafe {
                tickets.as_slice_with_len::<duckdb::ffi::duckdb_string_t>(input.len())[row]
            };
            if unsafe { duckdb::ffi::duckdb_string_t_length(value) } != 16 {
                return Err("invalid diagnostic ticket length".into());
            }
            let ticket: Ticket = unsafe {
                std::slice::from_raw_parts(
                    duckdb::ffi::duckdb_string_t_data(&mut value).cast::<u8>(),
                    16,
                )
            }
            .try_into()
            .expect("checked length");
            if cached
                .as_ref()
                .is_none_or(|(previous, _)| *previous != ticket)
            {
                cached = Some((ticket, registry.find(&ticket)?));
            }
            if flags.row_is_null(row as u64) {
                result.set_null(row);
            } else {
                let flag = unsafe { flags.as_slice_with_len::<bool>(input.len())[row] };
                if flag {
                    cached
                        .as_ref()
                        .expect("resolved ticket")
                        .1
                        .store(true, Ordering::Release);
                }
                // Exact BOOLEAN output, with one output slot per input row.
                unsafe {
                    result.as_mut_slice::<bool>()[row] = flag;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_identity_lambda_preserves_types_and_evaluates_its_operand_once() {
        let db = Connection::open_in_memory().unwrap();
        let registry = Registry::default();
        registry.register(&db).unwrap();
        let wrap = |value: &str| {
            format!(
                "list_extract(list_transform([{value}], diagnostic_value -> CASE WHEN __msduck_observe_null(?, diagnostic_value IS NULL) THEN NULL ELSE diagnostic_value END),1)"
            )
        };
        db.execute_batch("CREATE SEQUENCE lambda_calls").unwrap();
        let scope = registry.begin().unwrap();
        let sql = format!(
            "SELECT sum({}) FROM range(6000) d(i)",
            wrap("CASE WHEN nextval('lambda_calls')%17=0 THEN NULL ELSE i END")
        );
        let total: i64 = db
            .query_row(&sql, [scope.ticket().as_slice()], |r| r.get(0))
            .unwrap();
        assert_eq!(total, (0..6000).filter(|n| (n + 1) % 17 != 0).sum::<i64>());
        assert!(scope.null_eliminated());
        assert_eq!(
            db.query_row("SELECT currval('lambda_calls')", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            6000
        );
        for (value, null) in [
            ("1::TINYINT", false),
            ("NULL::INTEGER", true),
            ("123.456::DECIMAL(38,10)", false),
            ("'🦆'::VARCHAR", false),
            ("from_hex('0080ff')", false),
            ("'12:34:56.1234567'::TIME_NS", false),
            ("struct_pack(__msduck_utf16le := from_hex('3ed8'))", false),
            ("NULL::STRUCT(__msduck_utf16le BLOB)", true),
        ] {
            let scope = registry.begin().unwrap();
            let sql = format!(
                "SELECT typeof(actual)=typeof(expected), actual IS NOT DISTINCT FROM expected FROM (SELECT {} AS actual,{value} AS expected)",
                wrap(value)
            );
            let result: (bool, bool) = db
                .query_row(&sql, [scope.ticket().as_slice()], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .unwrap();
            assert_eq!(result, (true, true), "{value}");
            assert_eq!(scope.null_eliminated(), null, "{value}");
        }
    }

    #[test]
    fn frame_observation_ignores_unconsumed_nulls_and_evaluates_inputs_once() {
        let db = Connection::open_in_memory().unwrap();
        let registry = Registry::default();
        registry.register(&db).unwrap();
        let instrument = |sql: &str| {
            let mut statement =
                sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::DuckDbDialect {}, sql)
                    .unwrap()
                    .remove(0);
            let ticket =
                sqlparser::ast::Expr::Value(sqlparser::ast::Value::Placeholder("$1".into()).into());
            msduck_sql::aggregate_diagnostics::instrument(&mut statement, &ticket, |name| {
                matches!(name, "min" | "count")
            });
            statement.to_string()
        };
        for (frame, source, expected, warned) in [
            (
                "1 FOLLOWING AND 1 FOLLOWING",
                "(VALUES(1,NULL::INTEGER),(2,2))",
                vec![Some(2), None],
                false,
            ),
            (
                "1 FOLLOWING AND 1 FOLLOWING",
                "(VALUES(1,NULL::INTEGER))",
                vec![None],
                false,
            ),
            (
                "1 PRECEDING AND 1 PRECEDING",
                "(VALUES(1,NULL::INTEGER),(2,2))",
                vec![None, None],
                true,
            ),
        ] {
            let scope = registry.begin().unwrap();
            let sql = format!(
                "SELECT MIN(v) OVER (ORDER BY id ROWS BETWEEN {frame}) FROM {source} d(id,v) ORDER BY id"
            );
            let mut statement = db.prepare(&instrument(&sql)).unwrap();
            let actual = statement
                .query_map([scope.ticket().as_slice()], |r| r.get::<_, Option<i32>>(0))
                .unwrap()
                .collect::<duckdb::Result<Vec<_>>>()
                .unwrap();
            assert_eq!(actual, expected, "{sql}");
            assert_eq!(scope.null_eliminated(), warned, "{sql}");
            let count_scope = registry.begin().unwrap();
            let count_sql = instrument(&sql.replace("MIN(v)", "COUNT(v)"));
            let counts = db
                .prepare(&count_sql)
                .unwrap()
                .query_map([count_scope.ticket().as_slice()], |r| r.get::<_, i64>(0))
                .unwrap()
                .collect::<duckdb::Result<Vec<_>>>()
                .unwrap();
            assert_eq!(
                counts,
                expected
                    .iter()
                    .map(|v| i64::from(v.is_some()))
                    .collect::<Vec<_>>()
            );
            assert_eq!(count_scope.null_eliminated(), warned, "{count_sql}");
        }
        db.execute_batch("CREATE SEQUENCE frame_operand_calls")
            .unwrap();
        let scope = registry.begin().unwrap();
        let count: i64 = db.query_row(
            &instrument("SELECT COUNT(*) FROM (SELECT MIN(nextval('frame_operand_calls')) OVER (ORDER BY i ROWS BETWEEN 2 PRECEDING AND 2 FOLLOWING) AS v FROM range(6000) d(i)) WHERE v IS NOT NULL"),
            [scope.ticket().as_slice()], |r| r.get(0),
        ).unwrap();
        assert_eq!(count, 6000);
        assert_eq!(
            db.query_row("SELECT currval('frame_operand_calls')", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            6000
        );
        assert!(!scope.null_eliminated());
        db.execute_batch("CREATE SEQUENCE count_generated_calls")
            .unwrap();
        let count_scope = registry.begin().unwrap();
        let wrong: i64 = db.query_row(
            &instrument("SELECT COUNT(*) FROM (SELECT i,COUNT(nextval('count_generated_calls')) OVER(ORDER BY i ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) AS v FROM range(6000) d(i)) WHERE v<>i+1"),
            [count_scope.ticket().as_slice()], |r| r.get(0),
        ).unwrap();
        assert_eq!(wrong, 0);
        assert!(!count_scope.null_eliminated());
        assert_eq!(
            db.query_row("SELECT currval('count_generated_calls')", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            6000
        );
        for value in [
            "1::TINYINT",
            "NULL::INTEGER",
            "123.456::DECIMAL(38,10)",
            "'🦆'::VARCHAR",
            "from_hex('0080ff')",
            "'12:34:56.1234567'::TIME_NS",
            "struct_pack(__msduck_utf16le := from_hex('3ed8'))",
            "NULL::STRUCT(__msduck_utf16le BLOB)",
        ] {
            let scope = registry.begin().unwrap();
            let actual = instrument(&format!("SELECT MIN({value}) OVER () AS actual"));
            let sql = format!(
                "SELECT typeof(actual)=typeof(expected),actual IS NOT DISTINCT FROM expected FROM ({actual}) a CROSS JOIN (SELECT MIN({value}) OVER () AS expected) e"
            );
            let result: (bool, bool) = db
                .query_row(&sql, [scope.ticket().as_slice()], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .unwrap();
            assert_eq!(result, (true, true), "{value}");
        }
    }

    #[test]
    fn scopes_are_bounded_isolated_and_released_during_unwind() {
        let registry = Registry::default();
        let scopes: Vec<_> = (0..LIMIT).map(|_| registry.begin().unwrap()).collect();
        assert!(registry.begin().is_err());
        registry
            .find(scopes[0].ticket())
            .unwrap()
            .store(true, Ordering::Relaxed);
        assert!(scopes[0].null_eliminated());
        assert!(!scopes[1].null_eliminated());
        let stale = *scopes[0].ticket();
        drop(scopes);
        assert!(registry.find(&stale).is_err());
        let caught = std::panic::catch_unwind(|| {
            let _scope = registry.begin().unwrap();
            panic!("execution failed");
        });
        assert!(caught.is_err());
        assert!(registry.0.lock().unwrap().is_empty());
        assert!(!registry.begin().unwrap().null_eliminated());
    }

    #[test]
    fn native_observation_survives_having_and_preserves_single_evaluation() {
        let db = Connection::open_in_memory().unwrap();
        let registry = Registry::default();
        registry.register(&db).unwrap();
        db.execute_batch("CREATE SEQUENCE diagnostic_calls")
            .unwrap();
        let scope = registry.begin().unwrap();
        let total: i64 = db.query_row(
            "WITH input AS MATERIALIZED (SELECT nextval('diagnostic_calls') AS n FROM range(6000)), values_once AS MATERIALIZED (SELECT CASE WHEN n%17=0 THEN NULL ELSE n END AS v FROM input) SELECT SUM(CASE WHEN __msduck_observe_null(?,v IS NULL) THEN NULL ELSE v END) FROM values_once",
            [scope.ticket().as_slice()], |r| r.get(0),
        ).unwrap();
        assert_eq!(total, (1..=6000).filter(|n| n % 17 != 0).sum::<i64>());
        assert!(scope.null_eliminated());
        assert_eq!(
            db.query_row("SELECT currval('diagnostic_calls')", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            6000
        );
        for (source, suffix, expected) in [
            ("(VALUES(1),(NULL))", "HAVING COUNT(*)>10", true),
            ("(VALUES(CAST(NULL AS INT)),(NULL))", "", true),
            ("(VALUES(1),(NULL))", "WHERE 1=0", false),
            ("(VALUES(1),(2))", "", false),
        ] {
            let scope = registry.begin().unwrap();
            let sql = format!(
                "SELECT MIN(CASE WHEN __msduck_observe_null(?,v IS NULL) THEN NULL ELSE v END) FROM {source} d(v) {suffix}"
            );
            let mut statement = db.prepare(&sql).unwrap();
            let mut rows = statement.query([scope.ticket().as_slice()]).unwrap();
            while rows.next().unwrap().is_some() {}
            assert_eq!(scope.null_eliminated(), expected, "{sql}");
        }
    }

    #[test]
    fn shared_catalog_callbacks_keep_concurrent_statements_separate() {
        let db = Connection::open_in_memory().unwrap();
        let registry = Registry::default();
        registry.register(&db).unwrap();
        let mut workers = Vec::new();
        for nullable in [false, true, false, true] {
            let db = db.try_clone().unwrap();
            let registry = registry.clone();
            workers.push(std::thread::spawn(move || {
                let scope = registry.begin().unwrap();
                let count: i64 = db.query_row(
                    "SELECT count(*) FROM (SELECT MAX(CASE WHEN __msduck_observe_null(?, ? AND i%17=0) THEN NULL ELSE i END) OVER (ORDER BY i ROWS BETWEEN 1 PRECEDING AND CURRENT ROW) AS v FROM range(6000) d(i)) WHERE v IS NOT NULL",
                    duckdb::params![scope.ticket().as_slice(), nullable], |r| r.get(0),
                ).unwrap();
                assert_eq!(count, if nullable {5999} else {6000});
                assert_eq!(scope.null_eliminated(), nullable);
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }
        assert!(registry.0.lock().unwrap().is_empty());
        let scope = registry.begin().unwrap();
        let ticket = *scope.ticket();
        drop(scope);
        for bad in [ticket.to_vec(), vec![], vec![0; 17]] {
            assert!(
                db.query_row("SELECT __msduck_observe_null(?,false)", [bad], |r| r
                    .get::<_, bool>(0))
                    .is_err()
            );
        }
        let scope = registry.begin().unwrap();
        assert_eq!(
            db.query_row(
                "SELECT __msduck_observe_null(?,NULL::BOOLEAN)",
                [scope.ticket().as_slice()],
                |r| r.get::<_, Option<bool>>(0)
            )
            .unwrap(),
            None
        );
        assert!(!scope.null_eliminated());
    }

    #[test]
    fn preparation_does_not_observe_and_each_execution_uses_its_own_ticket() {
        let db = Connection::open_in_memory().unwrap();
        let registry = Registry::default();
        registry.register(&db).unwrap();
        let first = registry.begin().unwrap();
        let mut prepared = db.prepare("SELECT __msduck_observe_null(?,true)").unwrap();
        assert!(!first.null_eliminated());
        assert!(
            prepared
                .query_row([first.ticket().as_slice()], |r| r.get::<_, bool>(0))
                .unwrap()
        );
        assert!(first.null_eliminated());
        let ticket = *first.ticket();
        drop(first);
        assert!(
            prepared
                .query_row([ticket.as_slice()], |r| r.get::<_, bool>(0))
                .is_err()
        );
        let next = registry.begin().unwrap();
        assert!(!next.null_eliminated());
        assert!(
            prepared
                .query_row([next.ticket().as_slice()], |r| r.get::<_, bool>(0))
                .unwrap()
        );
        assert!(next.null_eliminated());
        assert!(
            db.query_row("SELECT __msduck_observe_null(NULL,true)", [], |r| r
                .get::<_, bool>(0))
                .is_err()
        );
    }
}

// Return NULL presence as data, keeping observation out of intermediate
// window segment states. State size is independent of the frame width.
mod count_frame {
    use duckdb::{Connection, ffi::*};

    #[derive(Clone, Copy, Default)]
    struct State {
        count: u64,
        eliminated: bool,
        overflow: bool,
    }
    impl State {
        fn combine(&mut self, other: Self) {
            self.eliminated |= other.eliminated;
            self.overflow |= other.overflow;
            match self.count.checked_add(other.count) {
                Some(count) => self.count = count,
                None => self.overflow = true,
            }
        }
    }
    unsafe extern "C" fn size(_: duckdb_function_info) -> idx_t {
        std::mem::size_of::<State>() as idx_t
    }
    unsafe extern "C" fn init(_: duckdb_function_info, state: duckdb_aggregate_state) {
        unsafe { state.cast::<State>().write(State::default()) }
    }
    unsafe extern "C" fn update(
        _: duckdb_function_info,
        input: duckdb_data_chunk,
        states: *mut duckdb_aggregate_state,
    ) {
        unsafe {
            let len = duckdb_data_chunk_get_size(input);
            let values = duckdb_data_chunk_get_vector(input, 0);
            let validity = duckdb_vector_get_validity(values);
            for row in 0..len as usize {
                let is_null =
                    !validity.is_null() && !duckdb_validity_row_is_valid(validity, row as idx_t);
                // ANY input: inspect validity only, never interpret its payload.
                (*(*states.add(row)).cast::<State>()).combine(State {
                    count: u64::from(!is_null),
                    eliminated: is_null,
                    overflow: false,
                });
            }
        }
    }
    unsafe extern "C" fn combine(
        _: duckdb_function_info,
        sources: *mut duckdb_aggregate_state,
        targets: *mut duckdb_aggregate_state,
        count: idx_t,
    ) {
        unsafe {
            for row in 0..count as usize {
                let source = (*sources.add(row)).cast::<State>().read();
                (*(*targets.add(row)).cast::<State>()).combine(source);
            }
        }
    }
    unsafe extern "C" fn finalize(
        info: duckdb_function_info,
        states: *mut duckdb_aggregate_state,
        output: duckdb_vector,
        count: idx_t,
        offset: idx_t,
    ) {
        unsafe {
            let values = duckdb_struct_vector_get_child(output, 0);
            let flags = duckdb_struct_vector_get_child(output, 1);
            for vector in [output, values, flags] {
                duckdb_vector_ensure_validity_writable(vector);
            }
            for index in 0..count as usize {
                let state = (*states.add(index)).cast::<State>().read();
                if state.overflow || state.count > i64::MAX as u64 {
                    duckdb_aggregate_function_set_error(
                        info,
                        c"Arithmetic overflow error converting expression to data type bigint."
                            .as_ptr(),
                    );
                    return;
                }
                let row = offset as usize + index;
                for vector in [output, values, flags] {
                    duckdb_validity_set_row_valid(duckdb_vector_get_validity(vector), row as idx_t);
                }
                duckdb_vector_get_data(values)
                    .cast::<i64>()
                    .add(row)
                    .write(state.count as i64);
                duckdb_vector_get_data(flags)
                    .cast::<bool>()
                    .add(row)
                    .write(state.eliminated);
            }
        }
    }
    pub(super) fn register(db: &Connection) -> duckdb::Result<()> {
        unsafe {
            let mut function = duckdb_create_aggregate_function();
            let mut argument = duckdb_create_logical_type(DUCKDB_TYPE_DUCKDB_TYPE_ANY);
            let mut children = [
                duckdb_create_logical_type(DUCKDB_TYPE_DUCKDB_TYPE_BIGINT),
                duckdb_create_logical_type(DUCKDB_TYPE_DUCKDB_TYPE_BOOLEAN),
            ];
            let mut result = std::ptr::null_mut();
            let registered = if function.is_null()
                || argument.is_null()
                || children.iter().any(|child| child.is_null())
            {
                Err(duckdb::Error::InvalidQuery)
            } else {
                let mut names = [c"value".as_ptr(), c"eliminated".as_ptr()];
                result = duckdb_create_struct_type(children.as_mut_ptr(), names.as_mut_ptr(), 2);
                if result.is_null() {
                    Err(duckdb::Error::InvalidQuery)
                } else {
                    duckdb_aggregate_function_set_name(function, c"__msduck_count_frame".as_ptr());
                    duckdb_aggregate_function_add_parameter(function, argument);
                    duckdb_aggregate_function_set_return_type(function, result);
                    duckdb_aggregate_function_set_special_handling(function);
                    duckdb_aggregate_function_set_functions(
                        function,
                        Some(size),
                        Some(init),
                        Some(update),
                        Some(combine),
                        Some(finalize),
                    );
                    db.register_aggregate_function(function)
                }
            };
            for child in &mut children {
                if !child.is_null() {
                    duckdb_destroy_logical_type(child);
                }
            }
            if !result.is_null() {
                duckdb_destroy_logical_type(&mut result);
            }
            if !argument.is_null() {
                duckdb_destroy_logical_type(&mut argument);
            }
            if !function.is_null() {
                duckdb_destroy_aggregate_function(&mut function);
            }
            registered
        }
    }

    #[test]
    fn bounded_count_pair_retains_null_presence_for_actual_frames() {
        let db = Connection::open_in_memory().unwrap();
        register(&db).unwrap();
        for (source, frame, expected) in [
            (
                "(VALUES(1,NULL::INT),(2,2))",
                "ROWS BETWEEN 1 FOLLOWING AND 1 FOLLOWING",
                vec![(1, false), (0, false)],
            ),
            (
                "(VALUES(1,NULL::INT),(2,2))",
                "ROWS BETWEEN 1 PRECEDING AND 1 PRECEDING",
                vec![(0, false), (0, true)],
            ),
            (
                "(VALUES(1,NULL::INT),(2,NULL::INT))",
                "ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING",
                vec![(0, true), (0, true)],
            ),
        ] {
            let sql = format!(
                "SELECT coalesce(d.value,0),coalesce(d.eliminated,false) FROM (SELECT id,__msduck_count_frame(v) OVER(ORDER BY id {frame}) AS d FROM {source} s(id,v)) ORDER BY id"
            );
            let mut stmt = db.prepare(&sql).unwrap();
            let actual = stmt
                .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, bool>(1)?)))
                .unwrap()
                .collect::<duckdb::Result<Vec<_>>>()
                .unwrap();
            assert_eq!(actual, expected, "{sql}");
        }
        for kind in [
            "INT",
            "DECIMAL(38,10)",
            "VARCHAR",
            "BLOB",
            "TIME_NS",
            "STRUCT(__msduck_utf16le BLOB)",
        ] {
            let sql = format!(
                "SELECT d.value,d.eliminated FROM (SELECT __msduck_count_frame(v) d FROM (SELECT NULL::{kind} v) s)"
            );
            let actual = db
                .query_row(&sql, [], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, bool>(1)?))
                })
                .unwrap();
            assert_eq!(actual, (0, true), "{kind}");
        }
        db.execute_batch("CREATE SEQUENCE count_frame_calls")
            .unwrap();
        let wrong: i64 = db.query_row("SELECT count(*) FROM (SELECT __msduck_count_frame(CASE WHEN nextval('count_frame_calls')%17=0 THEN NULL ELSE i END) OVER() d FROM range(6000) r(i)) WHERE d.value<>5648 OR NOT d.eliminated",[],|r|r.get(0)).unwrap();
        assert_eq!(wrong, 0);
        assert_eq!(
            db.query_row("SELECT currval('count_frame_calls')", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            6000
        );
        let wrong: i64 = db.query_row("SELECT count(*) FROM (SELECT i,__msduck_count_frame(CASE WHEN i%17=0 THEN NULL ELSE i END) OVER(ORDER BY i ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) d FROM range(100000) r(i)) WHERE NOT d.eliminated OR d.value<>i-i//17",[],|r|r.get(0)).unwrap();
        assert_eq!(wrong, 0);
        assert!(std::mem::size_of::<State>() <= 24);
    }
}

// Carry diagnostics as data until the actual window result is observed.
mod integer_frame {
    use duckdb::{Connection, ffi::*};
    use std::{
        ffi::CStr,
        panic::{AssertUnwindSafe, catch_unwind},
    };

    use msduck_core::bounded_aggregate::{State as ValueState, add};

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct State {
        value: ValueState,
        eliminated: bool,
    }

    fn protect(info: duckdb_function_info, action: impl FnOnce()) {
        if catch_unwind(AssertUnwindSafe(action)).is_err() {
            unsafe {
                duckdb_aggregate_function_set_error(
                    info,
                    c"Rust aggregate callback panicked".as_ptr(),
                );
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
                    (*(*states.add(index)).cast::<State>()).eliminated = true;
                    continue;
                }
                let value = if MONEY {
                    let raw = *data.cast::<duckdb_hugeint>().add(index);
                    let scaled = (i128::from(raw.upper) << 64) | i128::from(raw.lower);
                    let Ok(value) = i64::try_from(scaled) else {
                        (*(*states.add(index)).cast::<State>()).value.failed = true;
                        continue;
                    };
                    value
                } else if BIG {
                    *data.cast::<i64>().add(index)
                } else {
                    i64::from(*data.cast::<i32>().add(index))
                };
                let state = &mut *(*states.add(index)).cast::<State>();
                if let Some(next) = add::<BIG>(state.value, value, 1) {
                    state.value = next;
                } else {
                    state.value.failed = true;
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
                let target = &mut *(*target.add(index)).cast::<State>();
                target.eliminated |= source.eliminated;
                if let Some(next) = (!source.value.failed)
                    .then(|| add::<BIG>(target.value, source.value.sum, source.value.count))
                    .flatten()
                {
                    target.value = next;
                } else {
                    target.value.failed = true;
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
            let values = duckdb_struct_vector_get_child(output, 0);
            let flags = duckdb_struct_vector_get_child(output, 1);
            for vector in [output, values, flags] {
                duckdb_vector_ensure_validity_writable(vector);
            }
            let validity = duckdb_vector_get_validity(values);
            let data = duckdb_vector_get_data(values);
            for index in 0..count as usize {
                let state = (*states.add(index)).cast::<State>().read();
                // Window segment trees can build states that no output frame uses.
                // Keep overflow sticky in each state, but report only finalized ones.
                let row = offset as usize + index;
                duckdb_validity_set_row_valid(duckdb_vector_get_validity(output), row as idx_t);
                duckdb_validity_set_row_valid(duckdb_vector_get_validity(flags), row as idx_t);
                duckdb_vector_get_data(flags)
                    .cast::<bool>()
                    .add(row)
                    .write(state.eliminated);
                let value = match state.value.value(AVG) {
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
            let mut flag = duckdb_create_logical_type(DUCKDB_TYPE_DUCKDB_TYPE_BOOLEAN);
            let mut output = std::ptr::null_mut();
            let result = if function.is_null() || kind.is_null() || flag.is_null() {
                Err(duckdb::Error::InvalidQuery)
            } else {
                let mut children = [kind, flag];
                let mut names = [c"value".as_ptr(), c"eliminated".as_ptr()];
                output = duckdb_create_struct_type(children.as_mut_ptr(), names.as_mut_ptr(), 2);
                if output.is_null() {
                    Err(duckdb::Error::InvalidQuery)
                } else {
                    duckdb_aggregate_function_set_name(function, name.as_ptr());
                    duckdb_aggregate_function_add_parameter(function, kind);
                    duckdb_aggregate_function_set_return_type(function, output);
                    duckdb_aggregate_function_set_special_handling(function);
                    duckdb_aggregate_function_set_functions(
                        function,
                        Some(state_size),
                        Some(init),
                        Some(update::<BIG, MONEY>),
                        Some(combine::<BIG>),
                        Some(finalize::<BIG, AVG, MONEY>),
                    );
                    db.register_aggregate_function(function)
                }
            };
            for logical in [&mut output, &mut flag, &mut kind] {
                if !logical.is_null() {
                    duckdb_destroy_logical_type(logical);
                }
            }
            if !function.is_null() {
                duckdb_destroy_aggregate_function(&mut function);
            }
            result
        }
    }
    pub(super) fn register(db: &Connection) -> duckdb::Result<()> {
        register_one::<false, false, false>(db, c"__msduck_sum_int_frame")?;
        register_one::<true, false, false>(db, c"__msduck_sum_big_frame")?;
        register_one::<false, true, false>(db, c"__msduck_avg_int_frame")?;
        register_one::<true, true, false>(db, c"__msduck_avg_big_frame")?;
        register_one::<true, false, true>(db, c"__msduck_sum_money_frame")?;
        register_one::<true, true, true>(db, c"__msduck_avg_money_frame")
    }

    #[test]
    fn generated_integer_windows_observe_only_consumed_frames() {
        let db = Connection::open_in_memory().unwrap();
        crate::integer_aggregate::register(&db).unwrap();
        let registry = super::Registry::default();
        registry.register(&db).unwrap();
        let instrument = |sql: &str| {
            let mut statement =
                sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::DuckDbDialect {}, sql)
                    .unwrap()
                    .remove(0);
            let ticket =
                sqlparser::ast::Expr::Value(sqlparser::ast::Value::Placeholder("$1".into()).into());
            assert_eq!(
                crate::aggregate_diagnostics::instrument(&mut statement, ticket),
                1
            );
            statement.to_string()
        };
        for (family, kind, two) in [
            ("int", "INT", "2"),
            ("big", "BIGINT", "2"),
            ("money", "DECIMAL(19,4)", "2.0000"),
        ] {
            for aggregate in ["sum", "avg"] {
                for (source, frame, expected, warned) in [
                    (
                        format!("(VALUES(1,NULL::{kind}),(2,2::{kind}))"),
                        "1 FOLLOWING AND 1 FOLLOWING",
                        vec![Some(two.to_owned()), None],
                        false,
                    ),
                    (
                        format!("(VALUES(1,NULL::{kind}),(2,2::{kind}))"),
                        "1 PRECEDING AND 1 PRECEDING",
                        vec![None, None],
                        true,
                    ),
                    (
                        format!("(VALUES(1,NULL::{kind}))"),
                        "1 FOLLOWING AND 1 FOLLOWING",
                        vec![None],
                        false,
                    ),
                ] {
                    let scope = registry.begin().unwrap();
                    let sql = instrument(&format!(
                        "SELECT CAST(__msduck_{aggregate}_{family}(v) OVER(ORDER BY id ROWS BETWEEN {frame}) AS VARCHAR) FROM {source} s(id,v) ORDER BY id"
                    ));
                    let mut statement = db.prepare(&sql).unwrap();
                    let actual = statement
                        .query_map([scope.ticket().as_slice()], |r| {
                            r.get::<_, Option<String>>(0)
                        })
                        .unwrap()
                        .collect::<duckdb::Result<Vec<_>>>()
                        .unwrap();
                    assert_eq!(actual, expected, "{sql}");
                    assert_eq!(scope.null_eliminated(), warned, "{sql}");
                }
                db.execute_batch("CREATE OR REPLACE SEQUENCE observed_frame_calls")
                    .unwrap();
                let scope = registry.begin().unwrap();
                let query = instrument(&format!(
                    "SELECT __msduck_{aggregate}_{family}(CAST(CASE WHEN nextval('observed_frame_calls')%17=0 THEN NULL ELSE 1 END AS {kind})) OVER() AS v FROM range(6000)"
                ));
                let expected = if aggregate == "sum" { 5648 } else { 1 };
                let wrong:i64=db.query_row(&format!("SELECT count(*) FROM ({query}) WHERE v IS DISTINCT FROM CAST({expected} AS {kind})"),[scope.ticket().as_slice()],|r|r.get(0)).unwrap();
                assert_eq!(wrong, 0, "{query}");
                assert!(scope.null_eliminated());
                assert_eq!(
                    db.query_row("SELECT currval('observed_frame_calls')", [], |r| r
                        .get::<_, i64>(0))
                        .unwrap(),
                    6000
                );
            }
        }
    }

    #[test]
    fn paired_integer_frames_preserve_values_types_and_null_presence() {
        let db = Connection::open_in_memory().unwrap();
        crate::integer_aggregate::register(&db).unwrap();
        register(&db).unwrap();
        for (family, kind) in [
            ("int", "INT"),
            ("big", "BIGINT"),
            ("money", "DECIMAL(19,4)"),
        ] {
            for aggregate in ["sum", "avg"] {
                for frame in [
                    "UNBOUNDED PRECEDING AND CURRENT ROW",
                    "1 PRECEDING AND 1 FOLLOWING",
                    "1 FOLLOWING AND 1 FOLLOWING",
                    "1 PRECEDING AND 1 PRECEDING",
                    "UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING",
                ] {
                    let sql = format!(
                        "SELECT count(*) FROM (SELECT __msduck_{aggregate}_{family}_frame(v) OVER w d, __msduck_{aggregate}_{family}(v) OVER w expected, count(*) OVER w <> count(v) OVER w eliminated FROM (SELECT i, CAST(CASE WHEN i%17=0 THEN NULL ELSE (i%7-3)*1.1234 END AS {kind}) v FROM range(6000) r(i)) s WINDOW w AS (ORDER BY i ROWS BETWEEN {frame})) WHERE d.value IS DISTINCT FROM expected OR typeof(d.value)<>typeof(expected) OR coalesce(d.eliminated,false)<>eliminated"
                    );
                    let wrong: i64 = db.query_row(&sql, [], |r| r.get(0)).unwrap();
                    assert_eq!(wrong, 0, "{aggregate}/{family}/{frame}");
                }
                // Exercise combining many independent groups with mixed signs,
                // fractional money coefficients and NULLs, using four workers.
                db.execute_batch("SET threads=4").unwrap();
                let sql = format!(
                    "SELECT count(*) FROM (SELECT i%10000 g,__msduck_{aggregate}_{family}_frame(v) d,__msduck_{aggregate}_{family}(v) expected,count(*)<>count(v) eliminated FROM (SELECT i,CAST(CASE WHEN i%17=0 THEN NULL ELSE (i%37-18)*1.1234 END AS {kind}) v FROM range(200000) r(i)) s GROUP BY g) WHERE d.value IS DISTINCT FROM expected OR typeof(d.value)<>typeof(expected) OR d.eliminated IS DISTINCT FROM eliminated"
                );
                assert_eq!(
                    db.query_row(&sql, [], |r| r.get::<_, i64>(0)).unwrap(),
                    0,
                    "{aggregate}/{family} groups"
                );
                for (source, warned) in [
                    (format!("SELECT NULL::{kind} v"), true),
                    (format!("SELECT NULL::{kind} v WHERE false"), false),
                ] {
                    let sql = format!(
                        "SELECT d.value IS NULL,coalesce(d.eliminated,false) FROM (SELECT __msduck_{aggregate}_{family}_frame(v) d FROM ({source}) s)"
                    );
                    let actual: (bool, bool) = db
                        .query_row(&sql, [], |r| Ok((r.get(0)?, r.get(1)?)))
                        .unwrap();
                    assert_eq!(actual, (true, warned), "{sql}");
                }
            }
        }
        assert!(std::mem::size_of::<State>() <= 32);
    }

    #[test]
    fn paired_integer_frames_preserve_overflow_and_single_evaluation() {
        let db = Connection::open_in_memory().unwrap();
        register(&db).unwrap();
        for (family, kind, maximum) in [
            ("int", "INT", "2147483647"),
            ("big", "BIGINT", "9223372036854775807"),
            ("money", "DECIMAL(19,4)", "922337203685477.5807"),
        ] {
            for aggregate in ["sum", "avg"] {
                // Overflow in unused segment-tree states must not become a query error.
                let sql = format!(
                    "SELECT count(*) FROM (SELECT __msduck_{aggregate}_{family}_frame(CAST({maximum} AS {kind})) OVER(ORDER BY i ROWS BETWEEN CURRENT ROW AND CURRENT ROW) d FROM range(6000) r(i)) WHERE d.value IS DISTINCT FROM CAST({maximum} AS {kind}) OR d.eliminated"
                );
                assert_eq!(db.query_row(&sql, [], |r| r.get::<_, i64>(0)).unwrap(), 0);
                let sql = format!(
                    "SELECT d.value FROM (SELECT __msduck_{aggregate}_{family}_frame(v) OVER() d FROM (VALUES(CAST({maximum} AS {kind})),(CAST(1 AS {kind})),(NULL)) s(v))"
                );
                let error = db.query_row(&sql, [], |r| r.get::<_, i64>(0)).unwrap_err();
                assert!(
                    error.to_string().contains("Arithmetic overflow"),
                    "{sql}: {error}"
                );
                db.execute_batch("CREATE OR REPLACE SEQUENCE integer_frame_calls")
                    .unwrap();
                let expected = if aggregate == "sum" { 5648 } else { 1 };
                let sql = format!(
                    "SELECT count(*) FROM (SELECT __msduck_{aggregate}_{family}_frame(CAST(CASE WHEN nextval('integer_frame_calls')%17=0 THEN NULL ELSE 1 END AS {kind})) OVER() d FROM range(6000)) WHERE d.value IS DISTINCT FROM CAST({expected} AS {kind}) OR NOT d.eliminated"
                );
                assert_eq!(db.query_row(&sql, [], |r| r.get::<_, i64>(0)).unwrap(), 0);
                assert_eq!(
                    db.query_row("SELECT currval('integer_frame_calls')", [], |r| r
                        .get::<_, i64>(0))
                        .unwrap(),
                    6000
                );
            }
        }
        let wrong: i64 = db.query_row("SELECT count(*) FROM (SELECT i,__msduck_sum_big_frame(i%2) OVER(ORDER BY i ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) d FROM range(100000) r(i)) WHERE d.value<>(i+1)//2 OR d.eliminated", [], |r|r.get(0)).unwrap();
        assert_eq!(wrong, 0);
    }
}
