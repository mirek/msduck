//! Server sessions: SPIDs, LOGIN7 client names, current databases and the
//! handles that terminate a session for ALTER DATABASE ... WITH ROLLBACK.
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
        Arc, Mutex, MutexGuard,
        atomic::{AtomicUsize, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
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
}

/// The server's sessions by SPID.
#[derive(Default)]
pub struct Registry {
    entries: Mutex<BTreeMap<i16, Entry>>,
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
            .filter_map(|(_, entry)| Some((entry.terminator.clone()?, entry.interrupt.clone())))
            .collect::<Vec<_>>();
        for (terminate, interrupt) in &victims {
            if let Some(interrupt) = interrupt {
                interrupt.interrupt();
            }
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

    pub fn set_running(&self, running: bool) {
        self.update(|entry| entry.running = running);
    }

    /// Exchange entries with `other`, so this handle takes over `other`'s
    /// SPID, login and client. RESETCONNECTION keeps the session's SPID.
    pub fn exchange(&mut self, other: &mut Registration) {
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
}

/// `__msduck_sessions()`: the columns of `sys.dm_exec_sessions` that msduck
/// provides, in SQL Server's column order.
pub struct SessionsTable;

pub struct Scan {
    rows: Vec<Row>,
    next: AtomicUsize,
}

// client_version is omitted: the value SQL Server reports was not captured.
const COLUMNS: [(&str, LogicalTypeId); 11] = [
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
        }
        output.set_len(rows.len());
        Ok(())
    }
}

/// Register `__msduck_sessions()` for the server instance.
pub fn register(db: &duckdb::Connection, registry: &Arc<Registry>) -> duckdb::Result<()> {
    db.register_table_function_with_extra_info::<SessionsTable, _>("__msduck_sessions", registry)
}
