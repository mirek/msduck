//! Catalog acquisition for OUTPUT destinations. Writes use the normal INSERT path.
use anyhow::{Result, anyhow, bail, ensure};
use msduck_core::{diagnostic::SqlError, types::Type};
use msduck_sql::{binding_scope::Field, output::Sink};
use sqlparser::ast::*;

pub(crate) struct Bound {
    pub statement: Statement,
    pub declarations: Vec<(String, Type)>,
}

#[derive(Debug)]
pub(crate) struct Failed {
    pub operation: msduck_sql::output::Operation,
    message: String,
}
impl std::fmt::Display for Failed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
pub(crate) fn failed(
    error: anyhow::Error,
    operation: msduck_sql::output::Operation,
) -> anyhow::Error {
    let message = error.to_string();
    error.context(Failed { operation, message })
}

pub(crate) fn bind(db: &duckdb::Connection, sink: &Sink, fields: &[Field]) -> Result<Bound> {
    let name = sink.table.to_string();
    let mut query = db.prepare("SELECT s.name,o.name,o.type FROM sys.objects o JOIN sys.schemas s ON o.schema_id=s.schema_id WHERE o.object_id=__msduck_object_id(?,NULL)")?;
    let objects = query
        .query_map([&name], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<duckdb::Result<Vec<_>>>()?;
    let Some((schema, table, kind)) = objects.first() else {
        return Err(SqlError::new(208, 1, format!("Invalid object name '{name}'.")).into());
    };
    ensure!(
        kind.trim() == "U",
        "OUTPUT INTO requires a base table destination"
    );
    let mut query = db.prepare("SELECT name,is_identity FROM sys.columns WHERE object_id=__msduck_object_id(?,NULL) ORDER BY column_id")?;
    let columns = query
        .query_map([&name], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?))
        })?
        .collect::<duckdb::Result<Vec<_>>>()?;
    let selected = if let Some(names) = &sink.columns {
        let mut selected = Vec::new();
        for name in names {
            let Some(column) = columns
                .iter()
                .find(|column| column.0.eq_ignore_ascii_case(&name.value))
            else {
                return Err(SqlError::new(
                    207,
                    1,
                    format!("Invalid column name '{}'.", name.value),
                )
                .into());
            };
            ensure!(!column.1, "{}", crate::identity::EXPLICIT);
            ensure!(
                !selected
                    .iter()
                    .any(|other: &&(String, bool)| other.0.eq_ignore_ascii_case(&column.0)),
                "duplicate OUTPUT destination column"
            );
            selected.push(column);
        }
        selected
    } else {
        columns.iter().filter(|column| !column.1).collect()
    };
    if fields.len() > selected.len() {
        return Err(SqlError::syntax(121, 1, "The select list for the INSERT statement contains more items than the insert list. The number of SELECT values must match the number of INSERT columns.").into());
    }
    ensure!(
        fields.len() == selected.len(),
        "OUTPUT destination column count does not match projection"
    );
    let mut query = db.prepare("SELECT constraint_type,constraint_name FROM duckdb_constraints() WHERE database_name=current_database() AND schema_name=? AND ((table_name=? AND constraint_type IN ('CHECK','FOREIGN KEY')) OR (constraint_type='FOREIGN KEY' AND referenced_table=?)) ORDER BY table_name,constraint_index")?;
    if let Some(constraint) = query
        .query_map([schema, table, table], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .next()
    {
        let (kind, constraint) = constraint?;
        if kind == "CHECK" {
            return Err(SqlError::new(333, 1, format!("The target table '{name}' of the OUTPUT INTO clause cannot have any enabled check constraints or any enabled rules. Found check constraint or rule '{constraint}'.")).into());
        }
        bail!("OUTPUT INTO foreign-key destinations are not supported yet");
    }
    let declarations = fields
        .iter()
        .enumerate()
        .map(|(i, field)| {
            let kind = field
                .info
                .as_ref()
                .and_then(|info| info.logical_type())
                .ok_or_else(|| {
                    anyhow!(
                        "OUTPUT destination requires a known logical source type for column {}",
                        i + 1
                    )
                })?;
            Ok((format!("@__msduck_output_{i}"), kind))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut statement =
        msduck_sql::batch::parse("INSERT INTO __msduck_output_sink VALUES(NULL)")?.remove(0);
    let Statement::Insert(insert) = &mut statement else {
        unreachable!()
    };
    insert.table = TableObject::TableName(sink.table.clone());
    insert.columns = selected
        .iter()
        .map(|column| ObjectName::from(vec![Ident::with_quote('"', &column.0)]))
        .collect();
    let SetExpr::Values(values) = insert.source.as_mut().unwrap().body.as_mut() else {
        unreachable!()
    };
    values.rows[0].clear();
    values.rows[0].extend(
        declarations
            .iter()
            .map(|(name, _)| Expr::Identifier(Ident::new(name))),
    );
    Ok(Bound {
        statement,
        declarations,
    })
}
