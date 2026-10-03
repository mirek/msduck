//! User tables and their columns, as the key feature needs them.
use anyhow::Result;
use duckdb::Connection;
use msduck_sql::dialect::ext::keys::value::Column;
use sqlparser::ast::{ObjectName, ObjectNamePart};

#[derive(Clone, Debug)]
pub(in crate::engine::ext) struct Table {
    pub object_id: i32,
    pub schema: String,
    pub name: String,
    pub columns: Vec<Column>,
}

impl Table {
    /// `schema.table`, as SQL Server names it in key messages.
    pub fn qualified(&self) -> String {
        format!("{}.{}", self.schema, self.name)
    }
    pub fn column(&self, name: &str) -> Option<&Column> {
        self.columns
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(name))
    }
    /// The DuckDB name of the table.
    pub fn backend(&self) -> String {
        format!("{}.{}", quote(&self.schema), quote(&self.name))
    }
}

pub(in crate::engine::ext) fn quote(name: &str) -> String {
    sqlparser::ast::Ident::with_quote('"', name).to_string()
}

/// The schema and table parts of a one- or two-part name; `None` for
/// database-qualified or other names.
pub(in crate::engine::ext) fn parts(name: &ObjectName) -> Option<(Option<String>, String)> {
    let idents: Vec<&str> = name
        .0
        .iter()
        .map(|part| match part {
            ObjectNamePart::Identifier(ident) => Some(ident.value.as_str()),
            _ => None,
        })
        .collect::<Option<_>>()?;
    match idents.as_slice() {
        [table] => Some((None, (*table).to_owned())),
        [schema, table] => Some((Some((*schema).to_owned()), (*table).to_owned())),
        _ => None,
    }
}

/// Resolve a user table by name in the connection's current database.
pub(in crate::engine::ext) fn resolve(db: &Connection, name: &ObjectName) -> Result<Option<Table>> {
    let Some((schema, table)) = parts(name) else {
        return Ok(None);
    };
    let schema = schema.unwrap_or_else(|| "dbo".into());
    let mut statement = db.prepare(
        "SELECT o.object_id FROM sys.objects o JOIN sys.schemas s USING(schema_id)
         WHERE rtrim(o.type)='U' AND lower(s.name)=lower(?) AND lower(o.name)=lower(?)",
    )?;
    let mut rows = statement.query_map([&schema, &table], |r| r.get::<_, i32>(0))?;
    let Some(object_id) = rows.next().transpose()? else {
        return Ok(None);
    };
    by_id(db, object_id)
}

pub(in crate::engine::ext) fn by_id(db: &Connection, object_id: i32) -> Result<Option<Table>> {
    let mut statement = db.prepare(
        "SELECT s.name,o.name FROM sys.objects o JOIN sys.schemas s USING(schema_id)
         WHERE o.object_id=? AND rtrim(o.type)='U'",
    )?;
    let mut rows = statement.query_map([object_id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    let Some((schema, name)) = rows.next().transpose()? else {
        return Ok(None);
    };
    let columns = db
        .prepare(
            "SELECT c.name,CAST(c.system_type_id AS INTEGER),CAST(c.max_length AS INTEGER),
               CAST(coalesce(c.scale,0) AS INTEGER),c.is_nullable,d.data_type,c.collation_name
             FROM sys.columns c JOIN duckdb_columns() d
               ON d.database_name=current_database() AND lower(d.schema_name)=lower(?)
               AND lower(d.table_name)=lower(?) AND lower(d.column_name)=lower(c.name)
             WHERE c.object_id=? ORDER BY c.column_id",
        )?
        .query_map(duckdb::params![schema, name, object_id], |r| {
            Ok(Column {
                name: r.get(0)?,
                system_type_id: r.get::<_, i32>(1)? as u8,
                max_length: r.get::<_, i32>(2)? as i16,
                scale: r.get::<_, i32>(3)? as u8,
                nullable: r.get(4)?,
                storage: r.get(5)?,
                collation: r.get(6)?,
            })
        })?
        .collect::<duckdb::Result<Vec<_>>>()?;
    Ok(Some(Table {
        object_id,
        schema,
        name,
        columns,
    }))
}
