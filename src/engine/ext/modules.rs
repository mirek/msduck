//! Module definition store for procedures, functions and triggers.
//!
//! Each database keeps `main.__msduck_modules`, whose rows take object ids
//! from the same sequence as tables and views and appear in `sys.objects`
//! (and so in `OBJECT_ID`, `OBJECT_NAME` and `sys.all_objects`). Feature
//! modules own the meaning of `definition` (the original source text) and of
//! the free-form `properties` JSON.
#![allow(dead_code)] // Used by feature modules as their tasks land.
use super::Feature;
use anyhow::{Result, bail};
use duckdb::{Connection, OptionalExt, params};
use msduck_core::diagnostic::SqlError;

pub(super) struct Hooks;

impl Feature for Hooks {
    fn name(&self) -> &'static str {
        "modules"
    }
    fn bootstrap_database(&self, db: &Connection) -> Result<()> {
        // object_catalog::register has just (re)defined sys.objects over
        // tables, views, table types and default constraints. Keep that
        // definition under another name and extend it with module rows.
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS main.__msduck_modules(
                object_id INTEGER PRIMARY KEY,
                schema_id INTEGER NOT NULL,
                name VARCHAR NOT NULL,
                type_code VARCHAR NOT NULL,
                parent_object_id INTEGER NOT NULL DEFAULT 0,
                definition VARCHAR NOT NULL,
                is_disabled BOOLEAN NOT NULL DEFAULT false,
                properties VARCHAR NOT NULL DEFAULT '{}',
                create_date TIMESTAMP NOT NULL,
                modify_date TIMESTAMP NOT NULL,
                UNIQUE(schema_id, name));
             DROP VIEW IF EXISTS sys.__msduck_core_objects;
             ALTER VIEW sys.objects RENAME TO __msduck_core_objects;
             CREATE VIEW sys.objects AS
               SELECT * FROM sys.__msduck_core_objects
               UNION ALL
               SELECT name, object_id, CAST(NULL AS INTEGER), schema_id, parent_object_id,
                 rpad(type_code, 2, ' '),
                 CASE type_code
                   WHEN 'P' THEN 'SQL_STORED_PROCEDURE'
                   WHEN 'FN' THEN 'SQL_SCALAR_FUNCTION'
                   WHEN 'IF' THEN 'SQL_INLINE_TABLE_VALUED_FUNCTION'
                   WHEN 'TF' THEN 'SQL_TABLE_VALUED_FUNCTION'
                   WHEN 'TR' THEN 'SQL_TRIGGER'
                   ELSE type_code END,
                 create_date, modify_date, false, false, false
               FROM main.__msduck_modules;",
        )?;
        Ok(())
    }
}

/// SQL Server type codes of stored modules.
pub(crate) mod kind {
    pub const PROCEDURE: &str = "P";
    pub const SCALAR_FUNCTION: &str = "FN";
    pub const INLINE_FUNCTION: &str = "IF";
    pub const TABLE_FUNCTION: &str = "TF";
    pub const TRIGGER: &str = "TR";
}

/// A stored module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Module {
    pub object_id: i32,
    pub schema_id: i32,
    pub schema: String,
    pub name: String,
    pub type_code: String,
    /// The table of a trigger; 0 otherwise.
    pub parent_object_id: i32,
    pub definition: String,
    pub is_disabled: bool,
    pub properties: String,
}

const SELECT: &str = "SELECT m.object_id, m.schema_id, s.name, m.name, m.type_code, m.parent_object_id, m.definition, m.is_disabled, m.properties FROM main.__msduck_modules m JOIN main.__msduck_schemas s USING(schema_id)";

fn row(row: &duckdb::Row<'_>) -> duckdb::Result<Module> {
    Ok(Module {
        object_id: row.get(0)?,
        schema_id: row.get(1)?,
        schema: row.get(2)?,
        name: row.get(3)?,
        type_code: row.get(4)?,
        parent_object_id: row.get(5)?,
        definition: row.get(6)?,
        is_disabled: row.get(7)?,
        properties: row.get(8)?,
    })
}

/// The schema id of `schema` (`dbo` when `None`).
pub(crate) fn schema_id(db: &Connection, schema: Option<&str>) -> Result<i32> {
    let schema = schema.unwrap_or("dbo");
    let id: Option<i32> = db
        .query_row(
            "SELECT schema_id FROM main.__msduck_schemas WHERE lower(name) = lower(?)",
            [schema],
            |r| r.get(0),
        )
        .optional()?;
    match id {
        Some(id) => Ok(id),
        None => bail!(SqlError::new(
            2760,
            1,
            format!(
                "The specified schema name \"{schema}\" either does not exist or you do not have permission to use it."
            )
        )),
    }
}

/// Look a module up by (optional schema and) name, case-insensitively.
pub(crate) fn find(db: &Connection, schema: Option<&str>, name: &str) -> Result<Option<Module>> {
    let schema_id = match schema {
        Some(_) => schema_id(db, schema)?,
        None => schema_id(db, None)?,
    };
    Ok(db
        .query_row(
            &format!("{SELECT} WHERE m.schema_id = ? AND lower(m.name) = lower(?)"),
            params![schema_id, name],
            row,
        )
        .optional()?)
}

pub(crate) fn by_id(db: &Connection, object_id: i32) -> Result<Option<Module>> {
    Ok(db
        .query_row(&format!("{SELECT} WHERE m.object_id = ?"), [object_id], row)
        .optional()?)
}

/// Every module of the given type codes (all modules when empty), by id.
pub(crate) fn list(db: &Connection, type_codes: &[&str]) -> Result<Vec<Module>> {
    let mut statement = db.prepare(&format!("{SELECT} ORDER BY m.object_id"))?;
    let modules = statement
        .query_map([], row)?
        .collect::<duckdb::Result<Vec<_>>>()?;
    Ok(modules
        .into_iter()
        .filter(|m| type_codes.is_empty() || type_codes.contains(&m.type_code.as_str()))
        .collect())
}

/// Store a new module, failing with 2714 when the schema already has an
/// object of that name. Returns the new object id.
pub(crate) fn create(
    db: &Connection,
    schema: Option<&str>,
    name: &str,
    type_code: &str,
    parent_object_id: i32,
    definition: &str,
    properties: &str,
) -> Result<i32> {
    let schema_id = schema_id(db, schema)?;
    let exists: i64 = db.query_row(
        "SELECT count(*) FROM sys.objects WHERE schema_id = ? AND lower(name) = lower(?)",
        params![schema_id, name],
        |r| r.get(0),
    )?;
    if exists > 0 {
        bail!(SqlError::new(
            2714,
            6,
            format!("There is already an object named '{name}' in the database.")
        ));
    }
    Ok(db.query_row(
        "INSERT INTO main.__msduck_modules(object_id, schema_id, name, type_code, parent_object_id, definition, properties, create_date, modify_date)
         VALUES (CAST(nextval('main.__msduck_object_ids') AS INTEGER), ?, ?, ?, ?, ?, ?, CAST(current_timestamp AS TIMESTAMP), CAST(current_timestamp AS TIMESTAMP))
         RETURNING object_id",
        params![schema_id, name, type_code, parent_object_id, definition, properties],
        |r| r.get(0),
    )?)
}

/// Replace a module's definition and properties (ALTER), keeping its id.
pub(crate) fn alter(
    db: &Connection,
    object_id: i32,
    definition: &str,
    properties: &str,
) -> Result<()> {
    db.execute(
        "UPDATE main.__msduck_modules SET definition = ?, properties = ?, modify_date = CAST(current_timestamp AS TIMESTAMP) WHERE object_id = ?",
        params![definition, properties, object_id],
    )?;
    Ok(())
}

pub(crate) fn rename(db: &Connection, object_id: i32, name: &str) -> Result<()> {
    db.execute(
        "UPDATE main.__msduck_modules SET name = ?, modify_date = CAST(current_timestamp AS TIMESTAMP) WHERE object_id = ?",
        params![name, object_id],
    )?;
    Ok(())
}

pub(crate) fn set_disabled(db: &Connection, object_id: i32, disabled: bool) -> Result<()> {
    db.execute(
        "UPDATE main.__msduck_modules SET is_disabled = ? WHERE object_id = ?",
        params![disabled, object_id],
    )?;
    Ok(())
}

pub(crate) fn remove(db: &Connection, object_id: i32) -> Result<()> {
    db.execute(
        "DELETE FROM main.__msduck_modules WHERE object_id = ?",
        [object_id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> crate::engine::Session {
        let server = crate::server::Server::open(":memory:").unwrap();
        crate::engine::Session::new(server.connection().unwrap()).unwrap()
    }

    #[test]
    fn modules_get_object_ids_and_appear_in_sys_objects() {
        let session = session();
        let db = &session.db;
        let id = create(
            db,
            None,
            "p1",
            kind::PROCEDURE,
            0,
            "CREATE PROCEDURE p1 AS SELECT 1",
            "{}",
        )
        .unwrap();
        let (name, kind, desc): (String, String, String) = db
            .query_row(
                "SELECT name, type, type_desc FROM sys.objects WHERE object_id = ?",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            (name.as_str(), kind.as_str(), desc.as_str()),
            ("p1", "P ", "SQL_STORED_PROCEDURE")
        );
        let found: Option<i32> = db
            .query_row("SELECT __msduck_object_id('dbo.P1', 'P')", [], |r| r.get(0))
            .unwrap();
        assert_eq!(found, Some(id));
        let error = create(db, Some("dbo"), "P1", kind::SCALAR_FUNCTION, 0, "", "{}").unwrap_err();
        assert_eq!(error.downcast_ref::<SqlError>().unwrap().number, 2714);
        let module = find(db, None, "P1").unwrap().unwrap();
        assert_eq!((module.schema.as_str(), module.object_id), ("dbo", id));
        alter(db, id, "ALTER", "{\"x\":1}").unwrap();
        set_disabled(db, id, true).unwrap();
        let module = by_id(db, id).unwrap().unwrap();
        assert_eq!(
            (module.definition.as_str(), module.is_disabled),
            ("ALTER", true)
        );
        assert_eq!(list(db, &[kind::PROCEDURE]).unwrap().len(), 1);
        assert!(list(db, &[kind::TRIGGER]).unwrap().is_empty());
        remove(db, id).unwrap();
        assert!(find(db, None, "p1").unwrap().is_none());
    }
}
