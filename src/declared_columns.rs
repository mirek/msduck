//! Retain SQL Server column declarations before backend type lowering erases them.
use sqlparser::ast::*;

pub use msduck_core::catalog::TypeMetadata as Info;
use msduck_sql::catalog_shape;

pub fn register(db: &duckdb::Connection) -> duckdb::Result<()> {
    db.execute_batch(include_str!("columns.sql"))
}

/// SQL Server collation names belong to the logical declaration catalog, never
/// DuckDB's physical column type (which may be a UTF-16 carrier STRUCT).
pub fn lower_collation(column: &mut ColumnDef) -> Result<(), String> {
    let Some(collation) = column_collation(column) else {
        return Ok(());
    };
    if !matches!(
        msduck_sql::sql_type::declaration(&column.data_type),
        Ok(msduck_core::types::Type::Character(_))
    ) {
        return Err("COLLATE requires a character column declaration".into());
    }
    if column
        .options
        .iter()
        .filter(|option| matches!(option.option, ColumnOption::Collation(_)))
        .count()
        != 1
    {
        return Err("multiple column collation declarations".into());
    }
    let name = collation_name(collation)?;
    if crate::tds::collation::Collation::for_name(&name).is_none() {
        return Err(format!("unsupported column collation {name}"));
    }
    column
        .options
        .retain(|option| !matches!(option.option, ColumnOption::Collation(_)));
    Ok(())
}

fn collation_name(name: &ObjectName) -> Result<String, String> {
    match name.0.as_slice() {
        [ObjectNamePart::Identifier(id)] => Ok(id.value.clone()),
        _ => Err("unsupported qualified column collation".into()),
    }
}

fn column_collation(column: &ColumnDef) -> Option<&ObjectName> {
    column
        .options
        .iter()
        .find_map(|option| match &option.option {
            ColumnOption::Collation(name) => Some(name),
            _ => None,
        })
}

type Declaration<'a> = (&'a Ident, &'a DataType, Option<&'a ObjectName>);
pub fn record(db: &duckdb::Connection, statement: &Statement) -> anyhow::Result<()> {
    let (name, columns): (&ObjectName, Vec<Declaration<'_>>) = match statement {
        Statement::CreateTable(table) => (
            &table.name,
            table
                .columns
                .iter()
                .map(|c| (&c.name, &c.data_type, column_collation(c)))
                .collect(),
        ),
        Statement::AlterTable(table) => (
            &table.name,
            table
                .operations
                .iter()
                .filter_map(|op| match op {
                    AlterTableOperation::AddColumn { column_def, .. } => Some((
                        &column_def.name,
                        &column_def.data_type,
                        column_collation(column_def),
                    )),
                    AlterTableOperation::AlterColumn {
                        column_name,
                        op: AlterColumnOperation::SetDataType { data_type, .. },
                    } => Some((column_name, data_type, None)),
                    _ => None,
                })
                .collect(),
        ),
        _ => return Ok(()),
    };
    let object_id: Option<i32> = db.query_row(
        "SELECT __msduck_object_id(?,NULL)",
        [name.to_string()],
        |r| r.get(0),
    )?;
    let Some(object_id) = object_id else {
        return Ok(());
    };
    for (column, kind, collation) in columns {
        let column_id: Option<i32> = db.query_row(
            "SELECT max(column_id) FROM main.__msduck_columns WHERE object_id=? AND lower(name)=lower(?)",
            duckdb::params![object_id, column.value],
            |r| r.get(0),
        )?;
        let Some(column_id) = column_id else { continue };
        db.execute(
            "DELETE FROM main.__msduck_declared_columns WHERE object_id=? AND column_id=?",
            duckdb::params![object_id, column_id],
        )?;
        if let Some(s) = catalog_shape::declaration(kind) {
            db.execute("INSERT INTO main.__msduck_declared_columns SELECT ?,?,system_type_id,user_type_id,coalesce(?,max_length),coalesce(?,precision),coalesce(?,scale),collation_name FROM sys.types WHERE name=?",duckdb::params![object_id,column_id,s.length,s.precision,s.scale,s.name])?;
            if let Some(collation) = collation {
                let name = collation_name(collation).map_err(anyhow::Error::msg)?;
                db.execute("UPDATE main.__msduck_declared_columns SET collation_name=? WHERE object_id=? AND column_id=?", duckdb::params![name,object_id,column_id])?;
            }
        }
    }
    Ok(())
}

/// Decode the catalog row at the database boundary.
pub fn read_info(row: &duckdb::Row<'_>, start: usize) -> duckdb::Result<Info> {
    Ok(Info {
        system_type_id: row.get(start)?,
        user_type_id: row.get(start + 1)?,
        max_length: row.get(start + 2)?,
        precision: row.get(start + 3)?,
        scale: row.get(start + 4)?,
        collation_name: row.get(start + 5)?,
    })
}
/// Encode metadata only when persisting it; inference never handles backend values.
pub fn info_values(info: &Info) -> [duckdb::types::Value; 6] {
    use duckdb::types::Value;
    [
        info.system_type_id.map_or(Value::Null, Value::UTinyInt),
        info.user_type_id.map_or(Value::Null, Value::Int),
        info.max_length.map_or(Value::Null, Value::SmallInt),
        info.precision.map_or(Value::Null, Value::UTinyInt),
        info.scale.map_or(Value::Null, Value::UTinyInt),
        info.collation_name.clone().map_or(Value::Null, Value::Text),
    ]
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_metadata_round_trips_types_aliases_and_unknowns() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let db = server.connection().unwrap();
        let mut statement = db.prepare("SELECT system_type_id,user_type_id,max_length,precision,scale,collation_name FROM sys.types").unwrap();
        let mut metadata = statement
            .query_map([], |row| read_info(row, 0))
            .unwrap()
            .collect::<duckdb::Result<Vec<_>>>()
            .unwrap();
        assert!(
            metadata
                .iter()
                .any(|info| info.system_type_id != info.user_type_id.map(|id| id as u8))
        );
        assert!(metadata.iter().any(|info| info.max_length == Some(-1)));
        assert!(metadata.iter().any(|info| info.collation_name.is_some()));
        metadata.push(Info::default());
        for info in metadata {
            let restored = db
                .query_row(
                    "SELECT ?,?,?,?,?,?",
                    duckdb::params_from_iter(info_values(&info)),
                    |row| read_info(row, 0),
                )
                .unwrap();
            assert_eq!(restored, info);
        }
    }
    #[test]
    fn declarations_persist_and_rollback_without_losing_original_types() {
        let path = std::env::temp_dir().join(format!(
            "msduck-declared-{}-{}.duckdb",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let server = crate::server::Server::open(path.to_str().unwrap()).unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        let run = |s: &mut crate::engine::Session, sql| {
            assert!(
                s.batch_response(sql, &std::collections::HashMap::new(), false, None)
                    .1,
                "{sql}"
            )
        };
        run(
            &mut session,
            "CREATE TABLE dbo.definitions(n NVARCHAR(37),v VARCHAR(11)); BEGIN TRAN; ALTER TABLE dbo.definitions ALTER COLUMN n VARCHAR(7); ROLLBACK",
        );
        drop(session);
        drop(server);
        let server = crate::server::Server::open(path.to_str().unwrap()).unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        assert_eq!(session.db.query_row("SELECT user_type_id,max_length FROM sys.columns WHERE object_id=__msduck_object_id('definitions',NULL) AND name='n'",[],|r|Ok((r.get::<_,i32>(0)?,r.get::<_,i16>(1)?))).unwrap(),(231,74));
        assert_eq!(session.db.query_row("SELECT __msduck_columnproperty(__msduck_object_id('definitions',NULL),'n','precision')",[],|r|r.get::<_,i32>(0)).unwrap(),37);
        run(
            &mut session,
            "ALTER TABLE dbo.definitions DROP COLUMN v; ALTER TABLE dbo.definitions ADD v VARBINARY(23)",
        );
        assert_eq!(session.db.query_row("SELECT user_type_id,max_length,column_id FROM sys.columns WHERE object_id=__msduck_object_id('definitions',NULL) AND name='v'",[],|r|Ok((r.get::<_,i32>(0)?,r.get::<_,i16>(1)?,r.get::<_,i32>(2)?))).unwrap(),(165,23,3));
        run(&mut session, "DROP TABLE dbo.definitions");
        assert_eq!(
            session
                .db
                .query_row(
                    "SELECT count(*) FROM main.__msduck_declared_columns",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        drop(session);
        drop(server);
        std::fs::remove_file(path).unwrap();
    }
}
