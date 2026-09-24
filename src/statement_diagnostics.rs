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
