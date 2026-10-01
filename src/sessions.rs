//! Server sessions: SPIDs, LOGIN7 client names, current databases, isolation
//! levels and the handles that terminate a session for ALTER DATABASE ...
//! WITH ROLLBACK or cancel its waits.
//!
//! `sys.dm_exec_sessions` reads a snapshot through the `__msduck_sessions()`
//! table function, taken when a query starts executing.
use duckdb::{
    core::{DataChunkHandle, Inserter, LogicalTypeHandle, LogicalTypeId},
    vtab::{BindInfo, InitInfo, TableFunctionInfo, VTab},
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// SQL Server numbers user sessions above the system sessions.
const FIRST_SPID: i16 = 51;

/// What the client reported in LOGIN7.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Client {
    pub host_name: String,
    pub program_name: String,
    pub client_interface_name: String,
    pub host_process_id: u32,
}

/// Closes a session's client connection.
pub type Terminator = Arc<dyn Fn() + Send + Sync>;

/// SQL Server's `transaction_isolation_level` for READ COMMITTED, the
/// default of a new session.
pub const READ_COMMITTED: i16 = 2;

/// Why a [`Registration::wait`] ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wake {
    /// The full interval passed.
    Elapsed,
    /// [`Registration::cancel`] was called, or the caller's poll returned true.
    Cancelled,
    /// The session is being terminated (ALTER DATABASE ... WITH ROLLBACK).
    Terminated,
}

/// Wakes a session blocked in [`Registration::wait`].
#[derive(Default)]
struct Signal {
    state: Mutex<SignalState>,
    changed: Condvar,
}

#[derive(Default)]
struct SignalState {
    cancelled: bool,
    terminated: bool,
}

impl Signal {
    fn lock(&self) -> MutexGuard<'_, SignalState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn raise(&self, change: impl FnOnce(&mut SignalState)) {
        change(&mut self.lock());
        self.changed.notify_all();
    }
}

struct Entry {
    /// Microseconds since the Unix epoch, UTC.
    login_time: i64,
    client: Option<Client>,
    login_name: String,
    database_id: i32,
    /// The DuckDB catalog of the current database.
    alias: String,
    running: bool,
    terminator: Option<Terminator>,
    /// Interrupts the statement running on the session's DuckDB connection.
    interrupt: Option<Arc<duckdb::InterruptHandle>>,
    /// `transaction_isolation_level`: 1 read uncommitted, 2 read committed,
    /// 3 repeatable read, 4 serializable, 5 snapshot.
    isolation: i16,
    /// Wakes the session's waits (WAITFOR).
    signal: Arc<Signal>,
}

/// The server's sessions by SPID.
#[derive(Default)]
pub struct Registry {
    entries: Mutex<BTreeMap<i16, Entry>>,
    principals: Mutex<Principals>,
}

/// Logins seen since the server started, for `sys.server_principals`.
/// msduck has no login catalog: `sa` always exists (principal_id 1) and
/// every other authenticated name gets the next ID from 256 at its first
/// login.
#[derive(Default)]
struct Principals {
    /// By case-insensitive name: display name, principal_id and first login
    /// time (microseconds since the Unix epoch).
    logins: BTreeMap<String, (String, i32, i64)>,
    next: i32,
}

impl Principals {
    fn add(&mut self, name: &str, login_time: i64) {
        let key = name.to_lowercase();
        if key == "sa" || self.logins.contains_key(&key) {
            return;
        }
        let id = 256 + self.next;
        self.next += 1;
        self.logins.insert(key, (name.into(), id, login_time));
    }
}

impl Registry {
    fn entries(&self) -> MutexGuard<'_, BTreeMap<i16, Entry>> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Give a new session the lowest free SPID.
    pub fn register(
        self: &Arc<Self>,
        database_id: i32,
        alias: &str,
    ) -> anyhow::Result<Registration> {
        let mut entries = self.entries();
        let mut spid = FIRST_SPID;
        for used in entries.keys().copied() {
            if used != spid {
                break;
            }
            spid = spid
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("no session ID is available"))?;
        }
        let login_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_micros() as i64);
        entries.insert(
            spid,
            Entry {
                login_time,
                client: None,
                login_name: "sa".into(),
                database_id,
                alias: alias.into(),
                running: false,
                terminator: None,
                interrupt: None,
                isolation: READ_COMMITTED,
                signal: Arc::default(),
            },
        );
        Ok(Registration {
            registry: self.clone(),
            spid,
        })
    }

    /// Terminate every session except `except` whose current database is
    /// `alias`: interrupt its statement and close its connection. Returns
    /// how many sessions were asked to end.
    pub fn terminate(&self, alias: &str, except: i16) -> usize {
        // Terminators run outside the lock: a terminated session removes its
        // own entry when it ends.
        let victims = self
            .entries()
            .iter()
            .filter(|(spid, entry)| **spid != except && entry.alias == alias)
            .filter_map(|(_, entry)| {
                Some((
                    entry.terminator.clone()?,
                    entry.interrupt.clone(),
                    entry.signal.clone(),
                ))
            })
            .collect::<Vec<_>>();
        for (terminate, interrupt, signal) in &victims {
            if let Some(interrupt) = interrupt {
                interrupt.interrupt();
            }
            signal.raise(|state| state.terminated = true);
            terminate();
        }
        victims.len()
    }

    fn snapshot(&self) -> Vec<Row> {
        self.entries()
            .iter()
            .map(|(spid, entry)| Row {
                session_id: *spid,
                login_time: entry.login_time,
                client: entry.client.clone(),
                login_name: entry.login_name.clone(),
                status: if entry.running { "running" } else { "sleeping" },
                database_id: entry.database_id as i16,
                isolation: entry.isolation,
            })
            .collect()
    }
}

/// A session's entry. Dropping it ends the session.
pub struct Registration {
    registry: Arc<Registry>,
    spid: i16,
}

impl Registration {
    pub fn spid(&self) -> i16 {
        self.spid
    }

    fn update(&self, change: impl FnOnce(&mut Entry)) {
        if let Some(entry) = self.registry.entries().get_mut(&self.spid) {
            change(entry);
        }
    }

    pub fn set_login(&self, client: Client, login_name: &str, terminator: Option<Terminator>) {
        let login_time = self
            .registry
            .entries()
            .get(&self.spid)
            .map_or(0, |entry| entry.login_time);
        self.registry
            .principals
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .add(login_name, login_time);
        self.update(|entry| {
            entry.client = Some(client);
            entry.login_name = login_name.into();
            entry.terminator = terminator;
        });
    }

    pub fn set_interrupt(&self, interrupt: Arc<duckdb::InterruptHandle>) {
        self.update(|entry| entry.interrupt = Some(interrupt));
    }

    pub fn set_database(&self, database_id: i32, alias: &str) {
        self.update(|entry| {
            entry.database_id = database_id;
            alias.clone_into(&mut entry.alias);
        });
    }

    /// Terminate the other sessions whose current database is `alias`.
    pub fn terminate_others(&self, alias: &str) -> usize {
        self.registry.terminate(alias, self.spid)
    }

    /// Mark the start (`true`) or end of a request. A new request starts
    /// uncancelled.
    pub fn set_running(&self, running: bool) {
        self.update(|entry| {
            entry.running = running;
            if running {
                entry.signal.lock().cancelled = false;
            }
        });
    }

    /// Record the session's transaction isolation level (1-5).
    pub fn set_isolation(&self, isolation: i16) {
        self.update(|entry| entry.isolation = isolation);
    }

    fn signal(&self) -> Option<Arc<Signal>> {
        self.registry
            .entries()
            .get(&self.spid)
            .map(|entry| entry.signal.clone())
    }

    /// Cancel the session's current request, as an Attention does: its
    /// current and later waits end until the next request starts
    /// ([`Registration::set_running`]).
    pub fn cancel(&self) {
        if let Some(signal) = self.signal() {
            signal.raise(|state| state.cancelled = true);
        }
    }

    /// Block for `interval` unless the session is cancelled or terminated
    /// first. `poll` is checked at least every `poll_interval` for other
    /// cancellation sources (an Attention flag owned by the request).
    pub fn wait(
        &self,
        interval: Duration,
        poll_interval: Duration,
        poll: &dyn Fn() -> bool,
    ) -> Wake {
        let Some(signal) = self.signal() else {
            return Wake::Terminated;
        };
        let deadline = Instant::now().checked_add(interval);
        let mut state = signal.lock();
        loop {
            if state.terminated {
                return Wake::Terminated;
            }
            if state.cancelled || poll() {
                return Wake::Cancelled;
            }
            let now = Instant::now();
            let remaining = match deadline {
                Some(deadline) if deadline <= now => return Wake::Elapsed,
                Some(deadline) => deadline - now,
                None => poll_interval,
            };
            state = signal
                .changed
                .wait_timeout(state, remaining.min(poll_interval))
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
    }

    /// Exchange entries with `other`, so this handle takes over `other`'s
    /// SPID, login and client. RESETCONNECTION keeps the session's SPID.
    /// Each handle keeps its own isolation level: a reset session starts at
    /// READ COMMITTED.
    pub fn exchange(&mut self, other: &mut Registration) {
        {
            let mut entries = self.registry.entries();
            let mine = entries.get(&self.spid).map(|entry| entry.isolation);
            let theirs = entries.get(&other.spid).map(|entry| entry.isolation);
            if let (Some(mine), Some(theirs)) = (mine, theirs) {
                if let Some(entry) = entries.get_mut(&self.spid) {
                    entry.isolation = theirs;
                }
                if let Some(entry) = entries.get_mut(&other.spid) {
                    entry.isolation = mine;
                }
            }
        }
        std::mem::swap(&mut self.spid, &mut other.spid);
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.registry.entries().remove(&self.spid);
    }
}

struct Row {
    session_id: i16,
    login_time: i64,
    client: Option<Client>,
    login_name: String,
    status: &'static str,
    database_id: i16,
    isolation: i16,
}

/// `__msduck_sessions()`: the columns of `sys.dm_exec_sessions` that msduck
/// provides, in SQL Server's column order.
pub struct SessionsTable;

pub struct Scan {
    rows: Vec<Row>,
    next: AtomicUsize,
}

// client_version is omitted: the value SQL Server reports was not captured.
// transaction_isolation_level is last, after the columns the catalog
// descriptors list (see src/query_catalog.rs).
const COLUMNS: [(&str, LogicalTypeId); 12] = [
    ("session_id", LogicalTypeId::Smallint),
    ("login_time", LogicalTypeId::Timestamp),
    ("host_name", LogicalTypeId::Varchar),
    ("program_name", LogicalTypeId::Varchar),
    ("host_process_id", LogicalTypeId::Integer),
    ("client_interface_name", LogicalTypeId::Varchar),
    ("login_name", LogicalTypeId::Varchar),
    ("status", LogicalTypeId::Varchar),
    ("is_user_process", LogicalTypeId::Boolean),
    ("original_login_name", LogicalTypeId::Varchar),
    ("database_id", LogicalTypeId::Smallint),
    ("transaction_isolation_level", LogicalTypeId::Smallint),
];

impl VTab for SessionsTable {
    type InitData = Scan;
    type BindData = ();

    fn bind(bind: &BindInfo) -> Result<(), Box<dyn std::error::Error>> {
        for (name, kind) in COLUMNS {
            bind.add_result_column(name, LogicalTypeHandle::from(kind));
        }
        Ok(())
    }

    // The snapshot is taken per execution, not per bind, so a cached plan
    // never returns stale sessions.
    fn init(init: &InitInfo) -> Result<Scan, Box<dyn std::error::Error>> {
        // SAFETY: registration stores an `Arc<Registry>` as extra info.
        let registry = unsafe { &*init.get_extra_info::<Arc<Registry>>() };
        Ok(Scan {
            rows: registry.snapshot(),
            next: AtomicUsize::new(0),
        })
    }

    fn func(
        function: &TableFunctionInfo<Self>,
        output: &mut DataChunkHandle,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let scan = function.get_init_data();
        let start = scan.next.load(Ordering::Relaxed).min(scan.rows.len());
        let rows = &scan.rows[start..(start + 2048).min(scan.rows.len())];
        scan.next.store(start + rows.len(), Ordering::Relaxed);
        let text = |column: usize, values: &mut dyn Iterator<Item = Option<&str>>| {
            let mut vector = output.flat_vector(column);
            for (row, value) in values.enumerate() {
                match value {
                    Some(value) => vector.insert(row, value),
                    None => vector.set_null(row),
                }
            }
        };
        text(
            2,
            &mut rows
                .iter()
                .map(|r| r.client.as_ref().map(|c| c.host_name.as_str())),
        );
        text(
            3,
            &mut rows
                .iter()
                .map(|r| r.client.as_ref().map(|c| c.program_name.as_str())),
        );
        text(
            5,
            &mut rows
                .iter()
                .map(|r| r.client.as_ref().map(|c| c.client_interface_name.as_str())),
        );
        text(6, &mut rows.iter().map(|r| Some(r.login_name.as_str())));
        text(7, &mut rows.iter().map(|r| Some(r.status)));
        text(9, &mut rows.iter().map(|r| Some(r.login_name.as_str())));
        let integers = |column: usize, values: &mut dyn Iterator<Item = Option<i32>>| {
            let mut vector = output.flat_vector(column);
            for (row, value) in values.enumerate() {
                match value {
                    // SAFETY: the vector holds at least `rows.len()` INTEGERs.
                    Some(value) => unsafe {
                        vector.as_mut_slice_with_len::<i32>(rows.len())[row] = value
                    },
                    None => vector.set_null(row),
                }
            }
        };
        integers(
            4,
            &mut rows
                .iter()
                .map(|r| r.client.as_ref().map(|c| c.host_process_id as i32)),
        );
        // SAFETY: each vector holds at least `rows.len()` values of its type.
        unsafe {
            let mut vector = output.flat_vector(0);
            let ids = vector.as_mut_slice_with_len::<i16>(rows.len());
            for (slot, row) in ids.iter_mut().zip(rows) {
                *slot = row.session_id;
            }
            let mut vector = output.flat_vector(1);
            let times = vector.as_mut_slice_with_len::<i64>(rows.len());
            for (slot, row) in times.iter_mut().zip(rows) {
                *slot = row.login_time;
            }
            let mut vector = output.flat_vector(8);
            let user = vector.as_mut_slice_with_len::<bool>(rows.len());
            user.fill(true);
            let mut vector = output.flat_vector(10);
            let databases = vector.as_mut_slice_with_len::<i16>(rows.len());
            for (slot, row) in databases.iter_mut().zip(rows) {
                *slot = row.database_id;
            }
            let mut vector = output.flat_vector(11);
            let levels = vector.as_mut_slice_with_len::<i16>(rows.len());
            for (slot, row) in levels.iter_mut().zip(rows) {
                *slot = row.isolation;
            }
        }
        output.set_len(rows.len());
        Ok(())
    }
}

/// `__msduck_server_principals()`: the logins seen since the server started
/// (name, principal_id, first login time), without `sa`.
pub struct PrincipalsTable;

pub struct PrincipalScan {
    rows: Vec<(String, i32, i64)>,
    done: AtomicUsize,
}

impl VTab for PrincipalsTable {
    type InitData = PrincipalScan;
    type BindData = ();

    fn bind(bind: &BindInfo) -> Result<(), Box<dyn std::error::Error>> {
        bind.add_result_column("name", LogicalTypeHandle::from(LogicalTypeId::Varchar));
        bind.add_result_column(
            "principal_id",
            LogicalTypeHandle::from(LogicalTypeId::Integer),
        );
        bind.add_result_column(
            "create_date",
            LogicalTypeHandle::from(LogicalTypeId::Timestamp),
        );
        Ok(())
    }

    fn init(init: &InitInfo) -> Result<PrincipalScan, Box<dyn std::error::Error>> {
        // SAFETY: registration stores an `Arc<Registry>` as extra info.
        let registry = unsafe { &*init.get_extra_info::<Arc<Registry>>() };
        let rows = registry
            .principals
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .logins
            .values()
            .cloned()
            .collect();
        Ok(PrincipalScan {
            rows,
            done: AtomicUsize::new(0),
        })
    }

    fn func(
        function: &TableFunctionInfo<Self>,
        output: &mut DataChunkHandle,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let scan = function.get_init_data();
        let start = scan.done.load(Ordering::Relaxed).min(scan.rows.len());
        let rows = &scan.rows[start..(start + 2048).min(scan.rows.len())];
        scan.done.store(start + rows.len(), Ordering::Relaxed);
        let names = output.flat_vector(0);
        for (row, (name, _, _)) in rows.iter().enumerate() {
            names.insert(row, name.as_str());
        }
        // SAFETY: each vector holds at least `rows.len()` values of its type.
        unsafe {
            let mut vector = output.flat_vector(1);
            let ids = vector.as_mut_slice_with_len::<i32>(rows.len());
            for (slot, (_, id, _)) in ids.iter_mut().zip(rows) {
                *slot = *id;
            }
            let mut vector = output.flat_vector(2);
            let times = vector.as_mut_slice_with_len::<i64>(rows.len());
            for (slot, (_, _, time)) in times.iter_mut().zip(rows) {
                *slot = *time;
            }
        }
        output.set_len(rows.len());
        Ok(())
    }
}

/// Register `__msduck_sessions()` and `__msduck_server_principals()` for the
/// server instance.
pub fn register(db: &duckdb::Connection, registry: &Arc<Registry>) -> duckdb::Result<()> {
    db.register_table_function_with_extra_info::<SessionsTable, _>("__msduck_sessions", registry)?;
    db.register_table_function_with_extra_info::<PrincipalsTable, _>(
        "__msduck_server_principals",
        registry,
    )
}
