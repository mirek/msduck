//! msdb: the system database that keeps backup and restore history.
//!
//! msdb is created on first use (a BACKUP, a RESTORE, `USE msdb` or a
//! reference to an msdb object outside a transaction) with SQL Server's
//! database ID 4. Its history tables are created through the ordinary T-SQL
//! path, so they keep SQL Server's declared column types, and history rows
//! are written with T-SQL INSERTs in msdb's context.
//!
//! A statement in another database whose relations are all msdb objects
//! (such as `SELECT TOP(1) * FROM msdb..backupmediafamily`) runs in msdb's
//! context, so its result metadata comes from msdb's declarations.
use super::super::{Session, reenter};
use crate::database_catalog::MSDB;
use crate::engine::{Execution, Parameter};
use anyhow::{Result, bail};
use sqlparser::ast::{
    Ident, ObjectName, ObjectNamePart, Query, Statement, Visit, VisitMut, Visitor, VisitorMut,
};
use std::collections::{HashMap, HashSet};
use std::ops::ControlFlow;

/// The history tables, with SQL Server's columns (reference/gaps-backup.json).
const TABLES: &[(&str, &str)] = &[
    (
        "backupmediaset",
        "[media_set_id] int IDENTITY(1,1) NOT NULL PRIMARY KEY, [media_uuid] uniqueidentifier NULL,
         [media_family_count] tinyint NULL, [name] nvarchar(128) NULL,
         [description] nvarchar(255) NULL, [software_name] nvarchar(128) NULL,
         [software_vendor_id] int NULL, [MTF_major_version] tinyint NULL,
         [mirror_count] tinyint NULL, [is_password_protected] bit NULL,
         [is_compressed] bit NULL, [is_encrypted] bit NULL",
    ),
    (
        "backupmediafamily",
        "[media_set_id] int NOT NULL, [family_sequence_number] tinyint NOT NULL,
         [media_family_id] uniqueidentifier NULL, [media_count] int NULL,
         [logical_device_name] nvarchar(128) NULL, [physical_device_name] nvarchar(260) NULL,
         [device_type] tinyint NULL, [physical_block_size] int NULL, [mirror] tinyint NOT NULL",
    ),
    (
        "backupset",
        "[backup_set_id] int IDENTITY(1,1) NOT NULL PRIMARY KEY, [backup_set_uuid] uniqueidentifier NOT NULL,
         [media_set_id] int NOT NULL, [first_family_number] tinyint NULL,
         [first_media_number] smallint NULL, [last_family_number] tinyint NULL,
         [last_media_number] smallint NULL, [catalog_family_number] tinyint NULL,
         [catalog_media_number] smallint NULL, [position] int NULL, [expiration_date] datetime NULL,
         [software_vendor_id] int NULL, [name] nvarchar(128) NULL, [description] nvarchar(255) NULL,
         [user_name] nvarchar(128) NULL, [software_major_version] tinyint NULL,
         [software_minor_version] tinyint NULL, [software_build_version] smallint NULL,
         [time_zone] smallint NULL, [mtf_minor_version] tinyint NULL, [first_lsn] numeric(25,0) NULL,
         [last_lsn] numeric(25,0) NULL, [checkpoint_lsn] numeric(25,0) NULL,
         [database_backup_lsn] numeric(25,0) NULL, [database_creation_date] datetime NULL,
         [backup_start_date] datetime NULL, [backup_finish_date] datetime NULL, [type] char(1) NULL,
         [sort_order] smallint NULL, [code_page] smallint NULL, [compatibility_level] tinyint NULL,
         [database_version] int NULL, [backup_size] numeric(20,0) NULL,
         [database_name] nvarchar(128) NULL, [server_name] nvarchar(128) NULL,
         [machine_name] nvarchar(128) NULL, [flags] int NULL, [unicode_locale] int NULL,
         [unicode_compare_style] int NULL, [collation_name] nvarchar(128) NULL,
         [is_password_protected] bit NULL, [recovery_model] nvarchar(60) NULL,
         [has_bulk_logged_data] bit NULL, [is_snapshot] bit NULL, [is_readonly] bit NULL,
         [is_single_user] bit NULL, [has_backup_checksums] bit NULL, [is_damaged] bit NULL,
         [begins_log_chain] bit NULL, [has_incomplete_metadata] bit NULL,
         [is_force_offline] bit NULL, [is_copy_only] bit NULL,
         [first_recovery_fork_guid] uniqueidentifier NULL,
         [last_recovery_fork_guid] uniqueidentifier NULL, [fork_point_lsn] numeric(25,0) NULL,
         [database_guid] uniqueidentifier NULL, [family_guid] uniqueidentifier NULL,
         [differential_base_lsn] numeric(25,0) NULL, [differential_base_guid] uniqueidentifier NULL,
         [compressed_backup_size] numeric(20,0) NULL, [key_algorithm] nvarchar(32) NULL,
         [encryptor_thumbprint] varbinary(20) NULL, [encryptor_type] nvarchar(32) NULL,
         [last_valid_restore_time] datetime NULL, [compression_algorithm] nvarchar(32) NULL",
    ),
    (
        "backupfile",
        "[backup_set_id] int NOT NULL, [first_family_number] tinyint NULL,
         [first_media_number] smallint NULL, [filegroup_name] nvarchar(128) NULL,
         [page_size] int NULL, [file_number] numeric(10,0) NOT NULL,
         [backed_up_page_count] numeric(10,0) NULL, [file_type] char(1) NULL,
         [source_file_block_size] numeric(10,0) NULL, [file_size] numeric(20,0) NULL,
         [logical_name] nvarchar(128) NULL, [physical_drive] nvarchar(260) NULL,
         [physical_name] nvarchar(260) NULL, [state] tinyint NULL, [state_desc] nvarchar(64) NULL,
         [create_lsn] numeric(25,0) NULL, [drop_lsn] numeric(25,0) NULL,
         [file_guid] uniqueidentifier NULL, [read_only_lsn] numeric(25,0) NULL,
         [read_write_lsn] numeric(25,0) NULL, [differential_base_lsn] numeric(25,0) NULL,
         [differential_base_guid] uniqueidentifier NULL, [backup_size] numeric(20,0) NULL,
         [filegroup_guid] uniqueidentifier NULL, [is_readonly] bit NULL, [is_present] bit NULL",
    ),
    (
        "restorehistory",
        "[restore_history_id] int IDENTITY(1,1) NOT NULL PRIMARY KEY, [restore_date] datetime NULL,
         [destination_database_name] nvarchar(128) NULL, [user_name] nvarchar(128) NULL,
         [backup_set_id] int NOT NULL, [restore_type] char(1) NULL, [replace] bit NULL,
         [recovery] bit NULL, [restart] bit NULL, [stop_at] datetime NULL,
         [device_count] tinyint NULL, [stop_at_mark_name] nvarchar(128) NULL,
         [stop_before] bit NULL",
    ),
    (
        "restorefile",
        "[restore_history_id] int NOT NULL, [file_number] numeric(10,0) NULL,
         [destination_phys_drive] nvarchar(260) NULL, [destination_phys_name] nvarchar(260) NULL",
    ),
];

/// Serializes history writes, which allocate IDs from the current maximum.
static HISTORY: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn is_msdb(name: &str) -> bool {
    name.eq_ignore_ascii_case(MSDB)
}

fn exists(session: &Session) -> Result<bool> {
    Ok(session
        .database
        .catalog()
        .resolve(&session.db, MSDB)?
        .is_some())
}

/// Create msdb and its history tables if they are missing. Creating a
/// database is not possible inside a transaction; there, a missing msdb is
/// an error.
pub fn ensure(session: &mut Session) -> Result<()> {
    if !exists(session)? {
        if session.transactions > 0 {
            bail!(
                "msdb does not exist yet; msduck creates it on the first BACKUP, RESTORE or msdb reference outside a transaction"
            );
        }
        session
            .database
            .catalog()
            .clone()
            .ensure_system(&session.db, MSDB)?;
    }
    let present = tables(session)?;
    let missing: Vec<&(&str, &str)> = TABLES
        .iter()
        .filter(|(table, _)| !present.iter().any(|name| name == table))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    if session.transactions > 0 {
        bail!("msdb history tables are missing; create them outside a transaction");
    }
    let _history = HISTORY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    in_msdb(session, |session| {
        let present = tables(session)?;
        for (table, columns) in missing {
            if !present.iter().any(|name| name == table) {
                run(session, &format!("CREATE TABLE dbo.{table} ({columns})"))?;
            }
        }
        Ok(())
    })
}

/// The tables in msdb's `dbo` schema.
fn tables(session: &Session) -> Result<Vec<String>> {
    let mut query = session.db.prepare(
        "SELECT table_name FROM duckdb_tables() WHERE database_name=? AND schema_name='dbo'",
    )?;
    let names = query
        .query_map([MSDB], |row| row.get(0))?
        .collect::<duckdb::Result<Vec<String>>>()?;
    Ok(names)
}

/// Run `f` with msdb as the session's current database, then return to the
/// previous one. The backup feature's own hooks are suspended meanwhile.
pub fn in_msdb<T>(session: &mut Session, f: impl FnOnce(&mut Session) -> Result<T>) -> Result<T> {
    let original = session.database.name.clone();
    if is_msdb(&original) {
        return reenter(session, "backup", f);
    }
    // Keep the original database in use while the session is in msdb, so
    // no other session can drop, restore or take it meanwhile.
    let catalog = session.database.catalog().clone();
    let hold = catalog.enter_as(&session.db, &original, &|alias| session.own_uses(alias))?;
    session.use_database(MSDB)?;
    let result = reenter(session, "backup", f);
    let restored = session.use_database(&original);
    drop(hold);
    match (result, restored) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(error)) => {
            Err(error.context(format!("could not return to database {original}")))
        }
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(back)) => {
            Err(error.context(format!("could not return to database {original}: {back:#}")))
        }
    }
}

/// Execute T-SQL statements through the ordinary engine path.
pub fn run(session: &mut Session, sql: &str) -> Result<()> {
    for statement in super::super::super::parse_batch(sql)? {
        session.execute(statement, &mut HashMap::new())?;
    }
    Ok(())
}

/// Insert a history row in msdb's context: `values` lists every column
/// but the IDENTITY one, in order. Returns the new IDENTITY value, or 0.
/// Callers hold `history_lock`, so the largest ID is the new row's.
pub fn insert(session: &mut Session, table: &str, values: &str) -> Result<i32> {
    let (_, definition) = TABLES
        .iter()
        .find(|(name, _)| *name == table)
        .ok_or_else(|| anyhow::anyhow!("unknown msdb table {table}"))?;
    let mut identity = None;
    let mut columns = Vec::new();
    let mut depth = 0;
    let definitions = definition.split(|c: char| {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            _ => {}
        }
        c == ',' && depth == 0
    });
    for column in definitions {
        let column = column.trim();
        let name = &column[..column.find(']').map_or(0, |end| end + 1)];
        if column.contains("IDENTITY") {
            identity = Some(name.trim_matches(['[', ']']).to_owned());
        } else {
            columns.push(name);
        }
    }
    run(
        session,
        &format!(
            "INSERT INTO dbo.{table} ({}) VALUES ({values})",
            columns.join(",")
        ),
    )?;
    let Some(identity) = identity else {
        return Ok(0);
    };
    Ok(session.db.query_row(
        &format!("SELECT CAST(max({identity}) AS INTEGER) FROM \"msdb\".dbo.{table}"),
        [],
        |row| row.get(0),
    )?)
}

/// Hold the history lock while writing rows.
pub fn history_lock() -> std::sync::MutexGuard<'static, ()> {
    HISTORY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// T-SQL literals for history rows.
pub mod literal {
    pub fn text(value: &str) -> String {
        format!("N'{}'", value.replace('\'', "''"))
    }

    pub fn optional(value: Option<&str>) -> String {
        value.map_or_else(|| "NULL".into(), text)
    }

    /// A datetime from microseconds since the Unix epoch.
    pub fn datetime(micros: i64) -> String {
        let seconds = micros.div_euclid(1_000_000);
        let fraction = micros.rem_euclid(1_000_000) / 1000;
        let days = seconds.div_euclid(86_400);
        let time = seconds.rem_euclid(86_400);
        let (year, month, day) = civil(days);
        format!(
            "CAST('{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{fraction:03}' AS datetime)",
            time / 3600,
            time % 3600 / 60,
            time % 60
        )
    }

    pub fn guid(value: &str) -> String {
        format!("CAST('{}' AS uniqueidentifier)", value.replace('\'', "''"))
    }

    pub fn bit(value: bool) -> &'static str {
        if value { "1" } else { "0" }
    }

    /// Days since 1970-01-01 to a proleptic Gregorian date.
    fn civil(days: i64) -> (i64, i64, i64) {
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = doy - (153 * mp + 2) / 5 + 1;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = yoe + era * 400 + i64::from(month <= 2);
        (year, month, day)
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn datetimes_are_iso_literals() {
            assert_eq!(
                super::datetime(1_790_834_481_426_730),
                "CAST('2026-10-01T06:01:21.426' AS datetime)"
            );
            assert_eq!(
                super::datetime(0),
                "CAST('1970-01-01T00:00:00.000' AS datetime)"
            );
        }
    }
}

/// Relations of a statement: msdb-qualified names, and names that are not.
struct Relations {
    msdb: usize,
    other: Vec<String>,
}

fn relations(statement: &Statement) -> Relations {
    struct Collect(Relations);
    impl Visitor for Collect {
        type Break = ();
        fn pre_visit_relation(&mut self, relation: &ObjectName) -> ControlFlow<()> {
            match relation.0.first() {
                Some(ObjectNamePart::Identifier(database))
                    if relation.0.len() == 3 && is_msdb(&database.value) =>
                {
                    self.0.msdb += 1
                }
                _ => self.0.other.push(relation.to_string()),
            }
            ControlFlow::Continue(())
        }
    }
    let mut collect = Collect(Relations {
        msdb: 0,
        other: vec![],
    });
    let _ = statement.visit(&mut collect);
    collect.0
}

/// Names of common table expressions defined anywhere in the statement.
fn cte_names(statement: &Statement) -> HashSet<String> {
    struct Collect(HashSet<String>);
    impl Visitor for Collect {
        type Break = ();
        fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<()> {
            if let Some(with) = &query.with {
                for cte in &with.cte_tables {
                    self.0.insert(cte.alias.name.value.to_lowercase());
                }
            }
            ControlFlow::Continue(())
        }
    }
    let mut collect = Collect(HashSet::new());
    let _ = statement.visit(&mut collect);
    collect.0
}

/// `msdb..name` names msdb's default schema, `dbo`.
fn fill_default_schema(statement: &mut Statement) {
    struct Fill;
    impl VisitorMut for Fill {
        type Break = ();
        fn pre_visit_relation(&mut self, relation: &mut ObjectName) -> ControlFlow<()> {
            if relation.0.len() == 3
                && let (
                    Some(ObjectNamePart::Identifier(database)),
                    Some(ObjectNamePart::Identifier(schema)),
                ) = (relation.0.first(), relation.0.get(1))
                && is_msdb(&database.value)
                && schema.value.is_empty()
            {
                relation.0[1] = ObjectNamePart::Identifier(Ident::new("dbo"));
            }
            ControlFlow::Continue(())
        }
    }
    let _ = statement.visit(&mut Fill);
}

/// Whether the statement only reads or writes msdb objects, so it can run
/// in msdb's context: every relation is msdb-qualified or a CTE name.
fn msdb_only(statement: &Statement, relations: &Relations) -> bool {
    if relations.msdb == 0 {
        return false;
    }
    let ctes = cte_names(statement);
    relations.other.iter().all(|name| {
        !name.contains('.') && ctes.contains(&name.trim_matches(['[', ']', '"']).to_lowercase())
    })
}

/// Statements that reference msdb: create it on first use, and run
/// statements that only use msdb objects in msdb's context. Others continue
/// unchanged.
pub fn route(
    session: &mut Session,
    statement: &mut Statement,
    parameters: &mut HashMap<String, Parameter>,
) -> Result<Option<Execution>> {
    if let Statement::Use(sqlparser::ast::Use::Object(name)) = statement
        && let [ObjectNamePart::Identifier(database)] = name.0.as_slice()
        && is_msdb(&database.value)
    {
        ensure(session)?;
        return Ok(None);
    }
    if !matches!(
        statement,
        Statement::Query(_) | Statement::Insert(_) | Statement::Update(_) | Statement::Delete(_)
    ) {
        return Ok(None);
    }
    let found = relations(statement);
    if found.msdb == 0 {
        return Ok(None);
    }
    ensure(session)?;
    fill_default_schema(statement);
    if is_msdb(&session.database.name) || !msdb_only(statement, &found) {
        return Ok(None);
    }
    let statement = statement.clone();
    in_msdb(session, |session| session.execute(statement, parameters)).map(Some)
}
