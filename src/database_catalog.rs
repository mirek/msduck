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
const BACKEND_RESERVED: [&str; 3] = ["memory", "system", "temp"];
const COLLATION: &str = "SQL_Latin1_General_CP1_CI_AS";
const MAX_NAME: usize = 128;

/// Where the primary catalog lives and how user databases are stored.
#[derive(Debug)]
pub struct Catalog {
    primary: String,
    /// User database files are `<stem>.<encoded name>.duckdb` in this directory.
    directory: PathBuf,
    stem: String,
    /// The directory belongs to an in-memory server and is removed with it.
    temporary: bool,
}

impl Drop for Catalog {
    fn drop(&mut self) {
        if self.temporary {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }
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
            let directory = std::env::temp_dir().join(format!(
                "msduck-databases-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_nanos()
            ));
            std::fs::create_dir_all(&directory).context("create database directory")?;
            Self {
                primary,
                directory,
                stem: "msduck".into(),
                temporary: true,
            }
        } else {
            let file = Path::new(path);
            Self {
                primary,
                directory: file.parent().map(Path::to_path_buf).unwrap_or_default(),
                stem: file
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .context("database path needs a UTF-8 file name")?
                    .to_owned(),
                temporary: false,
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
                "SELECT name,file FROM {} ORDER BY database_id",
                catalog.registry()
            ))?;
            query
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<duckdb::Result<Vec<_>>>()?
        };
        for (name, file) in registered {
            let result = catalog
                .attach(owner, &name, &file)
                .and_then(|()| catalog.publish(owner, &name));
            if let Err(error) = result {
                // SQL Server keeps serving other databases when one cannot be
                // recovered; the database stays registered but is not listed.
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
        let name_key = key(name);
        if RESERVED.contains(&name_key.as_str()) || self.resolve(db, name)?.is_some() {
            return Err(exists(name));
        }
        ensure!(
            !BACKEND_RESERVED.contains(&name_key.as_str()) && name_key != key(&self.primary),
            "database name '{name}' is reserved by msduck"
        );
        let file = format!("{}.{}.duckdb", self.stem, encode(&name_key));
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
                 VALUES (?,?,CAST(nextval({}) AS INTEGER),?,CAST(now() AS TIMESTAMP))
                 ON CONFLICT DO NOTHING",
                self.registry(),
                literal(&self.ids())
            ),
            duckdb::params![name_key, name, file],
        )?;
        if inserted == 0 {
            return Err(exists(name));
        }
        let attached = self
            .attach(db, name, &file)
            .and_then(|()| self.publish_all(db));
        if let Err(error) = attached {
            let _ = db.execute_batch(&format!("DETACH DATABASE IF EXISTS {}", quote(name)));
            let _ = db.execute(
                &format!("DELETE FROM {} WHERE name_key=?", self.registry()),
                [&name_key],
            );
            self.remove_files(&file);
            return Err(error);
        }
        let database_id = db.query_row(
            &format!(
                "SELECT database_id FROM {} WHERE name_key=?",
                self.registry()
            ),
            [&name_key],
            |row| row.get(0),
        )?;
        Ok(Database {
            name: name.to_owned(),
            database_id,
        })
    }

    /// Detach a user database and remove its storage. The caller must make
    /// sure no session is using it; the connection must not be using it.
    pub fn remove(&self, db: &Connection, name: &str) -> Result<()> {
        let name_key = key(name);
        if name_key == MASTER {
            bail!(SqlError::new(
                3708,
                1,
                "Cannot drop the database 'master' because it is a system database."
            ));
        }
        let Some(alias) = self.resolve(db, name)? else {
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
        let file: String = db.query_row(
            &format!("SELECT file FROM {} WHERE name_key=?", self.registry()),
            [&name_key],
            |row| row.get(0),
        )?;
        db.execute_batch(&format!("DETACH DATABASE {}", quote(&alias)))?;
        db.execute(
            &format!("DELETE FROM {} WHERE name_key=?", self.registry()),
            [&name_key],
        )?;
        self.remove_files(&file);
        Ok(())
    }

    fn attach(&self, db: &Connection, name: &str, file: &str) -> Result<()> {
        let path = self.directory.join(file);
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

    fn remove_files(&self, file: &str) {
        let path = self.directory.join(file);
        let _ = std::fs::remove_file(&path);
        let mut wal = path.into_os_string();
        wal.push(".wal");
        let _ = std::fs::remove_file(wal);
    }
}

fn validate(name: &str) -> Result<()> {
    ensure!(!name.is_empty(), "database name cannot be empty");
    ensure!(
        name.chars().count() <= MAX_NAME,
        "The identifier that starts with '{}' is too long. Maximum length is 128.",
        name.chars().take(MAX_NAME).collect::<String>()
    );
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
    fn names_are_validated() {
        assert!(validate("x").is_ok());
        assert!(validate("").is_err());
        assert!(validate(&"x".repeat(129)).is_err());
        assert!(validate("a\u{0}b").is_err());
    }
}
