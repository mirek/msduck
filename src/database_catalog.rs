//! SQL Server databases as DuckDB catalogs attached to one server instance.
//!
//! The primary catalog is exposed as `master`. Each user database is a
//! separate attached catalog with its own `dbo` schema and msduck `sys`
//! objects. A registry in the primary catalog records user databases so that
//! file-backed servers re-attach them on startup.
//!
//! Native functions are registered once per DuckDB instance and cannot be
//! registered again, while msduck's macros, tables and views belong to each
//! catalog. A user database file is therefore bootstrapped in a short-lived
//! instance of its own before the server instance attaches it. In-memory
//! servers keep their user databases in a temporary directory.
use anyhow::{Context, Result, bail, ensure};
use duckdb::Connection;
use msduck_core::case_mapping::{Direction, Family};
use msduck_core::diagnostic::SqlError;
use std::path::{Path, PathBuf};

pub const MASTER: &str = "master";
pub const MASTER_ID: i32 = 1;
/// The system database that keeps backup and restore history. It is created
/// on first use (see `ensure_system`) and keeps SQL Server's ID.
pub const MSDB: &str = "msdb";
const MSDB_ID: i32 = 4;
/// SQL Server assigns user databases IDs after the four system databases.
const FIRST_USER_ID: i32 = 5;
/// System databases msduck does not provide; their names stay reserved.
const RESERVED: [&str; 4] = ["master", "tempdb", "model", "msdb"];
/// DuckDB's own catalogs cannot be reused as attachment aliases.
const BACKEND_RESERVED: [&str; 4] = ["main", "memory", "system", "temp"];
const COLLATION: &str = "SQL_Latin1_General_CP1_CI_AS";
const MAX_NAME: usize = 128;

/// Where the primary catalog lives and how user databases are stored.
#[derive(Debug)]
pub struct Catalog {
    primary: String,
    /// User database files are `<prefix>.<id>.<encoded name>.duckdb` in this
    /// directory; the prefix is the primary's full file name.
    directory: PathBuf,
    prefix: String,
    /// The directory belongs to an in-memory server and is removed with it.
    temporary: bool,
    /// Serializes CREATE and DROP: each spans a registry change, file work
    /// and an attach or detach that the registry key alone cannot order.
    changes: std::sync::Mutex<()>,
    /// How many sessions currently use each attached catalog. DROP refuses
    /// a database that any session uses.
    users: std::sync::Mutex<std::collections::HashMap<String, usize>>,
    /// Databases whose ALTER is terminating or waiting for other sessions.
    /// Sessions cannot enter them meanwhile, as under SQL Server's
    /// exclusive database lock.
    altering: std::sync::Mutex<std::collections::HashSet<String>>,
    /// The primary database file, or None for an in-memory server.
    primary_path: Option<PathBuf>,
    /// `master`'s recovery family, as BACKUP reports it. It is not stored:
    /// RESTORE never targets `master`.
    master_family: String,
}

impl Drop for Catalog {
    fn drop(&mut self) {
        if self.temporary {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }
}

/// A session's current database. While it exists, DROP refuses the database.
#[derive(Debug)]
pub struct Use {
    /// The SQL Server name.
    pub name: String,
    pub database_id: i32,
    alias: String,
    catalog: std::sync::Arc<Catalog>,
}

impl Use {
    /// The catalog the database belongs to.
    pub fn catalog(&self) -> &std::sync::Arc<Catalog> {
        &self.catalog
    }

    /// The DuckDB catalog that stores the database.
    pub fn alias(&self) -> &str {
        &self.alias
    }
}

impl Drop for Use {
    fn drop(&mut self) {
        self.catalog.release(&self.alias);
    }
}

/// The claim of the session that set a database to SINGLE_USER. SQL Server
/// keeps that session the database's single user even while its current
/// database is another one, until it disconnects or sets another user
/// access mode. Like `Use`, a hold keeps the database in use.
#[derive(Debug)]
pub struct Hold {
    alias: String,
    catalog: std::sync::Arc<Catalog>,
}

impl Hold {
    /// The DuckDB catalog that stores the held database.
    pub fn alias(&self) -> &str {
        &self.alias
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        self.catalog.release(&self.alias);
    }
}

/// `sys.databases.user_access`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserAccess {
    Multi = 0,
    Single = 1,
    Restricted = 2,
}

impl UserAccess {
    pub fn name(self) -> &'static str {
        match self {
            Self::Multi => "MULTI_USER",
            Self::Single => "SINGLE_USER",
            Self::Restricted => "RESTRICTED_USER",
        }
    }
}

/// One ALTER DATABASE ... SET option.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Setting {
    ReadCommittedSnapshot(bool),
    UserAccess(UserAccess),
}

/// `sys.databases.snapshot_isolation_state`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapshotIsolation {
    Off = 0,
    On = 1,
    /// ALLOW_SNAPSHOT_ISOLATION OFF waits for active transactions.
    ToOff = 2,
    /// ALLOW_SNAPSHOT_ISOLATION ON waits for active transactions.
    ToOn = 3,
}

/// What ALTER DATABASE does when other sessions use the database.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Termination {
    /// No clause: SQL Server waits for the other sessions indefinitely.
    Wait,
    NoWait,
    RollbackImmediate,
    /// Wait up to the duration, then terminate the remaining sessions.
    RollbackAfter(std::time::Duration),
}

/// A completed ALTER DATABASE.
#[derive(Debug)]
pub struct Altered {
    /// The DuckDB catalog of the altered database.
    pub alias: String,
    /// Whether other sessions were terminated.
    pub terminated: bool,
    /// The caller's claim when the database became SINGLE_USER.
    pub hold: Option<Hold>,
}

/// How long ROLLBACK waits for terminated sessions to release the database.
const RELEASE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// How long each termination round waits before terminating again.
const ROUND: std::time::Duration = std::time::Duration::from_millis(500);

/// One registry row. Its values are ordinary SQL data.
struct Row {
    name: String,
    name_key: String,
    database_id: i32,
    file: String,
    published: bool,
}

impl Row {
    /// The registry columns `read` expects, in order.
    const COLUMNS: &str = "r.name,r.name_key,r.database_id,r.file,r.published";

    /// Reads `Row::COLUMNS` from the first five columns.
    fn read(row: &duckdb::Row) -> duckdb::Result<Self> {
        Ok(Self {
            name: row.get(0)?,
            name_key: row.get(1)?,
            database_id: row.get(2)?,
            file: row.get(3)?,
            published: row.get(4)?,
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Attach {
    /// A new database whose file does not exist yet.
    Create,
    /// A registered database whose file must already exist.
    Recover,
}

/// A database visible to SQL Server clients.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Database {
    pub name: String,
    pub database_id: i32,
}

/// Bring a user database file up to date in an instance of its own. Scalar
/// registration defines instance-wide functions and catalog-local macros.
fn bootstrap_file(path: &Path) -> Result<()> {
    let db = Connection::open(path)?;
    crate::scalar::register(&db)?;
    crate::engine::ext::register(&db)?;
    crate::statement_diagnostics::Registry::default().register(&db)?;
    bootstrap_objects(&db)
}

/// Create the catalog-local objects every database needs in the connection's
/// default catalog: the `dbo` schema and msduck's catalog tables and views.
pub fn bootstrap_objects(db: &Connection) -> Result<()> {
    db.execute_batch("CREATE SCHEMA IF NOT EXISTS dbo")?;
    crate::schema_catalog::register(db)?;
    crate::object_catalog::register(db)?;
    crate::type_catalog::register(db)?;
    crate::declared_columns::register(db)?;
    crate::query_catalog::register(db)?;
    crate::index_catalog::register(db)?;
    crate::index_catalog::sync(db)?;
    crate::index_catalog::publish_views(db)?;
    crate::engine::ext::bootstrap_database(db)?;
    // The index views read the keys feature's record, which exists only
    // after the features bootstrap.
    crate::index_catalog::publish_views(db)?;
    Ok(())
}

impl Catalog {
    /// Prepare the registry in an already bootstrapped primary connection and
    /// attach every registered user database.
    pub fn open(owner: &Connection, path: &str) -> Result<Self> {
        let primary: String = owner.query_row("SELECT current_database()", [], |row| row.get(0))?;
        let catalog = if path == ":memory:" {
            let directory = temporary_directory()?;
            Self {
                primary,
                directory,
                prefix: "msduck".into(),
                temporary: true,
                changes: std::sync::Mutex::new(()),
                users: Default::default(),
                altering: Default::default(),
                primary_path: None,
                master_family: new_guid(),
            }
        } else {
            // Resolve the directory once, so later file work depends neither
            // on the process's current directory nor on symbolic links that
            // may be retargeted while the server runs.
            // The primary is open, so its file exists; the name of the file
            // behind any link is the prefix.
            let file = std::fs::canonicalize(path)
                .with_context(|| format!("resolve database path {path}"))?;
            Self {
                primary,
                directory: file
                    .parent()
                    .map(Path::to_path_buf)
                    .context("database path needs a parent directory")?,
                prefix: file
                    .file_name()
                    .and_then(|name| name.to_str())
                    .context("database path needs a UTF-8 file name")?
                    .to_owned(),
                temporary: false,
                changes: std::sync::Mutex::new(()),
                users: Default::default(),
                altering: Default::default(),
                primary_path: Some(file.clone()),
                master_family: new_guid(),
            }
        };
        owner.execute_batch(&format!(
            "CREATE TABLE IF NOT EXISTS {registry}(
                name_key VARCHAR PRIMARY KEY,
                name VARCHAR NOT NULL,
                database_id INTEGER UNIQUE NOT NULL,
                file VARCHAR NOT NULL,
                create_date TIMESTAMP NOT NULL,
                published BOOLEAN NOT NULL DEFAULT false);
             CREATE SEQUENCE IF NOT EXISTS {ids} START {FIRST_USER_ID} MAXVALUE 32767 NO CYCLE;
             ALTER TABLE {registry} ADD COLUMN IF NOT EXISTS user_access UTINYINT DEFAULT 0;
             ALTER TABLE {registry} ADD COLUMN IF NOT EXISTS read_committed_snapshot BOOLEAN DEFAULT false;
             ALTER TABLE {registry} ADD COLUMN IF NOT EXISTS snapshot_isolation_state UTINYINT DEFAULT 0;
             ALTER TABLE {registry} ADD COLUMN IF NOT EXISTS family_guid VARCHAR;
             ALTER TABLE {registry} ADD COLUMN IF NOT EXISTS database_guid VARCHAR;
             ALTER TABLE {registry} ADD COLUMN IF NOT EXISTS data_name VARCHAR;
             ALTER TABLE {registry} ADD COLUMN IF NOT EXISTS log_name VARCHAR;
             ALTER TABLE {registry} ADD COLUMN IF NOT EXISTS data_path VARCHAR;
             ALTER TABLE {registry} ADD COLUMN IF NOT EXISTS log_path VARCHAR;
             -- An ALLOW_SNAPSHOT_ISOLATION change that a stop interrupted did
             -- not complete: return to the state it started from.
             UPDATE {registry} SET snapshot_isolation_state = CASE snapshot_isolation_state WHEN 2 THEN 1 ELSE 0 END
                 WHERE snapshot_isolation_state IN (2, 3)",
            registry = catalog.registry(),
            ids = catalog.ids(),
        ))?;
        let registered = {
            let mut query = owner.prepare(&format!(
                "SELECT {} FROM {} r ORDER BY database_id",
                Row::COLUMNS,
                catalog.registry()
            ))?;
            query
                .query_map([], Row::read)?
                .collect::<duckdb::Result<Vec<_>>>()?
        };
        for row in registered {
            let name = row.name.clone();
            if catalog.forget_stale(owner, &row, false)? {
                continue;
            }
            // A CREATE that failed, or a DROP that stopped, left the database
            // hidden. Recovery never publishes it; DROP can remove it.
            if !row.published {
                eprintln!("msduck: database {name} is unavailable: it was not published");
                continue;
            }
            let result = catalog
                .attach(owner, &row, Attach::Recover)
                .and_then(|()| catalog.publish(owner, &name))
                .and_then(|()| catalog.set_published(owner, &row.name_key, true));
            if let Err(error) = result {
                // SQL Server keeps serving other databases when one cannot be
                // recovered; the database stays registered but is not listed.
                // Listing follows attachment, so detach a partial recovery.
                // A catalog DuckDB cannot detach would be listed while
                // incomplete, so the server does not start.
                let _ = owner.execute_batch(&format!("DETACH DATABASE IF EXISTS {}", quote(&name)));
                ensure!(
                    !catalog.attached(owner, &name)?,
                    "database {name} failed to recover and could not be detached: {error:#}"
                );
                eprintln!("msduck: database {name} is unavailable: {error:#}");
            }
        }
        catalog.publish(owner, &catalog.primary.clone())?;
        Ok(catalog)
    }

    /// The DuckDB catalog name that stores `master`.
    pub fn primary(&self) -> &str {
        &self.primary
    }

    /// Resolve a client database name to its attached DuckDB catalog alias.
    /// CREATE and DROP hold the same lock, so a database is never resolved
    /// while it is attached but not yet published, or while it is detaching.
    pub fn resolve(&self, db: &Connection, name: &str) -> Result<Option<String>> {
        let _change = self.lock();
        self.resolve_published(db, name)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ()> {
        self.changes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn resolve_published(&self, db: &Connection, name: &str) -> Result<Option<String>> {
        if key(name) == MASTER {
            return Ok(Some(self.primary.clone()));
        }
        let alias = db
            .query_row(
                &format!(
                    "SELECT r.name FROM {} r JOIN duckdb_databases() d ON d.database_name=r.name \
                     WHERE r.name_key=? AND r.published",
                    self.registry()
                ),
                [key(name)],
                |row| row.get(0),
            )
            .map(Some)
            .or_else(|error| match error {
                duckdb::Error::QueryReturnedNoRows => Ok(None),
                error => Err(error),
            })?;
        Ok(alias)
    }

    /// Make `name` the connection's current database with the `dbo` schema.
    /// Returns the database's SQL Server name.
    pub fn select(&self, db: &Connection, name: &str) -> Result<String> {
        let _change = self.lock();
        let alias = self
            .resolve_published(db, name)?
            .ok_or_else(|| missing(name))?;
        db.execute_batch(&format!("USE {}; SET schema = 'dbo'", quote(&alias)))?;
        Ok(self.display(&alias))
    }

    /// Make `name` the connection's current database for a session. The
    /// returned guard keeps the database in use until it is dropped.
    pub fn enter(self: &std::sync::Arc<Self>, db: &Connection, name: &str) -> Result<Use> {
        self.enter_as(db, name, &|_| 0)
    }

    /// `enter` for a session that already uses databases: `own` counts the
    /// session's own uses and holds of a catalog alias, which do not keep it
    /// out of a SINGLE_USER database.
    pub fn enter_as(
        self: &std::sync::Arc<Self>,
        db: &Connection,
        name: &str,
        own: &dyn Fn(&str) -> usize,
    ) -> Result<Use> {
        let _change = self.lock();
        let alias = self
            .resolve_published(db, name)?
            .ok_or_else(|| missing(name))?;
        let (database_id, user_access) = if alias == self.primary {
            (MASTER_ID, UserAccess::Multi as u8)
        } else {
            db.query_row(
                &format!(
                    "SELECT database_id,coalesce(user_access,0) FROM {} WHERE name=?",
                    self.registry()
                ),
                [&alias],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?
        };
        if self.in_transition(&alias) {
            bail!(SqlError::new(
                952,
                1,
                format!(
                    "Database '{}' is in transition. Try the statement later.",
                    self.display(&alias)
                ),
            ));
        }
        if user_access == UserAccess::Single as u8 && self.users(&alias) > own(&alias) {
            let mut error = SqlError::new(
                924,
                1,
                format!(
                    "Database '{}' is already open and can only have one user at a time.",
                    self.display(&alias)
                ),
            );
            error.severity = 14;
            bail!(error);
        }
        db.execute_batch(&format!("USE {}; SET schema = 'dbo'", quote(&alias)))?;
        *self.user_counts().entry(alias.clone()).or_default() += 1;
        Ok(Use {
            name: self.display(&alias),
            database_id,
            alias,
            catalog: self.clone(),
        })
    }

    fn user_counts(&self) -> std::sync::MutexGuard<'_, std::collections::HashMap<String, usize>> {
        self.users
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn users(&self, alias: &str) -> usize {
        self.user_counts().get(alias).copied().unwrap_or(0)
    }

    fn in_transition(&self, alias: &str) -> bool {
        self.altering
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains(alias)
    }

    fn release(&self, alias: &str) {
        let mut users = self.user_counts();
        if let Some(count) = users.get_mut(alias) {
            *count -= 1;
            if *count == 0 {
                users.remove(alias);
            }
        }
    }

    /// Set database options, as ALTER DATABASE does. `own` counts the
    /// caller's uses and holds of a catalog alias; `terminate` ends the other
    /// sessions whose current database is the alias and returns how many it
    /// asked to end. Errors follow reference/alter-database-sessions.json;
    /// the caller adds the final 5069 where SQL Server sends it.
    pub fn alter(
        self: &std::sync::Arc<Self>,
        db: &Connection,
        name: &str,
        settings: &[Setting],
        termination: Termination,
        own: &dyn Fn(&str) -> usize,
        terminate: &dyn Fn(&str) -> usize,
    ) -> Result<Altered> {
        // Termination waits without the change lock, so logins, USE and
        // the sessions being terminated are never blocked by it. Each round
        // re-checks the database under the lock.
        // RESTRICTED_USER admits db_owner, dbcreator and sysadmin members.
        // Every msduck login is sysadmin, so only these options need the
        // other sessions gone. Setting READ_COMMITTED_SNAPSHOT to its current
        // value completes at once, even WITH NO_WAIT
        // (reference/tedious-compat-gaps.json).
        let exclusive = |read_committed_snapshot: bool| {
            settings.iter().any(|setting| match setting {
                Setting::ReadCommittedSnapshot(on) => *on != read_committed_snapshot,
                Setting::UserAccess(access) => *access == UserAccess::Single,
            })
        };
        let deadline = |timeout| {
            std::time::Instant::now()
                .checked_add(timeout)
                .unwrap_or_else(|| std::time::Instant::now() + RELEASE_TIMEOUT * 1000)
        };
        /// Keeps a database in transition until the ALTER ends.
        struct Transition<'a>(&'a Catalog, Option<String>);
        impl Drop for Transition<'_> {
            fn drop(&mut self) {
                if let Some(alias) = self.1.take() {
                    self.0
                        .altering
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .remove(&alias);
                }
            }
        }
        let mut transition = Transition(self, None);
        let mut terminated = false;
        let mut waited = false;
        let mut release_deadline = None;
        loop {
            let change = self.lock();
            let (alias, display) = self.alterable(db, name, settings)?;
            let others = || self.users(&alias).saturating_sub(own(&alias));
            let (user_access, read_committed_snapshot): (u8, bool) = db.query_row(
                &format!(
                    "SELECT coalesce(user_access,0),coalesce(read_committed_snapshot,false) FROM {} WHERE name=?",
                    self.registry()
                ),
                [&alias],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            if user_access == UserAccess::Single as u8 && others() > 0 {
                bail!(SqlError::new(
                    5064,
                    1,
                    format!(
                        "Changes to the state or options of database '{display}' cannot be made at this time. The database is in single-user mode, and a user is currently connected to it."
                    )
                ));
            }
            if !exclusive(read_committed_snapshot) || others() == 0 {
                return self.apply(db, alias, settings, terminated, change);
            }
            let (wait, kill) = match termination {
                Termination::NoWait => bail!(SqlError::new(
                    5070,
                    2,
                    format!(
                        "Database state cannot be changed while other users are using the database '{display}'"
                    )
                )),
                Termination::Wait => bail!(
                    "ALTER DATABASE without a termination clause would wait for the other sessions using database '{display}'; msduck supports WITH ROLLBACK IMMEDIATE, WITH ROLLBACK AFTER or WITH NO_WAIT here"
                ),
                Termination::RollbackAfter(delay) if !waited => {
                    waited = true;
                    (deadline(delay), false)
                }
                Termination::RollbackAfter(_) | Termination::RollbackImmediate => {
                    let limit = *release_deadline.get_or_insert_with(|| deadline(RELEASE_TIMEOUT));
                    ensure!(
                        std::time::Instant::now() < limit,
                        "the other sessions using database '{display}' did not end"
                    );
                    (deadline(ROUND).min(limit), true)
                }
            };
            // A concurrent ALTER of the same database may already own the
            // marker; only the ALTER that set it removes it.
            if transition.1.is_none()
                && self
                    .altering
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .insert(alias.clone())
            {
                transition.1 = Some(alias.clone());
            }
            drop(change);
            // A session still logging in or entering the database may not be
            // terminable yet; a later round terminates it.
            if kill {
                terminated |= terminate(&alias) > 0;
            }
            while others() > 0 && std::time::Instant::now() < wait {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }

    /// Resolve an ALTER DATABASE target that may be altered.
    fn alterable(
        &self,
        db: &Connection,
        name: &str,
        settings: &[Setting],
    ) -> Result<(String, String)> {
        let alias = self.alterable_name(db, name)?;
        let display = self.display(&alias);
        if alias == self.primary {
            let (option, state) = match settings.first() {
                Some(Setting::ReadCommittedSnapshot(_)) => ("READ_COMMITTED_SNAPSHOT", 2),
                Some(Setting::UserAccess(access)) => (access.name(), 5),
                None => bail!("ALTER DATABASE requires an option"),
            };
            bail!(SqlError::new(
                5058,
                state,
                format!("Option '{option}' cannot be set in database '{display}'.")
            ));
        }
        Ok((alias, display))
    }

    /// Store ALTER DATABASE options while holding the change lock.
    fn apply(
        self: &std::sync::Arc<Self>,
        db: &Connection,
        alias: String,
        settings: &[Setting],
        terminated: bool,
        _change: std::sync::MutexGuard<'_, ()>,
    ) -> Result<Altered> {
        let mut read_committed_snapshot = None;
        let mut access = None;
        for setting in settings {
            match *setting {
                Setting::ReadCommittedSnapshot(on) => read_committed_snapshot = Some(on),
                Setting::UserAccess(value) => access = Some(value as u8),
            }
        }
        db.execute(
            &format!(
                "UPDATE {} SET read_committed_snapshot=coalesce(?,read_committed_snapshot),\
                 user_access=coalesce(?,user_access) WHERE name=?",
                self.registry()
            ),
            duckdb::params![read_committed_snapshot, access, alias],
        )?;
        let hold = (access == Some(UserAccess::Single as u8)).then(|| {
            *self.user_counts().entry(alias.clone()).or_default() += 1;
            Hold {
                alias: alias.clone(),
                catalog: self.clone(),
            }
        });
        Ok(Altered {
            alias,
            terminated,
            hold,
        })
    }

    /// Store `sys.databases.snapshot_isolation_state` for a user database:
    /// [`SnapshotIsolation`] as ALTER DATABASE ... SET
    /// ALLOW_SNAPSHOT_ISOLATION moves through it.
    pub fn set_snapshot_isolation(
        &self,
        db: &Connection,
        alias: &str,
        state: SnapshotIsolation,
    ) -> Result<()> {
        let _change = self.lock();
        db.execute(
            &format!(
                "UPDATE {} SET snapshot_isolation_state=? WHERE name=?",
                self.registry()
            ),
            duckdb::params![state as u8, alias],
        )?;
        Ok(())
    }

    /// The DuckDB catalog alias and SQL Server name of an ALTER DATABASE
    /// target, or error 5011 (the caller adds the final 5069).
    pub fn alter_target(&self, db: &Connection, name: &str) -> Result<(String, String)> {
        let _change = self.lock();
        let alias = self.alterable_name(db, name)?;
        let display = self.display(&alias);
        Ok((alias, display))
    }

    fn alterable_name(&self, db: &Connection, name: &str) -> Result<String> {
        self.resolve_published(db, name)?.ok_or_else(|| {
            let mut error = SqlError::new(
                5011,
                5,
                format!(
                    "User does not have permission to alter database '{name}', the database does not exist, or the database is not in a state that allows access checks."
                ),
            );
            error.severity = 14;
            error.into()
        })
    }

    /// `sys.databases.snapshot_isolation_state` of the database with this
    /// DuckDB catalog alias: always ON for `master`, OFF for an unknown
    /// alias. Inside a transaction the registry is read through another
    /// connection, so the state is current rather than the transaction's
    /// snapshot.
    pub fn snapshot_isolation(&self, db: &Connection, alias: &str) -> Result<SnapshotIsolation> {
        if alias == self.primary {
            return Ok(SnapshotIsolation::On);
        }
        let fresh;
        let db = if db.is_autocommit() {
            db
        } else {
            fresh = db.try_clone()?;
            &fresh
        };
        let state: u8 = db
            .query_row(
                &format!(
                    "SELECT coalesce(snapshot_isolation_state,0) FROM {} WHERE name=?",
                    self.registry()
                ),
                [alias],
                |row| row.get(0),
            )
            .or_else(|error| match error {
                duckdb::Error::QueryReturnedNoRows => Ok(0),
                error => Err(error),
            })?;
        Ok(match state {
            1 => SnapshotIsolation::On,
            2 => SnapshotIsolation::ToOff,
            3 => SnapshotIsolation::ToOn,
            _ => SnapshotIsolation::Off,
        })
    }

    /// The SQL Server name of an attached DuckDB catalog alias.
    pub fn display_name(&self, alias: &str) -> String {
        self.display(alias)
    }

    fn alias(&self, db: &Connection) -> Result<String> {
        Ok(db.query_row("SELECT current_database()", [], |row| row.get(0))?)
    }

    /// The SQL Server name of the connection's current database.
    pub fn current(&self, db: &Connection) -> Result<String> {
        let alias: String = db.query_row("SELECT current_database()", [], |row| row.get(0))?;
        Ok(self.display(&alias))
    }

    /// Every database clients can use, ordered by ID.
    pub fn list(&self, db: &Connection) -> Result<Vec<Database>> {
        let mut query = db.prepare(&format!(
            "SELECT name,database_id FROM {}.sys.databases ORDER BY database_id",
            quote(&self.primary)
        ))?;
        let rows = query
            .query_map([], |row| {
                Ok(Database {
                    name: row.get(0)?,
                    database_id: row.get(1)?,
                })
            })?
            .collect::<duckdb::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Create and attach a user database. The connection must not be inside
    /// a transaction because DuckDB attaches catalogs outside transactions.
    pub fn create(&self, db: &Connection, name: &str) -> Result<Database> {
        validate(name)?;
        let _change = self.lock();
        if RESERVED.contains(&key(name).as_str()) {
            return Err(exists(name));
        }
        self.create_locked(db, name, None, None)
    }

    /// Create a user database, or `msdb` with its fixed ID, while holding the
    /// change lock. A staged file, restored from a backup, becomes the new
    /// database's file before it is attached.
    fn create_locked(
        &self,
        db: &Connection,
        name: &str,
        fixed_id: Option<i32>,
        staged: Option<&Restore<'_>>,
    ) -> Result<Database> {
        let name_key = key(name);
        if let Some((row, attached)) = self.lookup(db, &name_key)?
            && !self.forget_stale(db, &row, attached)?
        {
            return Err(exists(name));
        }
        ensure!(
            !BACKEND_RESERVED.contains(&name_key.as_str()) && name_key != key(&self.primary),
            "database name '{name}' is reserved by msduck"
        );
        // Neither the file nor a WAL may exist: CREATE owns, and on failure
        // deletes, only storage it made itself. Another primary, such as one
        // that took over a renamed primary's file name, may hold a generated
        // name, so an occupied name moves on to the next ID until the
        // sequence, which does not cycle, runs out.
        let (database_id, file) = match fixed_id {
            Some(database_id) => {
                let file = file_name(&self.prefix, database_id, &name_key);
                let path = self.directory.join(&file);
                ensure!(
                    !occupied(&path)? && !occupied(&wal(&path))?,
                    "cannot create database {name}: '{}' already exists",
                    path.display()
                );
                (database_id, file)
            }
            None => loop {
                let database_id: i32 = db.query_row(
                    &format!("SELECT CAST(nextval({}) AS INTEGER)", literal(&self.ids())),
                    [],
                    |row| row.get(0),
                )?;
                let file = file_name(&self.prefix, database_id, &name_key);
                let path = self.directory.join(&file);
                // Only an existing entry skips the ID; any other error from
                // inspecting storage ends the statement without using more IDs.
                if !occupied(&path)? && !occupied(&wal(&path))? {
                    break (database_id, file);
                }
            },
        };
        // The primary key serializes concurrent creators before any attach.
        let inserted = db.execute(
            &format!(
                "INSERT INTO {}(name_key,name,database_id,file,create_date,
                                family_guid,database_guid,data_name,log_name,data_path,log_path)
                 VALUES (?,?,?,?,CAST(now() AS TIMESTAMP),?,?,?,?,?,?)
                 ON CONFLICT DO NOTHING",
                self.registry()
            ),
            duckdb::params![
                name_key,
                name,
                database_id,
                file,
                staged.map(|staged| staged.family_guid),
                staged.map(|staged| staged.database_guid),
                staged.map(|staged| staged.data_name),
                staged.map(|staged| staged.log_name),
                staged.map(|staged| staged.data_path),
                staged.map(|staged| staged.log_path),
            ],
        )?;
        if inserted == 0 {
            return Err(exists(name));
        }
        let row = Row {
            name: name.to_owned(),
            name_key: name_key.clone(),
            database_id,
            file,
            published: false,
        };
        // A restored file takes the generated name; on failure the cleanup
        // below deletes it like any file CREATE made.
        let staged_file = match staged {
            Some(staged) => self
                .path(&row)
                .and_then(|path| move_file(staged.staging, &path)),
            None => Ok(()),
        };
        // Clients see the database only once every catalog lists it.
        let attached = staged_file
            .and_then(|()| self.attach(db, &row, Attach::Create))
            .and_then(|()| self.publish_all(db))
            .and_then(|()| self.set_published(db, &name_key, true));
        if let Err(error) = attached {
            // DuckDB can report an error after detaching, so the catalog's
            // presence decides. A catalog still attached keeps its files and
            // its unpublished registration, and DROP can finish the cleanup.
            let _ = db.execute_batch(&format!("DETACH DATABASE IF EXISTS {}", quote(name)));
            let detached = !self.attached(db, name).unwrap_or(true);
            let cleanup = if detached {
                self.delete_files(&row, false)
            } else {
                Err(anyhow::anyhow!("the database could not be detached"))
            };
            // Forget the database only once its files are gone. Otherwise it
            // stays registered but unavailable, and DROP can finish the cleanup.
            match cleanup {
                Ok(()) => {
                    let _ = db.execute(
                        &format!("DELETE FROM {} WHERE name_key=?", self.registry()),
                        [&name_key],
                    );
                }
                Err(cleanup) => {
                    eprintln!("msduck: database {name} stays registered: {cleanup:#}")
                }
            }
            return Err(error);
        }
        Ok(Database {
            name: name.to_owned(),
            database_id,
        })
    }

    /// Detach a user database and remove its storage. The caller must make
    /// sure no session is using it; the connection must not be using it.
    pub fn remove(&self, db: &Connection, name: &str) -> Result<()> {
        self.remove_as(db, name, &|_| 0)
    }

    /// `remove` for a session whose SINGLE_USER holds, counted by `held`
    /// per catalog alias, do not keep the database in use.
    pub fn remove_as(
        &self,
        db: &Connection,
        name: &str,
        held: &dyn Fn(&str) -> usize,
    ) -> Result<()> {
        let _change = self.lock();
        let name_key = key(name);
        // A published system database cannot be dropped. An msdb whose
        // creation or recovery failed can, so that it can be created again.
        let unavailable_msdb = name_key == MSDB
            && self
                .lookup(db, &name_key)?
                .is_some_and(|(row, attached)| !(row.published && attached));
        if RESERVED.contains(&name_key.as_str()) && !unavailable_msdb {
            bail!(SqlError::new(
                3708,
                4,
                format!("Cannot drop the database '{name_key}' because it is a system database.")
            ));
        }
        // A registered database that failed to attach can still be dropped.
        let Some((row, attached)) = self.lookup(db, &name_key)? else {
            let mut error = SqlError::new(
                3701,
                1,
                format!(
                    "Cannot drop the database '{name}', because it does not exist or you do not have permission."
                ),
            );
            error.severity = 11;
            bail!(error)
        };
        // SQL Server reports state 3 when the dropping connection itself uses
        // the database and state 4 when only other sessions do.
        let own = attached && self.alias(db)? == row.name;
        let users = self.users(&row.name).saturating_sub(held(&row.name));
        if own || users > 0 {
            bail!(SqlError::new(
                3702,
                if own { 3 } else { 4 },
                format!(
                    "Cannot drop database \"{}\" because it is currently in use.",
                    row.name
                )
            ));
        }
        // Validate the stored file name before detaching anything.
        let path = self.path(&row)?;
        // A failed DROP leaves the database available while its file is
        // intact. DuckDB may report a failed checkpoint after it detached;
        // then nothing was deleted and the WAL still holds its changes.
        // Deletion follows only a successful detach, which checkpoints, and
        // removes the WAL before the file, so a file left behind is complete.
        let restore = |error: anyhow::Error| {
            if !present(&path).unwrap_or(false) {
                return error;
            }
            if let Err(restore) = self.set_published(db, &name_key, row.published) {
                eprintln!("msduck: database {} stays hidden: {restore:#}", row.name);
            }
            if attached {
                let restored = path
                    .to_str()
                    .context("database path is not UTF-8")
                    .and_then(|file| {
                        Ok(db.execute_batch(&format!(
                            "ATTACH IF NOT EXISTS {} AS {}",
                            literal(file),
                            quote(&row.name)
                        ))?)
                    });
                if let Err(restore) = restored {
                    eprintln!("msduck: database {} stays detached: {restore:#}", row.name);
                }
            }
            error
        };
        if attached {
            // Deleting needs a writable directory, and DuckDB cannot attach
            // the file again without one. Check before detaching, so the
            // usual failure leaves the database untouched.
            deletable(&path)?;
        }
        // Hide the database first. Once its files are gone, an unpublished
        // registration without files is stale, and CREATE or startup removes
        // it, so no failure is reported after the destructive step.
        self.set_published(db, &name_key, false)?;
        if attached {
            db.execute_batch(&format!("DETACH DATABASE {}", quote(&row.name)))
                .map_err(|error| restore(error.into()))?;
        }
        // Keep the registration until the files are gone, so a failed
        // deletion leaves a database that another DROP can finish removing.
        self.delete_files(&row, attached).map_err(restore)?;
        if let Err(error) = db.execute(
            &format!("DELETE FROM {} WHERE name_key=?", self.registry()),
            [&name_key],
        ) {
            eprintln!(
                "msduck: stale registration of {} remains: {error:#}",
                row.name
            );
        }
        Ok(())
    }

    /// The registration for a key and whether its catalog is attached.
    fn lookup(&self, db: &Connection, name_key: &str) -> Result<Option<(Row, bool)>> {
        Ok(db
            .query_row(
                &format!(
                    "SELECT {},d.database_name IS NOT NULL \
                     FROM {} r LEFT JOIN duckdb_databases() d ON d.database_name=r.name \
                     WHERE r.name_key=?",
                    Row::COLUMNS,
                    self.registry()
                ),
                [name_key],
                |row| Ok((Row::read(row)?, row.get::<_, bool>(5)?)),
            )
            .map(Some)
            .or_else(|error| match error {
                duckdb::Error::QueryReturnedNoRows => Ok(None),
                error => Err(error),
            })?)
    }

    fn attached(&self, db: &Connection, alias: &str) -> Result<bool> {
        Ok(db.query_row(
            "SELECT count(*) > 0 FROM duckdb_databases() WHERE database_name=?",
            [alias],
            |row| row.get(0),
        )?)
    }

    /// Remove a registration left by an interrupted CREATE or DROP: never
    /// published, or hidden by DROP, and without a catalog or any file.
    /// Callers hold the change lock or run before the server accepts clients.
    fn forget_stale(&self, db: &Connection, row: &Row, attached: bool) -> Result<bool> {
        if row.published || attached {
            return Ok(false);
        }
        let Ok(path) = self.path(row) else {
            return Ok(false);
        };
        // Any entry under the file's name, or one that cannot be inspected,
        // keeps the registration for DROP; startup is never stopped here.
        if occupied(&path).unwrap_or(true) {
            return Ok(false);
        }
        // A WAL without its file is what a committed DROP could not delete.
        let wal = wal(&path);
        match present(&wal) {
            Ok(false) => {}
            Ok(true) if std::fs::remove_file(&wal).is_ok() => {}
            _ => return Ok(false),
        }
        db.execute(
            &format!("DELETE FROM {} WHERE name_key=?", self.registry()),
            [&row.name_key],
        )?;
        Ok(true)
    }

    fn attach(&self, db: &Connection, row: &Row, mode: Attach) -> Result<()> {
        let name = row.name.as_str();
        let path = self.path(row)?;
        // DuckDB creates missing files; recovery must not replace lost data
        // with an empty database. `present` rejects symbolic links, so a
        // replaced file cannot redirect DuckDB outside the data directory.
        // CREATE checked, under the change lock, that neither path exists.
        if mode == Attach::Recover {
            ensure!(
                present(&path)?,
                "database file '{}' is missing",
                path.display()
            );
            present(&wal(&path))?;
        }
        bootstrap_file(&path)?;
        let path = path.to_str().context("database path is not UTF-8")?;
        db.execute_batch(&format!("ATTACH {} AS {}", literal(path), quote(name)))?;
        Ok(())
    }

    /// Refresh `sys.databases` in every attached database. Views bind to
    /// their own catalog, so each one names the primary registry explicitly.
    /// Publishing needs the server instance, where the registry is visible.
    fn publish_all(&self, db: &Connection) -> Result<()> {
        let mut aliases = vec![self.primary.clone()];
        let mut query = db.prepare(&format!(
            "SELECT r.name FROM {} r JOIN duckdb_databases() d ON d.database_name=r.name",
            self.registry()
        ))?;
        aliases.extend(
            query
                .query_map([], |row| row.get(0))?
                .collect::<duckdb::Result<Vec<String>>>()?,
        );
        for alias in aliases {
            self.publish(db, &alias)?;
        }
        Ok(())
    }

    fn publish(&self, db: &Connection, alias: &str) -> Result<()> {
        let target = quote(alias);
        self.publish_files(db, alias)?;
        db.execute_batch(&format!(
            "CREATE OR REPLACE VIEW {target}.sys.databases AS
             SELECT CAST(name AS VARCHAR) AS name,
                    CAST(database_id AS INTEGER) AS database_id,
                    CAST(NULL AS INTEGER) AS source_database_id,
                    CAST(create_date AS TIMESTAMP) AS create_date,
                    CAST(160 AS UTINYINT) AS compatibility_level,
                    CAST('{COLLATION}' AS VARCHAR) AS collation_name,
                    CAST(user_access AS UTINYINT) AS user_access,
                    CAST(CASE user_access WHEN 1 THEN 'SINGLE_USER' WHEN 2 THEN 'RESTRICTED_USER'
                         ELSE 'MULTI_USER' END AS VARCHAR) AS user_access_desc,
                    false AS is_read_only,
                    CAST(0 AS UTINYINT) AS state,
                    CAST('ONLINE' AS VARCHAR) AS state_desc,
                    CAST(snapshot_isolation_state AS UTINYINT) AS snapshot_isolation_state,
                    CAST(CASE snapshot_isolation_state WHEN 1 THEN 'ON'
                         WHEN 2 THEN 'IN_TRANSITION_TO_OFF' WHEN 3 THEN 'IN_TRANSITION_TO_ON'
                         ELSE 'OFF' END AS VARCHAR) AS snapshot_isolation_state_desc,
                    CAST(read_committed_snapshot AS BOOLEAN) AS is_read_committed_snapshot_on,
                    CAST(3 AS UTINYINT) AS recovery_model,
                    CAST('SIMPLE' AS VARCHAR) AS recovery_model_desc
             FROM (
                 SELECT '{MASTER}' AS name,{MASTER_ID} AS database_id,
                        CAST(TIMESTAMP '2003-04-08 09:13:36.39' AS TIMESTAMP) AS create_date,
                        0 AS user_access,false AS read_committed_snapshot,
                        1 AS snapshot_isolation_state
                 UNION ALL
                 SELECT r.name,r.database_id,r.create_date,
                        coalesce(r.user_access,0),coalesce(r.read_committed_snapshot,false),
                        coalesce(r.snapshot_isolation_state,0)
                 FROM {registry} r JOIN duckdb_databases() d ON d.database_name=r.name
                 WHERE r.published
             );
             CREATE OR REPLACE VIEW {target}.sys.dm_exec_sessions AS
             SELECT * FROM __msduck_sessions();
             CREATE OR REPLACE VIEW {target}.sys.server_principals AS
             SELECT CAST(name AS VARCHAR) AS name,
                    CAST(principal_id AS INTEGER) AS principal_id,
                    CASE WHEN principal_id=1 THEN CAST('\x01' AS BLOB)
                         ELSE unhex(md5(lower(name))) END AS sid,
                    CAST('S' AS VARCHAR) AS type,
                    CAST('SQL_LOGIN' AS VARCHAR) AS type_desc,
                    false AS is_disabled,
                    CAST(create_date AS TIMESTAMP) AS create_date,
                    CAST(create_date AS TIMESTAMP) AS modify_date,
                    CAST('{MASTER}' AS VARCHAR) AS default_database_name,
                    CAST('us_english' AS VARCHAR) AS default_language_name,
                    CAST(NULL AS INTEGER) AS credential_id,
                    CAST(NULL AS INTEGER) AS owning_principal_id,
                    false AS is_fixed_role,
                    CAST(NULL AS UUID) AS tenant_id
             FROM (
                 SELECT 'sa' AS name,1 AS principal_id,
                        TIMESTAMP '2003-04-08 09:10:35.46' AS create_date
                 UNION ALL
                 SELECT name,principal_id,create_date FROM __msduck_server_principals()
             );
             CREATE OR REPLACE MACRO {target}.main.__msduck_login_name(value) AS
                 (SELECT p.name FROM (SELECT value AS v) a, {target}.sys.server_principals p
                  WHERE p.sid=a.v);
             CREATE OR REPLACE MACRO {target}.main.__msduck_db_key(value) AS
                 translate(rtrim(CAST(value AS VARCHAR), ' '), {upper}, {lower});
             CREATE OR REPLACE MACRO {target}.main.__msduck_db_id(value) AS
                 (SELECT d.database_id
                  FROM (SELECT {target}.main.__msduck_db_key(value) AS k) a,
                       {target}.sys.databases d
                  WHERE {target}.main.__msduck_db_key(d.name)=a.k);
             CREATE OR REPLACE MACRO {target}.main.__msduck_db_name(value) AS
                 (SELECT d.name FROM (SELECT value AS v) a, {target}.sys.databases d
                  WHERE d.database_id=a.v);
             CREATE OR REPLACE MACRO {target}.main.__msduck_current_db_name() AS
                 CASE WHEN current_database()={primary} THEN '{MASTER}' ELSE current_database() END",
            registry = self.registry(),
            primary = literal(&self.primary),
            upper = literal(&KEY_MAP.0),
            lower = literal(&KEY_MAP.1),
        ))?;
        Ok(())
    }

    /// `sys.master_files` and `sys.database_files` in one catalog, with
    /// SQL Server's columns. Logical and physical names come from the
    /// registry (see `effective`); sizes are DuckDB's allocated blocks in
    /// 8 KB pages, and logs report no pages.
    fn publish_files(&self, db: &Connection, alias: &str) -> Result<()> {
        let target = quote(alias);
        let database_id = if alias == self.primary {
            MASTER_ID
        } else {
            db.query_row(
                &format!("SELECT database_id FROM {} WHERE name=?", self.registry()),
                [alias],
                |row| row.get(0),
            )?
        };
        let directory = format!("{}{}", self.directory.display(), std::path::MAIN_SEPARATOR);
        let (master_data, master_log) = self.primary_paths();
        let size = |catalog: &str| {
            format!(
                "coalesce((SELECT CAST(ceil(s.total_blocks * s.block_size / 8192.0) AS INTEGER)
                           FROM pragma_database_size() s WHERE s.database_name={catalog}),0)"
            )
        };
        db.execute_batch(&format!(
            "CREATE OR REPLACE VIEW {target}.sys.master_files AS
             SELECT CAST(f.database_id AS INTEGER) AS database_id,
                    CAST(f.file_id AS INTEGER) AS file_id,
                    CAST(NULL AS UUID) AS file_guid,
                    CAST(f.file_id - 1 AS UTINYINT) AS type,
                    CAST(CASE f.file_id WHEN 1 THEN 'ROWS' ELSE 'LOG' END AS VARCHAR) AS type_desc,
                    CAST(2 - f.file_id AS INTEGER) AS data_space_id,
                    CAST(f.name AS VARCHAR) AS name,
                    CAST(f.physical_name AS VARCHAR) AS physical_name,
                    CAST(0 AS UTINYINT) AS state,
                    CAST('ONLINE' AS VARCHAR) AS state_desc,
                    CAST(f.size AS INTEGER) AS size,
                    CAST(CASE WHEN f.file_id = 2 AND f.database_id <> {MASTER_ID} THEN 268435456 ELSE -1 END AS INTEGER) AS max_size,
                    CAST(CASE WHEN f.database_id IN ({MASTER_ID},{MSDB_ID}) THEN 10 ELSE 8192 END AS INTEGER) AS growth,
                    false AS is_media_read_only,
                    false AS is_read_only,
                    false AS is_sparse,
                    f.database_id IN ({MASTER_ID},{MSDB_ID}) AS is_percent_growth,
                    false AS is_name_reserved,
                    false AS is_persistent_log_buffer,
                    CAST(NULL AS DECIMAL(25,0)) AS create_lsn,
                    CAST(NULL AS DECIMAL(25,0)) AS drop_lsn,
                    CAST(NULL AS DECIMAL(25,0)) AS read_only_lsn,
                    CAST(NULL AS DECIMAL(25,0)) AS read_write_lsn,
                    CAST(NULL AS DECIMAL(25,0)) AS differential_base_lsn,
                    CAST(NULL AS UUID) AS differential_base_guid,
                    CAST(NULL AS TIMESTAMP) AS differential_base_time,
                    CAST(NULL AS DECIMAL(25,0)) AS redo_start_lsn,
                    CAST(NULL AS UUID) AS redo_start_fork_guid,
                    CAST(NULL AS DECIMAL(25,0)) AS redo_target_lsn,
                    CAST(NULL AS UUID) AS redo_target_fork_guid,
                    CAST(NULL AS DECIMAL(25,0)) AS backup_lsn,
                    CAST(NULL AS INTEGER) AS credential_id
             FROM (
                 SELECT {MASTER_ID} AS database_id,1 AS file_id,'master' AS name,
                        {master_data} AS physical_name,{master_size} AS size
                 UNION ALL
                 SELECT {MASTER_ID},2,'mastlog',{master_log},0
                 UNION ALL
                 SELECT r.database_id,1,
                        coalesce(r.data_name,CASE WHEN r.database_id={MSDB_ID} THEN 'MSDBData' ELSE r.name END),
                        coalesce(r.data_path,{directory}||r.file),
                        {user_size}
                 FROM {registry} r JOIN duckdb_databases() d ON d.database_name=r.name
                 WHERE r.published
                 UNION ALL
                 SELECT r.database_id,2,
                        coalesce(r.log_name,CASE WHEN r.database_id={MSDB_ID} THEN 'MSDBLog' ELSE r.name||'_log' END),
                        coalesce(r.log_path,{directory}||r.file||'.wal'),
                        0
                 FROM {registry} r JOIN duckdb_databases() d ON d.database_name=r.name
                 WHERE r.published
             ) f
             ORDER BY f.database_id, f.file_id;
             CREATE OR REPLACE VIEW {target}.sys.database_files AS
             SELECT file_id,file_guid,type,type_desc,data_space_id,name,physical_name,state,state_desc,
                    size,max_size,growth,is_media_read_only,is_read_only,is_sparse,is_percent_growth,
                    is_name_reserved,is_persistent_log_buffer,create_lsn,drop_lsn,read_only_lsn,
                    read_write_lsn,differential_base_lsn,differential_base_guid,differential_base_time,
                    redo_start_lsn,redo_start_fork_guid,redo_target_lsn,redo_target_fork_guid,backup_lsn
             FROM {target}.sys.master_files WHERE database_id={database_id};",
            registry = self.registry(),
            master_data = literal(&master_data),
            master_log = literal(&master_log),
            master_size = size(&literal(&self.primary)),
            user_size = size("r.name"),
            directory = literal(&directory),
        ))?;
        Ok(())
    }

    fn set_published(&self, db: &Connection, name_key: &str, published: bool) -> Result<()> {
        db.execute(
            &format!(
                "UPDATE {} SET published=? WHERE name_key=?",
                self.registry()
            ),
            duckdb::params![published, name_key],
        )?;
        Ok(())
    }

    fn display(&self, alias: &str) -> String {
        if alias == self.primary {
            MASTER.to_owned()
        } else {
            alias.to_owned()
        }
    }

    fn registry(&self) -> String {
        format!("{}.main.__msduck_databases", quote(&self.primary))
    }

    fn ids(&self) -> String {
        format!("{}.main.__msduck_database_ids", quote(&self.primary))
    }

    /// The registry is ordinary SQL data, so a stored file name is trusted
    /// only if it is a single component generated for this row's ID and name.
    /// Any prefix is accepted, so renaming the primary file keeps its databases.
    fn path(&self, row: &Row) -> Result<PathBuf> {
        let file = row.file.as_str();
        let suffix = file_suffix(row.database_id, &row.name_key);
        let mut components = Path::new(file).components();
        ensure!(
            matches!(components.next(), Some(std::path::Component::Normal(_)))
                && components.next().is_none()
                && !file.contains(['/', '\\'])
                && file.len() > suffix.len()
                && file.ends_with(&suffix),
            "invalid registered database file name '{file}'"
        );
        Ok(self.directory.join(file))
    }

    /// Delete a database's WAL and file. After a checkpointing detach the WAL
    /// goes first, so a failure leaves a complete file. Without one, the WAL
    /// may hold committed changes, so the file goes first and a failure never
    /// leaves an older file whose changes were discarded.
    fn delete_files(&self, row: &Row, checkpointed: bool) -> Result<()> {
        let path = self.path(row)?;
        let wal = wal(&path);
        let remove = |path: &Path| match std::fs::remove_file(path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                Err(error).with_context(|| format!("delete {}", path.display()))
            }
            _ => Ok(()),
        };
        if checkpointed {
            remove(&wal)?;
            remove(&path)
        } else {
            remove(&path)?;
            // Deleting the file commits the drop. A WAL left behind makes
            // the registration stale, and its removal is retried there.
            if let Err(error) = remove(&wal) {
                eprintln!("msduck: {error:#}");
            }
            Ok(())
        }
    }
}

/// The files of a database as BACKUP records them: logical and physical
/// names and the recovery family. Physical names are the names SQL Server
/// would report; msduck stores the data in its own DuckDB file.
#[derive(Clone, Debug)]
pub struct Files {
    pub database_id: i32,
    /// The SQL Server name.
    pub name: String,
    /// The DuckDB catalog that stores the database.
    pub alias: String,
    pub data_name: String,
    pub log_name: String,
    pub data_path: String,
    pub log_path: String,
    pub family_guid: String,
    pub database_guid: String,
    /// Microseconds since the Unix epoch.
    pub create_date: i64,
}

/// A database restored from a backup: the staged DuckDB file and the
/// metadata it keeps.
#[derive(Clone, Copy, Debug)]
pub struct Restore<'a> {
    pub name: &'a str,
    /// A DuckDB file in the catalog directory (see `staging_path`). It is
    /// moved into place, or left for the caller to delete on failure.
    pub staging: &'a Path,
    pub data_name: &'a str,
    pub log_name: &'a str,
    pub data_path: &'a str,
    pub log_path: &'a str,
    pub family_guid: &'a str,
    pub database_guid: &'a str,
    /// WITH REPLACE: overwrite an existing database of another family.
    pub replace: bool,
}

/// Several SQL Server errors reported together, in order.
#[derive(Debug)]
pub struct Diagnostics(pub Vec<SqlError>);

impl std::fmt::Display for Diagnostics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0.last() {
            Some(error) => f.write_str(&error.message),
            None => Ok(()),
        }
    }
}

impl std::error::Error for Diagnostics {}

/// One registered database's file metadata, before defaults apply.
struct FileRow {
    name: String,
    database_id: i32,
    file: String,
    data_name: Option<String>,
    log_name: Option<String>,
    data_path: Option<String>,
    log_path: Option<String>,
}

impl Catalog {
    /// The files of the published database `name`, giving it a recovery
    /// family and database GUID on first use.
    pub fn files(&self, db: &Connection, name: &str) -> Result<Option<Files>> {
        let _change = self.lock();
        let Some(alias) = self.resolve_published(db, name)? else {
            return Ok(None);
        };
        if alias == self.primary {
            let (data_path, log_path) = self.primary_paths();
            let create_date: i64 = db.query_row(
                "SELECT epoch_us(TIMESTAMP '2003-04-08 09:13:36.39')",
                [],
                |row| row.get(0),
            )?;
            return Ok(Some(Files {
                database_id: MASTER_ID,
                name: MASTER.into(),
                alias,
                data_name: "master".into(),
                log_name: "mastlog".into(),
                data_path,
                log_path,
                family_guid: self.master_family.clone(),
                database_guid: self.master_family.clone(),
                create_date,
            }));
        }
        db.execute(
            &format!(
                "UPDATE {} SET family_guid=coalesce(family_guid,upper(CAST(uuid() AS VARCHAR))),
                     database_guid=coalesce(database_guid,upper(CAST(uuid() AS VARCHAR)))
                 WHERE name=? AND (family_guid IS NULL OR database_guid IS NULL)",
                self.registry()
            ),
            [&alias],
        )?;
        let (row, family_guid, database_guid, create_date) = db.query_row(
            &format!(
                "SELECT name,database_id,file,data_name,log_name,data_path,log_path,
                        family_guid,database_guid,epoch_us(create_date)
                 FROM {} WHERE name=?",
                self.registry()
            ),
            [&alias],
            |row| {
                Ok((
                    FileRow::read(row)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, i64>(9)?,
                ))
            },
        )?;
        let (data_name, log_name, data_path, log_path) = self.effective(&row);
        Ok(Some(Files {
            database_id: row.database_id,
            name: alias.clone(),
            alias,
            data_name,
            log_name,
            data_path,
            log_path,
            family_guid,
            database_guid,
            create_date,
        }))
    }

    /// `master`'s data and log file names.
    fn primary_paths(&self) -> (String, String) {
        match &self.primary_path {
            Some(path) => {
                let path = path.display().to_string();
                (path.clone(), format!("{path}.wal"))
            }
            None => (":memory:".into(), ":memory:".into()),
        }
    }

    /// Logical and physical names with their defaults: `<name>` and
    /// `<name>_log` (`MSDBData` and `MSDBLog` for msdb), and the database's
    /// DuckDB file and its WAL.
    fn effective(&self, row: &FileRow) -> (String, String, String, String) {
        let (data_default, log_default) = if row.database_id == MSDB_ID {
            ("MSDBData".to_owned(), "MSDBLog".to_owned())
        } else {
            (row.name.clone(), format!("{}_log", row.name))
        };
        let storage = self.directory.join(&row.file).display().to_string();
        (
            row.data_name.clone().unwrap_or(data_default),
            row.log_name.clone().unwrap_or(log_default),
            row.data_path.clone().unwrap_or_else(|| storage.clone()),
            row.log_path.clone().unwrap_or(format!("{storage}.wal")),
        )
    }

    fn file_rows(&self, db: &Connection) -> Result<Vec<FileRow>> {
        let mut query = db.prepare(&format!(
            "SELECT r.name,r.database_id,r.file,r.data_name,r.log_name,r.data_path,r.log_path
             FROM {} r JOIN duckdb_databases() d ON d.database_name=r.name
             WHERE r.published ORDER BY r.database_id",
            self.registry()
        ))?;
        let rows = query
            .query_map([], FileRow::read)?
            .collect::<duckdb::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// A fresh path in the catalog directory for a DuckDB file that BACKUP
    /// or RESTORE writes before moving or deleting it. Nothing exists there.
    pub fn staging_path(&self, purpose: &str) -> Result<PathBuf> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        for _ in 0..16 {
            let path = self.directory.join(format!(
                ".{}.{purpose}-{}-{nanos}-{}.duckdb",
                self.prefix,
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            if !occupied(&path)? && !occupied(&wal(&path))? {
                return Ok(path);
            }
        }
        bail!("could not choose a unique {purpose} file name")
    }

    /// Create `msdb` with SQL Server's database ID if it does not exist yet.
    /// Returns whether it was created. Like CREATE DATABASE, this must run
    /// outside a transaction.
    pub fn ensure_system(&self, db: &Connection, name: &str) -> Result<bool> {
        ensure!(key(name) == MSDB, "unknown system database {name}");
        let _change = self.lock();
        if self.resolve_published(db, MSDB)?.is_some() {
            return Ok(false);
        }
        self.create_locked(db, MSDB, Some(MSDB_ID), None)?;
        Ok(true)
    }

    /// RESTORE DATABASE: create the database from a staged file, or replace
    /// an existing one in place, keeping its ID. Errors follow SQL Server:
    /// 3154 for a database of another family without REPLACE, 1834/3156/3119
    /// for physical names that belong to another database, 3102 when the
    /// restoring session uses the database and 3101 when others do. `own`
    /// counts the restoring session's holds of a catalog alias, and
    /// `current` is its current catalog alias.
    pub fn restore(
        &self,
        db: &Connection,
        restore: &Restore<'_>,
        own: &dyn Fn(&str) -> usize,
        current: &str,
    ) -> Result<Database> {
        let name = restore.name;
        validate(name)?;
        let name_key = key(name);
        ensure!(
            !RESERVED.contains(&name_key.as_str()),
            "RESTORE over the system database '{name}' is not supported by msduck"
        );
        let _change = self.lock();
        let mut existing = self.lookup(db, &name_key)?;
        if let Some((row, attached)) = &existing
            && self.forget_stale(db, row, *attached)?
        {
            existing = None;
        }
        if let Some((row, attached)) = &existing {
            ensure!(
                row.published && *attached,
                "database '{}' is unavailable; DROP it before restoring over it",
                row.name
            );
            if current == row.name {
                bail!(SqlError::new(
                    3102,
                    1,
                    format!(
                        "RESTORE cannot process database '{}' because it is in use by this session. It is recommended that the master database be used when performing this operation.",
                        row.name
                    )
                ));
            }
            if self.users(&row.name) > own(&row.name) || self.in_transition(&row.name) {
                bail!(SqlError::new(
                    3101,
                    1,
                    "Exclusive access could not be obtained because the database is in use."
                ));
            }
            let family: Option<String> = db.query_row(
                &format!(
                    "SELECT family_guid FROM {} WHERE name_key=?",
                    self.registry()
                ),
                [&name_key],
                |row| row.get(0),
            )?;
            if !restore.replace
                && !family
                    .as_deref()
                    .is_some_and(|family| family.eq_ignore_ascii_case(restore.family_guid))
            {
                bail!(SqlError::new(
                    3154,
                    4,
                    format!(
                        "The backup set holds a backup of a database other than the existing '{}' database.",
                        row.name
                    )
                ));
            }
        }
        // A physical name another database uses cannot be overwritten.
        let target = existing.as_ref().map(|(row, _)| row.name.clone());
        let mut owners = vec![(
            MASTER.to_owned(),
            self.primary_paths().0,
            self.primary_paths().1,
        )];
        for row in self.file_rows(db)? {
            if Some(&row.name) == target.as_ref() {
                continue;
            }
            let (_, _, data_path, log_path) = self.effective(&row);
            owners.push((row.name.clone(), data_path, log_path));
        }
        let mut errors = Vec::new();
        for (logical, physical) in [
            (restore.data_name, restore.data_path),
            (restore.log_name, restore.log_path),
        ] {
            if let Some((owner, _, _)) = owners
                .iter()
                .find(|(_, data, log)| data == physical || log == physical)
            {
                errors.push(SqlError::new(
                    1834,
                    1,
                    format!(
                        "The file '{physical}' cannot be overwritten.  It is being used by database '{owner}'."
                    ),
                ));
                errors.push(SqlError::new(
                    3156,
                    4,
                    format!(
                        "File '{logical}' cannot be restored to '{physical}'. Use WITH MOVE to identify a valid location for the file."
                    ),
                ));
            }
        }
        if !errors.is_empty() {
            errors.push(SqlError::new(
                3119,
                1,
                "Problems were identified while planning for the RESTORE statement. Previous messages provide details.",
            ));
            bail!(Diagnostics(errors));
        }
        let Some((row, _)) = existing else {
            return self.create_locked(db, name, None, Some(restore));
        };
        let path = self.path(&row)?;
        deletable(&path)?;
        // Open and bootstrap the staged file first, so a backup this build
        // cannot open never costs the existing database.
        bootstrap_file(restore.staging)?;
        // Hide the database, detach it and replace its file. A failure
        // after the old file is gone leaves the database hidden, as SQL
        // Server leaves a failed restore in the RESTORING state; DROP can
        // remove it.
        self.set_published(db, &name_key, false)?;
        db.execute_batch(&format!("DETACH DATABASE {}", quote(&row.name)))
            .inspect_err(|_| {
                let _ = self.set_published(db, &name_key, true);
            })?;
        self.delete_files(&row, true)?;
        move_file(restore.staging, &path)?;
        db.execute(
            &format!(
                "UPDATE {} SET family_guid=?,database_guid=?,data_name=?,log_name=?,data_path=?,log_path=?
                 WHERE name_key=?",
                self.registry()
            ),
            duckdb::params![
                restore.family_guid,
                restore.database_guid,
                restore.data_name,
                restore.log_name,
                restore.data_path,
                restore.log_path,
                name_key
            ],
        )?;
        let result = self
            .attach(db, &row, Attach::Recover)
            .and_then(|()| self.publish_all(db))
            .and_then(|()| self.set_published(db, &name_key, true));
        if let Err(error) = result {
            let _ = db.execute_batch(&format!("DETACH DATABASE IF EXISTS {}", quote(&row.name)));
            return Err(error);
        }
        Ok(Database {
            name: row.name,
            database_id: row.database_id,
        })
    }
}

impl FileRow {
    fn read(row: &duckdb::Row) -> duckdb::Result<Self> {
        Ok(Self {
            name: row.get(0)?,
            database_id: row.get(1)?,
            file: row.get(2)?,
            data_name: row.get(3)?,
            log_name: row.get(4)?,
            data_path: row.get(5)?,
            log_path: row.get(6)?,
        })
    }
}

/// A random (version 4) GUID in upper case, as SQL Server displays them.
pub fn new_guid() -> String {
    use ring::rand::SecureRandom;
    let mut bytes = [0u8; 16];
    // The system generator only fails when the OS has no entropy source.
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .expect("system random number generator");
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(bytes).to_string().to_uppercase()
}

/// Move a staged file into place; both are in the catalog directory.
fn move_file(from: &Path, to: &Path) -> Result<()> {
    ensure!(
        !occupied(to)? && !occupied(&wal(to))?,
        "'{}' already exists",
        to.display()
    );
    std::fs::rename(from, to)
        .with_context(|| format!("move {} to {}", from.display(), to.display()))
}

fn validate(name: &str) -> Result<()> {
    ensure!(!name.is_empty(), "database name cannot be empty");
    // SQL Server measures sysname in UTF-16 code units.
    if name.encode_utf16().count() > MAX_NAME {
        let mut units = 0;
        let prefix: String = name
            .chars()
            .take_while(|c| {
                units += c.len_utf16();
                units <= MAX_NAME
            })
            .collect();
        bail!("The identifier that starts with '{prefix}' is too long. Maximum length is 128.");
    }
    ensure!(
        !name.chars().any(char::is_control),
        "database name contains a control character"
    );
    Ok(())
}

/// SQL Server compares database names case-insensitively under the default
/// server collation; the registry keys on that collation's captured
/// lower-case mapping of UTF-16 units, not on generic Unicode casing.
fn key(name: &str) -> String {
    // Comparisons under the collation ignore trailing spaces.
    let mut units: Vec<u16> = name.trim_end_matches(' ').encode_utf16().collect();
    Family::SqlLatin1.map_in_place(Direction::Lower, &mut units);
    // The mapping leaves surrogates unchanged, so the units stay valid.
    String::from_utf16_lossy(&units)
}

/// `key` as the two argument strings of SQL `translate`: every BMP character
/// the captured mapping changes, and its lower-case form at the same position.
/// Surrogates, and so supplementary characters, map to themselves.
static KEY_MAP: std::sync::LazyLock<(String, String)> = std::sync::LazyLock::new(|| {
    (0..=u16::MAX)
        .filter_map(|unit| {
            let lower = Family::SqlLatin1.map_unit(Direction::Lower, unit);
            Some((char::from_u32(unit.into())?, char::from_u32(lower.into())?))
        })
        .filter(|(unit, lower)| unit != lower)
        .unzip()
});

/// Whether any directory entry, including a link, has this name.
fn occupied(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("inspect {}", path.display())),
    }
}

/// Whether a database file exists, without following a symbolic link. Only
/// regular files are accepted.
fn present(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            ensure!(
                metadata.file_type().is_file(),
                "database file '{}' is not a regular file",
                path.display()
            );
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("inspect {}", path.display())),
    }
}

/// Whether files next to `path` can be created and removed. Each probe has a
/// fresh name, so one left by an interrupted DROP blocks nothing; process IDs
/// repeat across container restarts, so the name also carries the clock.
fn deletable(path: &Path) -> Result<()> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    for _ in 0..16 {
        let mut probe = path.to_path_buf().into_os_string();
        probe.push(format!(
            ".drop-{}-{nanos}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let probe = PathBuf::from(probe);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe)
        {
            Ok(_) => {
                return std::fs::remove_file(&probe)
                    .with_context(|| format!("delete {}", probe.display()));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("cannot delete files in {}", path.display()));
            }
        }
    }
    bail!(
        "could not create a unique deletion probe for {}",
        path.display()
    )
}

fn wal(path: &Path) -> PathBuf {
    let mut wal = path.to_path_buf().into_os_string();
    wal.push(".wal");
    PathBuf::from(wal)
}

/// Longest encoded name kept in a file name. With the prefix, the ID and the
/// suffix, the component stays well inside common 255-byte limits.
const FILE_NAME_FRAGMENT: usize = 64;

/// The database ID makes the name unique; a bounded, readable fragment of the
/// encoded name follows it.
fn file_name(prefix: &str, database_id: i32, name_key: &str) -> String {
    // The prefix is already a file name, but a Unix name may contain `\`,
    // which registered names reject for portability. Its length also needs a
    // bound so that the whole component fits. A shortened prefix keeps a hash
    // of the full prefix, so servers whose file names share a start do not
    // collide.
    // Escaping `%` first keeps the mapping injective.
    let escaped = prefix.replace('%', "%25").replace('\\', "%5C");
    let prefix = escaped.as_str();
    let prefix = if prefix.len() > FILE_NAME_FRAGMENT {
        let mut end = FILE_NAME_FRAGMENT;
        while !prefix.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}~{:016x}", &prefix[..end], fnv1a(prefix.as_bytes()))
    } else {
        prefix.to_owned()
    };
    format!("{prefix}{}", file_suffix(database_id, name_key))
}

/// The part of a file name that identifies its database: the unique ID and a
/// bounded fragment of the encoded name.
fn file_suffix(database_id: i32, name_key: &str) -> String {
    let mut fragment = String::new();
    for c in name_key.chars() {
        let encoded = encode(c.encode_utf8(&mut [0; 4]));
        if fragment.len() + encoded.len() > FILE_NAME_FRAGMENT {
            break;
        }
        fragment.push_str(&encoded);
    }
    format!(".{database_id}.{fragment}.duckdb")
}

/// Create a fresh directory for an in-memory server's user databases. The
/// process-wide counter keeps names unique within a process even when the
/// clock repeats, and `create_dir` refuses a directory that already exists.
fn temporary_directory() -> Result<PathBuf> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let base = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    for _ in 0..16 {
        let directory = base.join(format!(
            "msduck-databases-{}-{nanos}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let mut builder = std::fs::DirBuilder::new();
        // Only the server's account may read an in-memory server's files.
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        match builder.create(&directory) {
            Ok(()) => return Ok(directory),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error).context("create database directory"),
        }
    }
    bail!("could not create a unique database directory")
}

/// 64-bit FNV-1a: stable across Rust releases, unlike `DefaultHasher`.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// A portable file-name fragment: ASCII letters, digits, `_` and `-` are kept,
/// every other UTF-8 byte is written as `%XX`.
fn encode(name: &str) -> String {
    name.bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-' {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

fn quote(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn exists(name: &str) -> anyhow::Error {
    SqlError::new(
        1801,
        3,
        format!("Database '{name}' already exists. Choose a different database name."),
    )
    .into()
}

fn missing(name: &str) -> anyhow::Error {
    SqlError::new(
        911,
        1,
        format!("Database '{name}' does not exist. Make sure that the name is entered correctly."),
    )
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_are_portable() {
        assert_eq!(encode("sales_2026-q3"), "sales_2026-q3");
        assert_eq!(encode("a b/c.é"), "a%20b%2Fc%2E%C3%A9");
    }

    #[test]
    fn file_names_are_bounded_and_unique_by_id() {
        assert_eq!(file_name("msduck", 5, "my app"), "msduck.5.my%20app.duckdb");
        let long = file_name("msduck", 32767, &"é".repeat(128));
        // Escapes are never split; the fragment stops at a whole character.
        assert_eq!(long, format!("msduck.32767.{}.duckdb", "%C3%A9".repeat(10)));
        assert!(long.len() < 100);
        let prefix = "s".repeat(246);
        let bounded = file_name(&prefix, 32767, &"é".repeat(128));
        assert!(bounded.starts_with(&format!("{}~", "s".repeat(64))));
        assert!(bounded.len() < 180, "{}", bounded.len());
        assert!(file_name(&"é".repeat(40), 5, "x").starts_with(&format!("{}~", "é".repeat(32))));
        // Prefixes sharing their first 64 bytes still produce different names.
        let a = file_name(&format!("{}a", "s".repeat(64)), 5, "sales");
        let b = file_name(&format!("{}b", "s".repeat(64)), 5, "sales");
        assert_ne!(a, b);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
    }

    #[test]
    fn generated_names_pass_their_own_validation() {
        assert_eq!(file_name("a\\b.duckdb", 5, "x"), "a%5Cb.duckdb.5.x.duckdb");
        assert_eq!(
            file_name("a%5Cb.duckdb", 5, "x"),
            "a%255Cb.duckdb.5.x.duckdb"
        );
    }

    #[test]
    fn keys_follow_the_server_collation() {
        assert_eq!(key("Sales"), "sales");
        assert_eq!(key("Sales  "), "sales");
        assert_eq!(key(" sales"), " sales");
        // Generic Unicode lowercases İ to i and a combining dot.
        assert_eq!(key("İ"), "i");
        assert_eq!(key("ΣẞıI🦆"), "σẞıi🦆");
    }

    #[test]
    fn the_sql_key_map_matches_key() {
        let (upper, lower) = &*KEY_MAP;
        assert_eq!(upper.chars().count(), lower.chars().count());
        let translated: String = "İKΣ"
            .chars()
            .map(|c| {
                upper
                    .chars()
                    .position(|u| u == c)
                    .map_or(c, |i| lower.chars().nth(i).unwrap())
            })
            .collect();
        assert_eq!(translated, key("İKΣ"));
    }

    #[test]
    fn temporary_directories_are_distinct() {
        let a = temporary_directory().unwrap();
        let b = temporary_directory().unwrap();
        assert_ne!(a, b);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&a).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }
        std::fs::remove_dir(a).unwrap();
        std::fs::remove_dir(b).unwrap();
    }

    #[test]
    fn names_are_validated() {
        assert!(validate("x").is_ok());
        assert!(validate("").is_err());
        assert!(validate(&"x".repeat(129)).is_err());
        // Supplementary characters take two UTF-16 units each.
        assert!(validate(&"🦆".repeat(64)).is_ok());
        let error = validate(&"🦆".repeat(65)).unwrap_err().to_string();
        assert_eq!(
            error,
            format!(
                "The identifier that starts with '{}' is too long. Maximum length is 128.",
                "🦆".repeat(64)
            )
        );
        assert!(validate("a\u{0}b").is_err());
    }
}
