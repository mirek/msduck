//! Backend tables of temporary objects.
//!
//! Names never start with `#` or `@`, which the permanent-table paths (such
//! as SELECT INTO) reject, and carry a prefix that marks them as temporary:
//!
//! - `#t` becomes `__msduck_temp_<id>_t` and `@t` becomes `__msduck_tv_<id>_t`,
//!   where `<id>` is 12 hexadecimal digits unique to the creation;
//! - `##t` becomes `__msduck_global_t` (lower case), which every session
//!   derives the same way.
use super::{super::reenter, Backend, NAME};
use crate::engine::Session;
use anyhow::Result;
use duckdb::arrow::record_batch::RecordBatch;
use std::sync::atomic::{AtomicU64, Ordering};

const PREFIXES: [&str; 3] = ["__msduck_temp_", "__msduck_tv_", "__msduck_global_"];

/// Keep the readable part of a name; uniqueness comes from the id.
fn readable(name: &str) -> String {
    name.trim_start_matches(['#', '@'])
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

pub(super) fn unique_name(kind: &str, name: &str) -> String {
    // Start from the clock so names stay unique across restarts, even
    // before orphans of an earlier process are dropped.
    static NEXT: std::sync::LazyLock<AtomicU64> = std::sync::LazyLock::new(|| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_micros() as u64);
        AtomicU64::new(now & 0xffff_ffff_ffff)
    });
    let id = NEXT.fetch_add(1, Ordering::Relaxed) & 0xffff_ffff_ffff;
    format!("__msduck_{kind}_{id:012x}_{}", readable(name))
}

/// The backend name of a global temporary table. Global names compare
/// case-insensitively; characters other than ASCII letters, digits and `_`
/// are encoded, so distinct names never share a table.
pub(super) fn global_name(name: &str) -> String {
    let mut physical = String::from("__msduck_global_");
    for c in name.trim_start_matches('#').to_lowercase().chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            physical.push(c);
        } else {
            physical.push_str(&format!("${:x}$", c as u32));
        }
    }
    physical
}

fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Whether `dbo.<physical>` exists in the current database.
pub(super) fn exists(db: &duckdb::Connection, physical: &str) -> Result<bool> {
    let count: i64 = db.query_row(
        "SELECT count(*) FROM duckdb_tables() WHERE database_name = current_database() AND schema_name = 'dbo' AND table_name = ?",
        [physical],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// The identity of a backend table in its database, if it exists.
pub(super) fn oid(db: &duckdb::Connection, backend: &Backend) -> Result<Option<i64>> {
    let oid: Option<i64> = db.query_row(
        "SELECT max(table_oid) FROM duckdb_tables() WHERE database_name = ? AND schema_name = 'dbo' AND table_name = ?",
        [&backend.alias, &backend.physical],
        |row| row.get(0),
    )?;
    Ok(oid)
}

/// Drop a backend table with the ordinary DROP TABLE path, so the catalog,
/// identity sequences and indexes go with it. A table in a database other
/// than the current one is dropped directly; that database's catalog drops
/// its stale rows at its next DDL statement.
pub(super) fn drop_table(session: &mut Session, backend: &Backend) -> Result<()> {
    if backend.alias != session.database.alias() {
        session.db.execute_batch(&format!(
            "DROP TABLE IF EXISTS {}.dbo.{}",
            quote(&backend.alias),
            quote(&backend.physical)
        ))?;
        return Ok(());
    }
    let statement = parse(&format!(
        "DROP TABLE IF EXISTS dbo.{}",
        quote(&backend.physical)
    ))?;
    let mut parameters = std::collections::HashMap::new();
    reenter(session, NAME, |session| {
        session.execute(statement, &mut parameters)
    })?;
    Ok(())
}

fn parse(sql: &str) -> Result<sqlparser::ast::Statement> {
    let mut statements = msduck_sql::batch::parse(sql)?;
    anyhow::ensure!(statements.len() == 1, "expected one statement");
    Ok(statements.remove(0))
}

/// The rows of a table variable, kept outside the transaction.
pub(super) fn snapshot(db: &duckdb::Connection, physical: &str) -> Result<Vec<RecordBatch>> {
    let mut statement = db.prepare(&format!("SELECT * FROM dbo.{}", quote(physical)))?;
    Ok(statement.query_arrow([])?.collect())
}

/// Give a table variable back the rows of a snapshot, recreating its table
/// if the rollback removed it.
pub(super) fn restore(
    session: &mut Session,
    backend: &Backend,
    definition: &str,
    rows: Vec<RecordBatch>,
) -> Result<()> {
    anyhow::ensure!(
        backend.alias == session.database.alias(),
        "unsupported table variable restore in another database"
    );
    let recreated = !exists(&session.db, &backend.physical)?;
    if recreated {
        let create = msduck_sql::dialect::ext::temp_tables::create_table(
            definition,
            &sqlparser::ast::Ident::with_quote('"', &backend.physical),
        )?;
        let mut parameters = std::collections::HashMap::new();
        reenter(session, NAME, |session| {
            session.execute(create, &mut parameters)
        })?;
    }
    session
        .db
        .execute_batch(&format!("DELETE FROM dbo.{}", quote(&backend.physical)))?;
    {
        let mut appender = session.db.appender_to_db(&backend.physical, "dbo")?;
        for batch in rows {
            appender.append_record_batch(batch)?;
        }
        appender.flush()?;
    }
    if recreated {
        // IDENTITY values are not rolled back: continue after the restored
        // rows rather than from the seed of the recreated sequence.
        let table = msduck_sql::dialect::ext::temp_tables::backend_name(&backend.physical);
        for (column, sequence) in crate::identity::columns(&session.db, &table)? {
            session.db.query_row(
                &format!(
                    "SELECT count(nextval('{sequence}')) FROM main.__msduck_identity_definitions d, range(CAST(coalesce((SELECT (max({}) - d.seed) // d.increment_value + 1 FROM dbo.{}), 0) AS BIGINT)) WHERE d.sequence_name = ?",
                    quote(&column),
                    quote(&backend.physical)
                ),
                [&sequence],
                |row| row.get::<_, i64>(0),
            )?;
        }
    }
    Ok(())
}

/// Drop every temporary table left in a database by an earlier process.
pub(super) fn drop_orphans(db: &duckdb::Connection) -> Result<()> {
    let mut names = Vec::new();
    {
        let mut statement = db.prepare(
            "SELECT table_name FROM duckdb_tables() WHERE database_name = current_database() AND schema_name = 'dbo'",
        )?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let name: String = row.get(0)?;
            if PREFIXES.iter().any(|prefix| name.starts_with(prefix)) {
                names.push(name);
            }
        }
    }
    if names.is_empty() {
        return Ok(());
    }
    for name in &names {
        let statement = parse(&format!("DROP TABLE IF EXISTS dbo.{}", quote(name)))?;
        crate::identity::drop_table(db, &statement, true)?;
    }
    crate::object_catalog::sync(db)?;
    crate::query_catalog::sync(db)?;
    crate::index_catalog::sync(db)?;
    crate::computed_columns::prune(db)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_names_are_marked_unique_and_injective_for_globals() {
        let a = unique_name("temp", "#Orders-2");
        let b = unique_name("temp", "#Orders-2");
        assert!(a.starts_with("__msduck_temp_") && a.ends_with("_Orders_2"));
        assert_ne!(a, b);
        assert_eq!(global_name("##Shared"), "__msduck_global_shared");
        assert_ne!(global_name("##a-b"), global_name("##a_b"));
        assert!(!a.starts_with(['#', '@']));
    }
}
