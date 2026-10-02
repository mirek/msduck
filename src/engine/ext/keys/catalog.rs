//! Per-database record of key constraints and keys-managed indexes.
//!
//! `main.__msduck_keys` keeps one row per PRIMARY KEY or UNIQUE constraint
//! (kind `PK` or `UQ`) declared by CREATE TABLE, and per index this feature
//! creates (kind `IX`). `backend_name` is the DuckDB index that enforces a
//! managed key; `is_native` says DuckDB also enforces the constraint itself
//! (with NULL keys distinct, so a nullable unique key has both). Indexes are also
//! registered in the table-owned index catalog (`index_catalog`), which owns
//! their index IDs; `incarnation` links the two.
use anyhow::Result;
use duckdb::{Connection, params};

pub(super) fn bootstrap(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE SEQUENCE IF NOT EXISTS main.__msduck_key_tags START 1 NO CYCLE;
        CREATE TABLE IF NOT EXISTS main.__msduck_keys(
          tag BIGINT PRIMARY KEY,object_id INTEGER NOT NULL,name VARCHAR NOT NULL,
          kind VARCHAR NOT NULL,is_unique BOOLEAN NOT NULL,is_clustered BOOLEAN NOT NULL,
          is_native BOOLEAN NOT NULL,backend_name VARCHAR,incarnation BIGINT,key_columns VARCHAR NOT NULL,
          included_columns VARCHAR NOT NULL,filter_definition VARCHAR,
          filter_columns VARCHAR NOT NULL);
        -- The declared layout of PRIMARY KEY and UNIQUE constraints: the
        -- CLUSTERED or NONCLUSTERED keyword (NULL when neither was written,
        -- or for keys recorded before this table existed) and the 1-based
        -- ordinals of DESC key columns. sys.indexes and sys.index_columns
        -- read it; a key without a row keeps SQL Server's defaults.
        CREATE TABLE IF NOT EXISTS main.__msduck_key_layout(
          tag BIGINT NOT NULL,clustered BOOLEAN,descending INTEGER[] NOT NULL)",
    )?;
    Ok(())
}

/// Record the declared layout of a key constraint.
pub(super) fn record_layout(
    db: &Connection,
    tag: i64,
    clustered: Option<bool>,
    descending: &[i32],
) -> Result<()> {
    db.execute("DELETE FROM main.__msduck_key_layout WHERE tag=?", [tag])?;
    let ordinals = descending
        .iter()
        .map(i32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    db.execute(
        &format!(
            "INSERT INTO main.__msduck_key_layout VALUES(?,?,CAST([{ordinals}] AS INTEGER[]))"
        ),
        params![tag, clustered],
    )?;
    if clustered == Some(true) {
        // A declared CLUSTERED key is the table's clustered index for later
        // CREATE CLUSTERED INDEX statements (1902) too.
        db.execute(
            "UPDATE main.__msduck_keys SET is_clustered=true WHERE tag=?",
            [tag],
        )?;
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub(super) struct Key {
    pub tag: i64,
    pub object_id: i32,
    pub name: String,
    /// `PK`, `UQ` or `IX`.
    pub kind: String,
    pub unique: bool,
    pub clustered: bool,
    pub native: bool,
    pub backend_name: Option<String>,
    pub incarnation: Option<i64>,
    pub columns: Vec<String>,
    pub include: Vec<String>,
    pub filter: Option<String>,
    /// Columns the filter references.
    pub filter_columns: Vec<String>,
}

impl Key {
    pub fn message_kind(&self) -> msduck_sql::dialect::ext::keys::message::Kind {
        use msduck_sql::dialect::ext::keys::message::Kind;
        match self.kind.as_str() {
            "PK" => Kind::PrimaryKey,
            "UQ" => Kind::UniqueConstraint,
            _ => Kind::UniqueIndex,
        }
    }
    pub fn constraint(&self) -> bool {
        self.kind != "IX"
    }
}

pub(super) fn next_tag(db: &Connection) -> Result<i64> {
    Ok(db.query_row("SELECT nextval('main.__msduck_key_tags')", [], |r| r.get(0))?)
}

fn list(values: &[String]) -> String {
    serde_json::to_string(values).unwrap_or_else(|_| "[]".into())
}

fn parse_list(text: &str) -> Vec<String> {
    serde_json::from_str(text).unwrap_or_default()
}

pub(super) fn insert(db: &Connection, key: &Key) -> Result<()> {
    db.execute(
        "INSERT INTO main.__msduck_keys VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)",
        params![
            key.tag,
            key.object_id,
            key.name,
            key.kind,
            key.unique,
            key.clustered,
            key.native,
            key.backend_name,
            key.incarnation,
            list(&key.columns),
            list(&key.include),
            key.filter,
            list(&key.filter_columns),
        ],
    )?;
    Ok(())
}

pub(super) fn remove(db: &Connection, tag: i64) -> Result<()> {
    db.execute("DELETE FROM main.__msduck_keys WHERE tag=?", [tag])?;
    db.execute("DELETE FROM main.__msduck_key_layout WHERE tag=?", [tag])?;
    Ok(())
}

const COLUMNS: &str = "tag,object_id,name,kind,is_unique,is_clustered,is_native,backend_name,incarnation,key_columns,included_columns,filter_definition,filter_columns";

fn read(row: &duckdb::Row<'_>) -> duckdb::Result<Key> {
    Ok(Key {
        tag: row.get(0)?,
        object_id: row.get(1)?,
        name: row.get(2)?,
        kind: row.get(3)?,
        unique: row.get(4)?,
        clustered: row.get(5)?,
        native: row.get(6)?,
        backend_name: row.get(7)?,
        incarnation: row.get(8)?,
        columns: parse_list(&row.get::<_, String>(9)?),
        include: parse_list(&row.get::<_, String>(10)?),
        filter: row.get(11)?,
        filter_columns: parse_list(&row.get::<_, String>(12)?),
    })
}

/// The recorded keys of one table, constraints first, in creation order.
/// Reads do not prune, so they work on a separate read-only connection.
pub(super) fn table(db: &Connection, object_id: i32) -> Result<Vec<Key>> {
    Ok(db
        .prepare(&format!(
            "SELECT {COLUMNS} FROM main.__msduck_keys WHERE object_id=? ORDER BY kind='IX',tag"
        ))?
        .query_map([object_id], read)?
        .collect::<duckdb::Result<Vec<_>>>()?)
}

pub(super) fn by_tag(db: &Connection, tag: i64) -> Result<Option<Key>> {
    let mut statement = db.prepare(&format!(
        "SELECT {COLUMNS} FROM main.__msduck_keys WHERE tag=?"
    ))?;
    let mut rows = statement.query_map([tag], read)?;
    Ok(rows.next().transpose()?)
}

/// Every live key of the current database.
pub(super) fn all(db: &Connection) -> Result<Vec<Key>> {
    Ok(db
        .prepare(&format!(
            "SELECT {COLUMNS} FROM main.__msduck_keys ORDER BY tag"
        ))?
        .query_map([], read)?
        .collect::<duckdb::Result<Vec<_>>>()?)
}

/// Forget keys of dropped tables. Object IDs are never reused, so a
/// recreated table cannot inherit an old row. A live table's rows stay even
/// while its indexes are briefly dropped around an ALTER TABLE.
pub(super) fn prune(db: &Connection) -> Result<()> {
    db.execute_batch(
        "DELETE FROM main.__msduck_keys k WHERE NOT EXISTS(
           SELECT 1 FROM sys.objects o WHERE o.object_id=k.object_id AND rtrim(o.type)='U');
         DELETE FROM main.__msduck_key_layout l WHERE NOT EXISTS(
           SELECT 1 FROM main.__msduck_keys k WHERE k.tag=l.tag)",
    )?;
    Ok(())
}
