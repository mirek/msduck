//! Acquire operand declarations before binding. No database handle enters Resolver.
use duckdb::Connection;
use msduck_sql::aggregate_columns::{
    Columns, Snapshot, datetime2_type, datetimeoffset_type, prepare, resolve,
};
use sqlparser::ast::*;
use std::ops::ControlFlow;

/// Root adapter: capture catalog declarations for this compilation only.
pub fn annotate<T: VisitMut>(
    db: &Connection,
    value: &mut T,
    parameters: &std::collections::HashMap<String, crate::parameter::Parameter>,
) -> Result<(), String> {
    if !prepare(value) {
        return Ok(());
    }
    let catalog = acquire(db, value);
    resolve(&catalog, value, parameters)
}

/// Private images retain SQL declarations independently of native storage.
pub fn annotate_relations<T: VisitMut>(
    db: &Connection,
    value: &mut T,
    parameters: &std::collections::HashMap<String, crate::parameter::Parameter>,
    relations: &msduck_sql::aggregate_columns::Relations,
) -> Result<(), String> {
    if !prepare(value) {
        return Ok(());
    }
    let catalog = acquire(db, value);
    msduck_sql::aggregate_columns::resolve_with_relations(&catalog, relations, value, parameters)
}

pub(super) fn acquire<T: VisitMut>(db: &Connection, value: &mut T) -> Snapshot {
    struct Tables(Snapshot);
    impl VisitorMut for Tables {
        type Break = ();
        fn pre_visit_table_factor(&mut self, factor: &mut TableFactor) -> ControlFlow<()> {
            if let TableFactor::Table {
                name, args: None, ..
            } = factor
            {
                let names = name
                    .0
                    .iter()
                    .map(|part| part.as_ident().map(|id| id.value.to_lowercase()))
                    .collect::<Option<Vec<_>>>();
                if let Some(names) = names {
                    let key = match names.as_slice() {
                        [table] => Some(("dbo".to_string(), table.clone())),
                        [schema, table] => Some((schema.clone(), table.clone())),
                        _ => None,
                    };
                    if let Some(key) = key {
                        self.0.entry(key).or_insert_with(|| Ok(vec![]));
                    }
                }
            }
            ControlFlow::Continue(())
        }
    }
    let mut tables = Tables(Snapshot::new());
    let _ = value.visit(&mut tables);
    for ((schema, table), result) in &mut tables.0 {
        *result = columns(db, schema, table);
    }
    tables.0
}

fn columns(db: &Connection, schema: &str, table: &str) -> Result<Columns, String> {
    if schema.eq_ignore_ascii_case("sys") {
        let collation: Option<String> = db
            .query_row(
                "SELECT collation_name FROM sys.types WHERE name='nvarchar'",
                [],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        if let Some(fields) = collation
            .as_deref()
            .and_then(|collation| crate::query_catalog::system_catalog_fields(table, collation))
        {
            return Ok(fields
                .into_iter()
                .map(|field| {
                    (
                        field.name,
                        field
                            .info
                            .and_then(|info| info.logical_type())
                            .map(crate::sql_type::ast),
                    )
                })
                .collect());
        }
    }
    let mut statement = db.prepare("SELECT column_name, data_type FROM information_schema.columns WHERE table_catalog=current_database() AND table_schema=? COLLATE NOCASE AND table_name=? COLLATE NOCASE ORDER BY ordinal_position").map_err(|e| e.to_string())?;
    let columns = statement
        .query_map([schema, table], |row| {
            let name: String = row.get(0)?;
            let storage: String = row.get(1)?;
            Ok((
                name,
                if crate::variant_pack::storage_kind(&storage).is_some() {
                    Some(crate::variant_compare::kind())
                } else if storage == "BOOLEAN" {
                    Some(DataType::Bit(None))
                } else if let Some(kind) = crate::datetime2_cast::storage_kind(&storage) {
                    crate::datetime2_cast::storage_scale(&kind).map(datetime2_type)
                } else if let Some(kind) = crate::datetimeoffset_cast::storage_kind(&storage) {
                    crate::datetimeoffset_cast::storage_scale(&kind).map(datetimeoffset_type)
                } else {
                    crate::integer_conversion::storage_integer_type(&storage)
                },
            ))
        })
        .map_err(|e| e.to_string())?
        .collect::<duckdb::Result<Columns>>()
        .map_err(|e| e.to_string())?;
    let mut columns = columns;
    let mut time_columns = columns
        .iter()
        .map(|(name, _)| (name.clone(), String::new(), None))
        .collect::<Vec<_>>();
    crate::assignment::declared_targets(db, schema, table, &mut time_columns)
        .map_err(|e| e.to_string())?;
    for ((_, kind), (_, storage, _)) in columns.iter_mut().zip(time_columns) {
        if storage.starts_with("TIME(") || storage.starts_with("__MSDUCK_") {
            *kind = crate::assignment::storage_kind(&storage);
        }
    }
    let object = format!(
        "{}.{}",
        Ident::with_quote('"', schema),
        Ident::with_quote('"', table)
    );
    let mut declarations = db.prepare("SELECT name,CAST(system_type_id AS INTEGER),CAST(max_length AS INTEGER),CAST(precision AS INTEGER),CAST(scale AS INTEGER) FROM sys.columns WHERE object_id=__msduck_object_id(?,NULL) AND system_type_id IN (165,173,106,108,59,62,60,122,40,58,61)").map_err(|e|e.to_string())?;
    for row in declarations
        .query_map([object], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i32>(1)?,
                r.get::<_, i32>(2)?,
                r.get::<_, i32>(3)?,
                r.get::<_, i32>(4)?,
            ))
        })
        .map_err(|e| e.to_string())?
    {
        let (name, id, width, precision, scale) = row.map_err(|e| e.to_string())?;
        let length = Some(if width == -1 {
            BinaryLength::Max
        } else {
            BinaryLength::IntegerLength {
                length: width as u64,
            }
        });
        if let Some((_, kind)) = columns.iter_mut().find(|c| c.0.eq_ignore_ascii_case(&name)) {
            *kind = Some(
                if let Some(numeric) = crate::datalength::catalog_scalar(id, precision, scale) {
                    numeric
                } else if id == 165 {
                    DataType::Varbinary(length)
                } else {
                    DataType::Binary(Some(width as u64))
                },
            );
        }
    }
    Ok(columns)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlparser::parser::Parser;

    fn parse(sql: &str) -> Statement {
        Parser::parse_sql(&crate::dialect::ServerDialect, sql)
            .unwrap()
            .remove(0)
    }

    #[test]
    fn snapshots_remain_owned_and_refresh_after_ddl() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        let (_, ok) = session.batch_response(
            "CREATE TABLE dbo.snapshot_types(n SMALLINT, m MONEY, b VARBINARY(7), t TIME(2))",
            &Default::default(),
            false,
            None,
        );
        assert!(ok);
        let db = server.connection().unwrap();
        let mut query = parse("SELECT SUM(n),MAX(m),MAX(b),MAX(t) FROM dbo.snapshot_types");
        let before = acquire(&db, &mut query);
        let key = ("dbo".into(), "snapshot_types".into());
        let columns = before[&key].as_ref().unwrap();
        assert_eq!(columns[0].1, Some(DataType::SmallInt(None)));
        assert_eq!(
            msduck_sql::money_cast::money_type(columns[1].1.as_ref().unwrap()),
            Some(msduck_core::money::MoneyType::Money)
        );
        assert_eq!(columns[2].1.as_ref().unwrap().to_string(), "VARBINARY(7)");
        assert_eq!(
            columns[3].1,
            Some(DataType::Time(Some(2), TimezoneInfo::None))
        );
        let (_, ok) = session.batch_response(
            "ALTER TABLE dbo.snapshot_types ALTER COLUMN n BIGINT",
            &Default::default(),
            false,
            None,
        );
        assert!(ok);
        let after = acquire(&db, &mut query);
        assert_eq!(
            after[&key].as_ref().unwrap()[0].1,
            Some(DataType::BigInt(None))
        );
        assert_eq!(
            before[&key].as_ref().unwrap()[0].1,
            Some(DataType::SmallInt(None))
        );
        drop(db);
        drop(session);
        drop(server);
        // Binding needs only the captured values even after the database closes.
        resolve(&before, &mut query, &Default::default()).unwrap();
    }
}
