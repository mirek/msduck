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
use msduck_core::diagnostic::SqlError;
use std::path::{Path, PathBuf};

pub const MASTER: &str = "master";
pub const MASTER_ID: i32 = 1;
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
}

impl Drop for Catalog {
    fn drop(&mut self) {
        if self.temporary {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }
}

/// One registry row. Its values are ordinary SQL data.
struct Row {
    name: String,
    name_key: String,
    database_id: i32,
    file: String,
}

impl Row {
    /// Reads `name, name_key, database_id, file` from the first four columns.
    fn read(row: &duckdb::Row) -> duckdb::Result<Self> {
        Ok(Self {
            name: row.get(0)?,
            name_key: row.get(1)?,
            database_id: row.get(2)?,
            file: row.get(3)?,
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
            }
        } else {
            let file = Path::new(path);
            Self {
                primary,
                directory: file.parent().map(Path::to_path_buf).unwrap_or_default(),
                prefix: file
                    .file_name()
                    .and_then(|name| name.to_str())
                    .context("database path needs a UTF-8 file name")?
                    .to_owned(),
                temporary: false,
                changes: std::sync::Mutex::new(()),
            }
        };
        owner.execute_batch(&format!(
            "CREATE TABLE IF NOT EXISTS {registry}(
                name_key VARCHAR PRIMARY KEY,
                name VARCHAR NOT NULL,
                database_id INTEGER UNIQUE NOT NULL,
                file VARCHAR NOT NULL,
                create_date TIMESTAMP NOT NULL);
             CREATE SEQUENCE IF NOT EXISTS {ids} START {FIRST_USER_ID} MAXVALUE 32767 NO CYCLE",
            registry = catalog.registry(),
            ids = catalog.ids(),
        ))?;
        let registered = {
            let mut query = owner.prepare(&format!(
                "SELECT name,name_key,database_id,file FROM {} ORDER BY database_id",
                catalog.registry()
            ))?;
            query
                .query_map([], Row::read)?
                .collect::<duckdb::Result<Vec<_>>>()?
        };
        for row in registered {
            let name = row.name.clone();
            let result = catalog
                .attach(owner, &row, Attach::Recover)
                .and_then(|()| catalog.publish(owner, &name));
            if let Err(error) = result {
                // SQL Server keeps serving other databases when one cannot be
                // recovered; the database stays registered but is not listed.
                // Listing follows attachment, so detach a partial recovery.
                let _ = owner.execute_batch(&format!("DETACH DATABASE IF EXISTS {}", quote(&name)));
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
    pub fn resolve(&self, db: &Connection, name: &str) -> Result<Option<String>> {
        if name.eq_ignore_ascii_case(MASTER) {
            return Ok(Some(self.primary.clone()));
        }
        let alias = db
            .query_row(
                &format!(
                    "SELECT r.name FROM {} r JOIN duckdb_databases() d ON d.database_name=r.name WHERE r.name_key=?",
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

    /// Whether a name is registered, including a database that failed to attach.
    fn registered(&self, db: &Connection, name_key: &str) -> Result<bool> {
        Ok(db.query_row(
            &format!(
                "SELECT count(*) > 0 FROM {} WHERE name_key=?",
                self.registry()
            ),
            [name_key],
            |row| row.get(0),
        )?)
    }

    /// Make `name` the connection's current database with the `dbo` schema.
    /// Returns the database's SQL Server name.
    pub fn select(&self, db: &Connection, name: &str) -> Result<String> {
        let alias = self.resolve(db, name)?.ok_or_else(|| missing(name))?;
        db.execute_batch(&format!("USE {}; SET schema = 'dbo'", quote(&alias)))?;
        Ok(self.display(&alias))
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
        let _change = self
            .changes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let name_key = key(name);
        if RESERVED.contains(&name_key.as_str()) || self.registered(db, &name_key)? {
            return Err(exists(name));
        }
        ensure!(
            !BACKEND_RESERVED.contains(&name_key.as_str()) && name_key != key(&self.primary),
            "database name '{name}' is reserved by msduck"
        );
        let database_id: i32 = db.query_row(
            &format!("SELECT CAST(nextval({}) AS INTEGER)", literal(&self.ids())),
            [],
            |row| row.get(0),
        )?;
        let file = file_name(&self.prefix, database_id, &name_key);
        let path = self.directory.join(&file);
        ensure!(
            !path.exists(),
            "Cannot create database '{name}' because file '{}' already exists.",
            path.display()
        );
        // The primary key serializes concurrent creators before any attach.
        let inserted = db.execute(
            &format!(
                "INSERT INTO {}(name_key,name,database_id,file,create_date)
                 VALUES (?,?,?,?,CAST(now() AS TIMESTAMP))
                 ON CONFLICT DO NOTHING",
                self.registry()
            ),
            duckdb::params![name_key, name, database_id, file],
        )?;
        if inserted == 0 {
            return Err(exists(name));
        }
        let row = Row {
            name: name.to_owned(),
            name_key: name_key.clone(),
            database_id,
            file,
        };
        let attached = self
            .attach(db, &row, Attach::Create)
            .and_then(|()| self.publish_all(db));
        if let Err(error) = attached {
            let _ = db.execute_batch(&format!("DETACH DATABASE IF EXISTS {}", quote(name)));
            // Forget the database only once its files are gone. Otherwise it
            // stays registered but unavailable, and DROP can finish the cleanup.
            match self.delete_files(&row) {
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
        let _change = self
            .changes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let name_key = key(name);
        if name_key == MASTER {
            bail!(SqlError::new(
                3708,
                1,
                "Cannot drop the database 'master' because it is a system database."
            ));
        }
        // A registered database that failed to attach can still be dropped.
        let registered = db
            .query_row(
                &format!(
                    "SELECT r.name,r.name_key,r.database_id,r.file,d.database_name IS NOT NULL \
                     FROM {} r LEFT JOIN duckdb_databases() d ON d.database_name=r.name \
                     WHERE r.name_key=?",
                    self.registry()
                ),
                [&name_key],
                |row| Ok((Row::read(row)?, row.get::<_, bool>(4)?)),
            )
            .map(Some)
            .or_else(|error| match error {
                duckdb::Error::QueryReturnedNoRows => Ok(None),
                error => Err(error),
            })?;
        let Some((row, attached)) = registered else {
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
        // Validate the stored file name before detaching anything.
        self.path(&row)?;
        if attached {
            db.execute_batch(&format!("DETACH DATABASE {}", quote(&row.name)))?;
        }
        // Keep the registration until the files are gone, so a failed
        // deletion leaves a database that another DROP can finish removing.
        self.delete_files(&row)?;
        db.execute(
            &format!("DELETE FROM {} WHERE name_key=?", self.registry()),
            [&name_key],
        )?;
        Ok(())
    }

    fn attach(&self, db: &Connection, row: &Row, mode: Attach) -> Result<()> {
        let name = row.name.as_str();
        let path = self.path(row)?;
        // DuckDB creates missing files; recovery must not replace lost data
        // with an empty database.
        if mode == Attach::Recover {
            ensure!(
                path.is_file(),
                "database file '{}' is missing",
                path.display()
            );
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
        db.execute_batch(&format!(
            "CREATE OR REPLACE VIEW {target}.sys.databases AS
             SELECT CAST(name AS VARCHAR) AS name,
                    CAST(database_id AS INTEGER) AS database_id,
                    CAST(NULL AS INTEGER) AS source_database_id,
                    CAST(create_date AS TIMESTAMP) AS create_date,
                    CAST(160 AS UTINYINT) AS compatibility_level,
                    CAST('{COLLATION}' AS VARCHAR) AS collation_name,
                    CAST(0 AS UTINYINT) AS user_access,
                    CAST('MULTI_USER' AS VARCHAR) AS user_access_desc,
                    false AS is_read_only,
                    CAST(0 AS UTINYINT) AS state,
                    CAST('ONLINE' AS VARCHAR) AS state_desc,
                    CAST(3 AS UTINYINT) AS recovery_model,
                    CAST('SIMPLE' AS VARCHAR) AS recovery_model_desc
             FROM (
                 SELECT '{MASTER}' AS name,{MASTER_ID} AS database_id,
                        CAST(TIMESTAMP '2003-04-08 09:13:36.39' AS TIMESTAMP) AS create_date
                 UNION ALL
                 SELECT r.name,r.database_id,r.create_date
                 FROM {registry} r JOIN duckdb_databases() d ON d.database_name=r.name
             );
             CREATE OR REPLACE MACRO {target}.main.__msduck_db_id(value) AS
                 (SELECT database_id FROM {target}.sys.databases WHERE lower(name)=lower(CAST(value AS VARCHAR)));
             CREATE OR REPLACE MACRO {target}.main.__msduck_db_name(value) AS
                 (SELECT name FROM {target}.sys.databases WHERE database_id=value);
             CREATE OR REPLACE MACRO {target}.main.__msduck_current_db_name() AS
                 CASE WHEN current_database()={primary} THEN '{MASTER}' ELSE current_database() END",
            registry = self.registry(),
            primary = literal(&self.primary),
        ))?;
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

    fn delete_files(&self, row: &Row) -> Result<()> {
        let path = self.path(row)?;
        let mut wal = path.clone().into_os_string();
        wal.push(".wal");
        for path in [path, PathBuf::from(wal)] {
            match std::fs::remove_file(&path) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    return Err(error).with_context(|| format!("delete {}", path.display()));
                }
                _ => {}
            }
        }
        Ok(())
    }
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
/// server collation; the registry keys on the lower-case form.
fn key(name: &str) -> String {
    name.to_lowercase()
}

/// Longest encoded name kept in a file name. With the prefix, the ID and the
/// suffix, the component stays well inside common 255-byte limits.
const FILE_NAME_FRAGMENT: usize = 64;

/// The database ID makes the name unique; a bounded, readable fragment of the
/// encoded name follows it.
fn file_name(prefix: &str, database_id: i32, name_key: &str) -> String {
    // The prefix is already a valid file name; only its length needs a bound
    // so that the whole component fits. A shortened prefix keeps a hash of the
    // full prefix, so servers whose file names share a start do not collide.
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
