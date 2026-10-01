//! The constraint store: one row per CHECK, FOREIGN KEY, PRIMARY KEY and
//! UNIQUE constraint in `main.__msduck_constraints`, and the catalog views
//! derived from it. DEFAULT constraints stay in the built-in
//! `main.__msduck_default_constraints` table.
use anyhow::{Context, Result};
use duckdb::Connection;
use msduck_sql::dialect::ext::constraints::Referential;
use sqlparser::ast::{Ident, ObjectName, ObjectNamePart};

/// Constraint type codes, as in `sys.objects.type`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Type {
    Check,
    Foreign,
    Primary,
    Unique,
}

impl Type {
    pub fn code(self) -> &'static str {
        match self {
            Self::Check => "C",
            Self::Foreign => "F",
            Self::Primary => "PK",
            Self::Unique => "UQ",
        }
    }
    fn from_code(code: &str) -> Result<Self> {
        Ok(match code {
            "C" => Self::Check,
            "F" => Self::Foreign,
            "PK" => Self::Primary,
            "UQ" => Self::Unique,
            other => anyhow::bail!("unknown constraint type {other}"),
        })
    }
    /// Whether NOCHECK and CHECK CONSTRAINT apply.
    pub fn toggles(self) -> bool {
        matches!(self, Self::Check | Self::Foreign)
    }
}

/// A user table, by its current names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Table {
    pub id: i32,
    pub schema: String,
    pub name: String,
}

pub(crate) fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

impl Table {
    /// The DuckDB relation.
    pub fn sql(&self) -> String {
        format!("{}.{}", quote(&self.schema), quote(&self.name))
    }
    /// `schema.table`, as SQL Server diagnostics name it.
    pub fn display(&self) -> String {
        format!("{}.{}", self.schema, self.name)
    }
    /// A T-SQL name for statements executed through the engine.
    pub fn object_name(&self) -> ObjectName {
        ObjectName(vec![
            ObjectNamePart::Identifier(Ident::with_quote('[', &self.schema)),
            ObjectNamePart::Identifier(Ident::with_quote('[', &self.name)),
        ])
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Constraint {
    pub id: i32,
    pub name: String,
    pub kind: Type,
    pub table: Table,
    /// Key or referencing columns; for CHECK, the columns it references.
    pub columns: Vec<String>,
    pub referenced: Option<Table>,
    pub referenced_columns: Vec<String>,
    pub on_delete: Referential,
    pub on_update: Referential,
    /// CHECK expression text, as written.
    pub definition: Option<String>,
    pub disabled: bool,
    pub untrusted: bool,
    /// The one column a CHECK names in its diagnostics, if any.
    pub column: Option<String>,
}

impl Constraint {
    pub fn enabled(&self) -> bool {
        !self.disabled
    }
    pub fn action(&self, update: bool) -> Referential {
        if update {
            self.on_update
        } else {
            self.on_delete
        }
    }
    pub fn self_referencing(&self) -> bool {
        self.referenced
            .as_ref()
            .is_some_and(|r| r.id == self.table.id)
    }
}

pub(crate) fn bootstrap(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS main.__msduck_constraints(
            object_id INTEGER PRIMARY KEY,
            parent_object_id INTEGER NOT NULL,
            name VARCHAR NOT NULL,
            type_code VARCHAR NOT NULL,
            is_system_named BOOLEAN NOT NULL,
            is_disabled BOOLEAN NOT NULL,
            is_not_trusted BOOLEAN NOT NULL,
            definition VARCHAR,
            columns VARCHAR[] NOT NULL,
            parent_column VARCHAR,
            referenced_object_id INTEGER,
            referenced_columns VARCHAR[] NOT NULL,
            delete_action INTEGER NOT NULL,
            update_action INTEGER NOT NULL,
            create_date TIMESTAMP NOT NULL,
            modify_date TIMESTAMP NOT NULL);
        CREATE OR REPLACE VIEW main.__msduck_live_constraints AS
          SELECT c.*,o.schema_id,
            CAST(CASE c.type_code WHEN 'C' THEN 'CHECK_CONSTRAINT' WHEN 'F' THEN 'FOREIGN_KEY_CONSTRAINT'
              WHEN 'PK' THEN 'PRIMARY_KEY_CONSTRAINT' ELSE 'UNIQUE_CONSTRAINT' END AS VARCHAR) AS type_desc
          FROM main.__msduck_constraints c JOIN main.__msduck_objects o ON o.object_id=c.parent_object_id AND o.type_code='U'
          UNION ALL
          SELECT CAST(2000000000+k.tag AS INTEGER),k.object_id,k.name,k.kind,
            starts_with(k.name,k.kind||'__'),false,false,CAST(NULL AS VARCHAR),CAST([] AS VARCHAR[]),
            CAST(NULL AS VARCHAR),CAST(NULL AS INTEGER),CAST([] AS VARCHAR[]),0,0,o.create_date,o.modify_date,o.schema_id,
            CAST(CASE k.kind WHEN 'PK' THEN 'PRIMARY_KEY_CONSTRAINT' ELSE 'UNIQUE_CONSTRAINT' END AS VARCHAR)
          FROM main.__msduck_keys k JOIN main.__msduck_objects o ON o.object_id=k.object_id AND o.type_code='U'
          WHERE k.kind IN ('PK','UQ');
        DROP VIEW IF EXISTS sys.__msduck_constraint_base_objects;
        ALTER VIEW sys.objects RENAME TO __msduck_constraint_base_objects;
        CREATE VIEW sys.objects AS
          SELECT * FROM sys.__msduck_constraint_base_objects
          UNION ALL
          SELECT name,object_id,CAST(NULL AS INTEGER),schema_id,parent_object_id,rpad(type_code,2,' '),type_desc,
            create_date,modify_date,false,false,false
          FROM main.__msduck_live_constraints;
        CREATE OR REPLACE VIEW sys.check_constraints AS
          SELECT name,object_id,CAST(NULL AS INTEGER) AS principal_id,schema_id,parent_object_id,
            CAST('C ' AS VARCHAR) AS type,type_desc,create_date,modify_date,false AS is_ms_shipped,
            false AS is_published,false AS is_schema_published,is_disabled,false AS is_not_for_replication,
            is_not_trusted,
            CAST(coalesce((SELECT k.column_id FROM main.__msduck_column_info k WHERE k.object_id=c.parent_object_id AND lower(k.name)=lower(c.parent_column)),0) AS INTEGER) AS parent_column_id,
            '(' || definition || ')' AS definition,false AS uses_database_collation,is_system_named
          FROM main.__msduck_live_constraints c WHERE type_code='C';
        CREATE OR REPLACE VIEW sys.foreign_keys AS
          SELECT name,object_id,CAST(NULL AS INTEGER) AS principal_id,schema_id,parent_object_id,
            CAST('F ' AS VARCHAR) AS type,type_desc,create_date,modify_date,false AS is_ms_shipped,
            false AS is_published,false AS is_schema_published,referenced_object_id,
            CAST(NULL AS INTEGER) AS key_index_id,is_disabled,false AS is_not_for_replication,is_not_trusted,
            CAST(delete_action AS TINYINT) AS delete_referential_action,
            CAST(CASE delete_action WHEN 1 THEN 'CASCADE' WHEN 2 THEN 'SET_NULL' WHEN 3 THEN 'SET_DEFAULT' ELSE 'NO_ACTION' END AS VARCHAR) AS delete_referential_action_desc,
            CAST(update_action AS TINYINT) AS update_referential_action,
            CAST(CASE update_action WHEN 1 THEN 'CASCADE' WHEN 2 THEN 'SET_NULL' WHEN 3 THEN 'SET_DEFAULT' ELSE 'NO_ACTION' END AS VARCHAR) AS update_referential_action_desc,
            is_system_named
          FROM main.__msduck_live_constraints WHERE type_code='F';
        CREATE OR REPLACE VIEW sys.foreign_key_columns AS
          SELECT c.object_id AS constraint_object_id,CAST(k.ordinal AS INTEGER) AS constraint_column_id,
            c.parent_object_id,pc.column_id AS parent_column_id,c.referenced_object_id,rc.column_id AS referenced_column_id
          FROM main.__msduck_live_constraints c
          CROSS JOIN LATERAL (SELECT unnest(c.columns) AS parent_name,unnest(c.referenced_columns) AS referenced_name,unnest(generate_series(1,len(c.columns))) AS ordinal) k
          JOIN main.__msduck_column_info pc ON pc.object_id=c.parent_object_id AND lower(pc.name)=lower(k.parent_name)
          JOIN main.__msduck_column_info rc ON rc.object_id=c.referenced_object_id AND lower(rc.name)=lower(k.referenced_name)
          WHERE c.type_code='F';
        CREATE OR REPLACE VIEW sys.key_constraints AS
          SELECT name,object_id,CAST(NULL AS INTEGER) AS principal_id,schema_id,parent_object_id,
            CAST(rpad(type_code,2,' ') AS VARCHAR) AS type,type_desc,create_date,modify_date,false AS is_ms_shipped,
            false AS is_published,false AS is_schema_published,CAST(NULL AS INTEGER) AS unique_index_id,
            is_system_named,true AS is_enforced
          FROM main.__msduck_live_constraints WHERE type_code IN ('PK','UQ');",
    )
    .context("constraint catalog bootstrap")?;
    Ok(())
}

const SELECT: &str =
    "SELECT c.object_id,c.name,c.type_code,c.parent_object_id,ps.name,po.name,c.columns,
    c.referenced_object_id,rs.name,ro.name,c.referenced_columns,c.delete_action,c.update_action,
    c.definition,c.is_disabled,c.is_not_trusted,c.parent_column
  FROM main.__msduck_constraints c
  JOIN main.__msduck_objects po ON po.object_id=c.parent_object_id AND po.type_code='U'
  JOIN main.__msduck_schemas ps ON ps.schema_id=po.schema_id
  LEFT JOIN main.__msduck_objects ro ON ro.object_id=c.referenced_object_id AND ro.type_code='U'
  LEFT JOIN main.__msduck_schemas rs ON rs.schema_id=ro.schema_id";

fn strings(value: duckdb::types::Value) -> Vec<String> {
    match value {
        duckdb::types::Value::List(items) => items
            .into_iter()
            .filter_map(|item| match item {
                duckdb::types::Value::Text(text) => Some(text),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn read(row: &duckdb::Row<'_>) -> duckdb::Result<Constraint> {
    let referenced_id: Option<i32> = row.get(7)?;
    let referenced = match (
        referenced_id,
        row.get::<_, Option<String>>(8)?,
        row.get::<_, Option<String>>(9)?,
    ) {
        (Some(id), Some(schema), Some(name)) => Some(Table { id, schema, name }),
        _ => None,
    };
    let code: String = row.get(2)?;
    Ok(Constraint {
        id: row.get(0)?,
        name: row.get(1)?,
        kind: Type::from_code(&code).unwrap_or(Type::Check),
        table: Table {
            id: row.get(3)?,
            schema: row.get(4)?,
            name: row.get(5)?,
        },
        columns: strings(row.get(6)?),
        referenced,
        referenced_columns: strings(row.get(10)?),
        on_delete: Referential::from_code(row.get(11)?),
        on_update: Referential::from_code(row.get(12)?),
        definition: row.get(13)?,
        disabled: row.get(14)?,
        untrusted: row.get(15)?,
        column: row.get(16)?,
    })
}

/// Every constraint of live tables, in creation order.
pub(crate) fn load(db: &Connection) -> Result<Vec<Constraint>> {
    let mut statement = db.prepare(&format!("{SELECT} ORDER BY c.object_id"))?;
    let rows = statement.query_map([], read)?;
    let mut constraints: Vec<Constraint> = rows.collect::<duckdb::Result<_>>()?;
    constraints.extend(keys(db)?);
    Ok(constraints)
}

/// PRIMARY KEY and UNIQUE constraints are recorded by the keys feature in
/// `main.__msduck_keys`; their object ids are `KEY_IDS + tag`.
pub(crate) const KEY_IDS: i64 = 2_000_000_000;

fn keys(db: &Connection) -> Result<Vec<Constraint>> {
    let mut statement = db.prepare(
        "SELECT k.tag,k.name,k.kind,k.object_id,s.name,o.name,k.key_columns
         FROM main.__msduck_keys k JOIN main.__msduck_objects o ON o.object_id=k.object_id AND o.type_code='U'
         JOIN main.__msduck_schemas s ON s.schema_id=o.schema_id
         WHERE k.kind IN ('PK','UQ') ORDER BY k.tag",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            Table {
                id: row.get(3)?,
                schema: row.get(4)?,
                name: row.get(5)?,
            },
            row.get::<_, String>(6)?,
        ))
    })?;
    let mut keys = Vec::new();
    for row in rows {
        let (tag, name, kind, table, columns) = row?;
        keys.push(Constraint {
            id: (KEY_IDS + tag) as i32,
            name,
            kind: if kind == "PK" {
                Type::Primary
            } else {
                Type::Unique
            },
            table,
            columns: serde_json::from_str(&columns).unwrap_or_default(),
            referenced: None,
            referenced_columns: vec![],
            on_delete: Referential::NoAction,
            on_update: Referential::NoAction,
            definition: None,
            disabled: false,
            untrusted: false,
            column: None,
        });
    }
    Ok(keys)
}

/// Record a PRIMARY KEY or UNIQUE constraint that DuckDB enforces natively,
/// in the keys feature's store.
pub(crate) fn insert_key(
    db: &Connection,
    table: &Table,
    name: &str,
    primary: bool,
    columns: &[String],
) -> Result<()> {
    db.execute(
        "INSERT INTO main.__msduck_keys VALUES(nextval('main.__msduck_key_tags'),?,?,?,true,false,true,NULL,NULL,?,'[]',NULL,'[]')",
        duckdb::params![
            table.id,
            name,
            if primary { "PK" } else { "UQ" },
            serde_json::to_string(columns)?,
        ],
    )?;
    Ok(())
}

/// Whether a recorded key is native, and the DuckDB index of a managed one.
pub(crate) fn key_storage(db: &Connection, id: i32) -> Result<(bool, Option<String>)> {
    Ok(db.query_row(
        "SELECT is_native,backend_name FROM main.__msduck_keys WHERE tag=?",
        [i64::from(id) - KEY_IDS],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?)
}

pub(crate) fn delete_key(db: &Connection, id: i32) -> Result<()> {
    db.execute(
        "DELETE FROM main.__msduck_keys WHERE tag=?",
        [i64::from(id) - KEY_IDS],
    )?;
    Ok(())
}

/// Whether the database has any constraint that msduck enforces itself, or
/// any foreign key (whose referenced side needs attention on writes).
pub(crate) fn any_enforced(db: &Connection) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM main.__msduck_constraints WHERE type_code IN ('C','F'))",
        [],
        |row| row.get(0),
    )?)
}

/// Drop rows whose table no longer exists.
pub(crate) fn prune(db: &Connection) -> Result<()> {
    db.execute_batch(
        "DELETE FROM main.__msduck_constraints c WHERE NOT EXISTS(SELECT 1 FROM main.__msduck_objects o WHERE o.object_id=c.parent_object_id AND o.type_code='U')",
    )?;
    Ok(())
}

pub(crate) fn next_object_id(db: &Connection) -> Result<i32> {
    Ok(db.query_row(
        "SELECT CAST(nextval('main.__msduck_object_ids') AS INTEGER)",
        [],
        |row| row.get(0),
    )?)
}

/// A constraint about to be stored.
pub(crate) struct New<'a> {
    pub id: i32,
    pub table: &'a Table,
    pub name: &'a str,
    pub system_named: bool,
    pub kind: Type,
    pub columns: &'a [String],
    pub column: Option<&'a str>,
    pub definition: Option<&'a str>,
    pub referenced: Option<&'a Table>,
    pub referenced_columns: &'a [String],
    pub on_delete: Referential,
    pub on_update: Referential,
    pub untrusted: bool,
}

/// A native VARCHAR[] literal; list parameters cannot be bound.
fn list(values: &[String]) -> String {
    let items = values
        .iter()
        .map(|value| format!("'{}'", value.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(",");
    format!("CAST([{items}] AS VARCHAR[])")
}

pub(crate) fn insert(db: &Connection, new: &New<'_>) -> Result<()> {
    db.execute(
        &format!(
            "INSERT INTO main.__msduck_constraints VALUES(?,?,?,?,?,false,?,?,{},?,?,{},?,?,CAST(current_timestamp AS TIMESTAMP),CAST(current_timestamp AS TIMESTAMP))",
            list(new.columns),
            list(new.referenced_columns)
        ),
        duckdb::params![
            new.id,
            new.table.id,
            new.name,
            new.kind.code(),
            new.system_named,
            new.untrusted,
            new.definition,
            new.column,
            new.referenced.map(|table| table.id),
            new.on_delete.code(),
            new.on_update.code(),
        ],
    )?;
    Ok(())
}

pub(crate) fn delete(db: &Connection, id: i32) -> Result<()> {
    db.execute(
        "DELETE FROM main.__msduck_constraints WHERE object_id=?",
        [id],
    )?;
    Ok(())
}

pub(crate) fn set_state(db: &Connection, id: i32, disabled: bool, untrusted: bool) -> Result<()> {
    db.execute(
        "UPDATE main.__msduck_constraints SET is_disabled=?,is_not_trusted=?,modify_date=CAST(current_timestamp AS TIMESTAMP) WHERE object_id=?",
        duckdb::params![disabled, untrusted, id],
    )?;
    Ok(())
}

/// The schema of a two-part name, or `dbo`; a leading database part must name
/// the current database.
pub(crate) fn split_name(name: &ObjectName, database: &str) -> Option<(String, String)> {
    let parts = name
        .0
        .iter()
        .map(|part| part.as_ident().map(|ident| ident.value.clone()))
        .collect::<Option<Vec<_>>>()?;
    match parts.as_slice() {
        [table] => Some(("dbo".into(), table.clone())),
        [schema, table] => Some((schema.clone(), table.clone())),
        [db, schema, table] if db.eq_ignore_ascii_case(database) => Some((
            if schema.is_empty() {
                "dbo".into()
            } else {
                schema.clone()
            },
            table.clone(),
        )),
        _ => None,
    }
}

/// A user table by schema and name (case-insensitive).
pub(crate) fn table(db: &Connection, schema: &str, name: &str) -> Result<Option<Table>> {
    let mut statement = db.prepare(
        "SELECT o.object_id,s.name,o.name FROM main.__msduck_objects o JOIN main.__msduck_schemas s USING(schema_id)
         WHERE o.type_code='U' AND lower(s.name)=lower(?) AND lower(o.name)=lower(?)",
    )?;
    let mut rows = statement.query_map([schema, name], |row| {
        Ok(Table {
            id: row.get(0)?,
            schema: row.get(1)?,
            name: row.get(2)?,
        })
    })?;
    Ok(rows.next().transpose()?)
}

/// A column of a user table, as `sys.columns` describes it.
#[derive(Clone, Debug)]
pub(crate) struct Column {
    pub name: String,
    pub id: i32,
    pub nullable: bool,
    pub user_type: i32,
    pub max_length: i32,
    pub precision: i32,
    pub scale: i32,
    pub computed: bool,
    pub persisted: bool,
    pub identity: bool,
}

pub(crate) fn columns(db: &Connection, table: &Table) -> Result<Vec<Column>> {
    let mut statement = db.prepare(
        "SELECT c.name,c.column_id,c.is_nullable,c.user_type_id,c.max_length,c.precision,c.scale,c.is_computed,
           coalesce((SELECT k.is_persisted FROM main.__msduck_computed_columns k WHERE k.object_id=c.object_id AND k.name_key=lower(c.name)),false),
           c.is_identity
         FROM sys.columns c WHERE c.object_id=? ORDER BY c.column_id",
    )?;
    let rows = statement.query_map([table.id], |row| {
        Ok(Column {
            name: row.get(0)?,
            id: row.get(1)?,
            nullable: row.get(2)?,
            user_type: row.get(3)?,
            max_length: row.get::<_, i64>(4)? as i32,
            precision: row.get::<_, i64>(5)? as i32,
            scale: row.get::<_, i64>(6)? as i32,
            computed: row.get(7)?,
            persisted: row.get(8)?,
            identity: row.get(9)?,
        })
    })?;
    Ok(rows.collect::<duckdb::Result<_>>()?)
}

/// Native column defaults, by lower-cased column name.
pub(crate) fn native_defaults(
    db: &Connection,
    table: &Table,
) -> Result<std::collections::HashMap<String, String>> {
    let mut statement = db.prepare(
        "SELECT column_name,column_default FROM duckdb_columns() WHERE database_name=current_database()
         AND schema_name=? AND table_name=? AND column_default IS NOT NULL",
    )?;
    let rows = statement.query_map([&table.schema, &table.name], |row| {
        Ok((
            row.get::<_, String>(0)?.to_lowercase(),
            row.get::<_, String>(1)?,
        ))
    })?;
    Ok(rows.collect::<duckdb::Result<_>>()?)
}

/// Native PRIMARY KEY and UNIQUE column sets of a table, primary key first.
pub(crate) fn native_keys(db: &Connection, table: &Table) -> Result<Vec<(bool, Vec<String>)>> {
    let mut statement = db.prepare(
        "SELECT constraint_type='PRIMARY KEY',constraint_column_names FROM duckdb_constraints()
         WHERE database_name=current_database() AND schema_name=? AND table_name=?
           AND constraint_type IN ('PRIMARY KEY','UNIQUE')
         ORDER BY constraint_type='PRIMARY KEY' DESC,constraint_index",
    )?;
    let rows = statement.query_map([&table.schema, &table.name], |row| {
        Ok((row.get::<_, bool>(0)?, strings(row.get(1)?)))
    })?;
    let mut keys = rows.collect::<duckdb::Result<Vec<_>>>()?;
    // Key constraints the keys feature records, including those it enforces
    // through its own indexes. Unique indexes are not candidate keys here:
    // DROP INDEX does not know about foreign keys.
    let mut statement = db.prepare(
        "SELECT kind='PK',key_columns FROM main.__msduck_keys
         WHERE object_id=? AND kind IN ('PK','UQ') ORDER BY kind='PK' DESC,tag",
    )?;
    let rows = statement.query_map([table.id], |row| {
        Ok((row.get::<_, bool>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (primary, columns) = row?;
        let columns: Vec<String> = serde_json::from_str(&columns).unwrap_or_default();
        if !keys
            .iter()
            .any(|(p, c)| *p == primary && same_columns(c, &columns))
        {
            keys.push((primary, columns));
        }
    }
    keys.sort_by_key(|(primary, _)| !primary);
    Ok(keys)
}

fn same_columns(a: &[String], b: &[String]) -> bool {
    a.len() == b.len()
        && a.iter()
            .all(|x| b.iter().any(|y| x.eq_ignore_ascii_case(y)))
}

/// Whether an object of any type named `name` exists in the schema.
pub(crate) fn object_exists(db: &Connection, schema: &str, name: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sys.objects o JOIN main.__msduck_schemas s USING(schema_id)
         WHERE lower(s.name)=lower(?) AND lower(o.name)=lower(?))",
        [schema, name],
        |row| row.get(0),
    )?)
}

/// A named DEFAULT constraint: its object id, table and column name.
pub(crate) struct Default {
    pub id: i32,
    pub table: i32,
    pub column: String,
}

pub(crate) fn default_named(db: &Connection, schema: &str, name: &str) -> Result<Option<Default>> {
    let mut statement = db.prepare(
        "SELECT d.object_id,d.parent_object_id,k.name FROM main.__msduck_default_constraints d
         JOIN main.__msduck_objects o ON o.object_id=d.parent_object_id
         JOIN main.__msduck_schemas s ON s.schema_id=o.schema_id
         JOIN main.__msduck_column_info k ON k.object_id=d.parent_object_id AND k.column_id=d.column_id
         WHERE lower(s.name)=lower(?) AND lower(d.name)=lower(?)",
    )?;
    let mut rows = statement.query_map([schema, name], |row| {
        Ok(Default {
            id: row.get(0)?,
            table: row.get(1)?,
            column: row.get(2)?,
        })
    })?;
    Ok(rows.next().transpose()?)
}

/// Named DEFAULT constraints of a table: (name, column).
pub(crate) fn defaults_of(db: &Connection, table: &Table) -> Result<Vec<(i32, String, String)>> {
    let mut statement = db.prepare(
        "SELECT d.object_id,d.name,k.name FROM main.__msduck_default_constraints d
         JOIN main.__msduck_column_info k ON k.object_id=d.parent_object_id AND k.column_id=d.column_id
         WHERE d.parent_object_id=? ORDER BY d.object_id",
    )?;
    let rows = statement.query_map([table.id], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
    })?;
    Ok(rows.collect::<duckdb::Result<_>>()?)
}

/// Record a named column DEFAULT of ALTER TABLE ... ADD. The statement that
/// added the column has already given its DEFAULT an object under SQL
/// Server's generated name (object_catalog), which takes the declared name.
pub(crate) fn record_default(
    db: &Connection,
    table: &Table,
    column: &str,
    name: &str,
    value: &sqlparser::ast::Expr,
) -> Result<i32> {
    let column_id: i32 = db.query_row(
        "SELECT column_id FROM main.__msduck_column_info WHERE object_id=? AND lower(name)=lower(?)",
        duckdb::params![table.id, column],
        |row| row.get(0),
    )?;
    let existing: Option<i32> = db.query_row(
        "SELECT max(object_id) FROM main.__msduck_default_constraints WHERE parent_object_id=? AND column_id=?",
        duckdb::params![table.id, column_id],
        |row| row.get(0),
    )?;
    let id = match existing {
        Some(id) => {
            db.execute(
                "UPDATE main.__msduck_default_constraints SET name=? WHERE object_id=?",
                duckdb::params![name, id],
            )?;
            db.execute(
                "DELETE FROM main.__msduck_default_sources WHERE object_id=?",
                [id],
            )?;
            id
        }
        None => {
            let id = next_object_id(db)?;
            db.execute(
                "INSERT INTO main.__msduck_default_constraints VALUES(?,?,?,?,CAST(current_timestamp AS TIMESTAMP),CAST(current_timestamp AS TIMESTAMP))",
                duckdb::params![id, table.id, column_id, name],
            )?;
            id
        }
    };
    record_default_source(db, id, value, false)?;
    Ok(id)
}

/// The declared text of a DEFAULT and whether SQL Server generated its
/// name, for the catalog views (src/engine/ext/catalog).
pub(crate) fn record_default_source(
    db: &Connection,
    id: i32,
    value: &sqlparser::ast::Expr,
    system_named: bool,
) -> Result<()> {
    db.execute(
        "INSERT INTO main.__msduck_default_sources VALUES(?,?,?)",
        duckdb::params![
            id,
            msduck_sql::dialect::ext::catalog::declarations::declared_source(value),
            system_named
        ],
    )?;
    Ok(())
}

pub(crate) fn delete_default(db: &Connection, id: i32) -> Result<()> {
    db.execute(
        "DELETE FROM main.__msduck_default_constraints WHERE object_id=?",
        [id],
    )?;
    Ok(())
}
