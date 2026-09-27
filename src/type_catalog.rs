//! Built-in SQL Server type identities for metadata discovery.
use anyhow::{Context, Result, bail, ensure};
use duckdb::Connection;
use sqlparser::ast::*;

pub fn register(db: &duckdb::Connection) -> duckdb::Result<()> {
    db.execute_batch(
        "CREATE SEQUENCE IF NOT EXISTS main.__msduck_user_type_ids START 257 MAXVALUE 2147483647 NO CYCLE",
    )?;
    db.execute_batch(include_str!("type_catalog.sql"))
}

/// The declaration needed to register a disk-backed table type. The caller
/// supplies parsed types; this catalog adapter derives byte widths and type IDs.
#[allow(dead_code)] // CREATE TYPE execution is a separate, claimed integration gate.
pub(crate) struct TableColumn<'a> {
    pub name: &'a str,
    pub data_type: &'a DataType,
    pub nullable: bool,
    pub collation_name: Option<&'a str>,
}

#[allow(dead_code)]
fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.encode_utf16().count() <= 128 && !name.contains('\0')
}

#[allow(dead_code)]
fn type_object_name(name: &str, object_id: i32) -> String {
    let mut prefix = String::new();
    let mut used = 0;
    for ch in name.chars() {
        let width = ch.len_utf16();
        if used + width > 116 {
            break;
        }
        prefix.push(ch);
        used += width;
    }
    format!("TT_{prefix}_{object_id:08X}")
}

/// Writes all catalog rows in the caller's transaction. With `autocommit=true`,
/// the adapter owns a single transaction and rolls it back on any error.
#[allow(dead_code)]
pub(crate) fn create_table_type(
    db: &Connection,
    schema: &str,
    name: &str,
    columns: &[TableColumn<'_>],
    autocommit: bool,
) -> Result<(i32, i32)> {
    ensure!(
        valid_name(schema) && valid_name(name),
        "invalid table type name"
    );
    ensure!(
        !columns.is_empty() && columns.len() <= 1024,
        "table type needs 1 to 1024 columns"
    );
    for (index, column) in columns.iter().enumerate() {
        ensure!(valid_name(column.name), "invalid table type column name");
        ensure!(
            columns[..index]
                .iter()
                .all(|previous| !previous.name.eq_ignore_ascii_case(column.name)),
            "duplicate table type column name"
        );
    }
    if autocommit {
        db.execute_batch("BEGIN TRANSACTION")?;
    }
    let result = (|| -> Result<(i32, i32)> {
        let schema_id: Option<i32> = db.query_row(
            "SELECT max(schema_id) FROM main.__msduck_schemas WHERE lower(name)=lower(?)",
            [schema],
            |row| row.get(0),
        )?;
        let schema_id = schema_id.context("table type schema does not exist")?;
        let exists: i64 = db.query_row(
            "SELECT count(*) FROM sys.types WHERE schema_id=? AND lower(name)=lower(?)",
            duckdb::params![schema_id, name],
            |row| row.get(0),
        )?;
        ensure!(exists == 0, "table type already exists in schema");
        // Resolve every declaration before the first catalog write, including
        // when the caller owns an outer transaction and DuckDB has no savepoint.
        let mut prepared = Vec::with_capacity(columns.len());
        for column in columns {
            let shape = msduck_sql::catalog_shape::declaration(column.data_type)
                .context("unsupported table type column declaration")?;
            let builtin: (u8, i32, i16, u8, u8, Option<String>) = db.query_row(
                "SELECT system_type_id,user_type_id,max_length,precision,scale,collation_name FROM sys.types WHERE schema_id=4 AND lower(name)=lower(?) AND NOT is_table_type",
                [shape.name],
                |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?)),
            )?;
            let length = shape.length.unwrap_or(builtin.2);
            let precision = shape.precision.unwrap_or(builtin.3);
            let scale = shape.scale.unwrap_or(builtin.4);
            ensure!(
                length == -1 || length > 0,
                "invalid table type column length"
            );
            if length == -1 {
                ensure!(
                    matches!(builtin.0, 165 | 167 | 231 | 34 | 35 | 99 | 241),
                    "MAX is unsupported for this table type column"
                );
            } else if builtin.2 > 0 {
                ensure!(
                    length <= builtin.2,
                    "table type column length exceeds type limit"
                );
            }
            let character = matches!(builtin.0, 35 | 99 | 167 | 175 | 231 | 239);
            ensure!(
                column.collation_name.is_none() || character,
                "COLLATE requires a character column"
            );
            if let Some(collation) = column.collation_name {
                ensure!(
                    crate::tds::collation::Collation::for_name(collation).is_some(),
                    "unsupported table type column collation"
                );
            }
            let collation = column.collation_name.map(str::to_owned).or(builtin.5);
            let ansi_padded = matches!(builtin.0, 98 | 165 | 167 | 173 | 175 | 231 | 239);
            prepared.push((
                column.name,
                builtin.0,
                builtin.1,
                length,
                precision,
                scale,
                collation,
                column.nullable,
                ansi_padded,
            ));
        }
        let user_type_id: i32 = db.query_row(
            "SELECT CAST(nextval('main.__msduck_user_type_ids') AS INTEGER)",
            [],
            |row| row.get(0),
        )?;
        let object_id: i32 = db.query_row(
            "SELECT CAST(nextval('main.__msduck_object_ids') AS INTEGER)",
            [],
            |row| row.get(0),
        )?;
        let object_name = type_object_name(name, object_id);
        db.execute(
            "INSERT INTO main.__msduck_table_types VALUES(?,?,?,?,?,CAST(current_timestamp AS TIMESTAMP))",
            duckdb::params![user_type_id, name, schema_id, object_id, object_name],
        )?;
        for (
            index,
            (
                column_name,
                system_type_id,
                column_user_type_id,
                length,
                precision,
                scale,
                collation,
                nullable,
                ansi_padded,
            ),
        ) in prepared.into_iter().enumerate()
        {
            db.execute(
                "INSERT INTO main.__msduck_table_type_columns VALUES(?,?,?,?,?,?,?,?,?,?,?)",
                duckdb::params![
                    object_id,
                    i32::try_from(index + 1)?,
                    column_name,
                    system_type_id,
                    column_user_type_id,
                    length,
                    precision,
                    scale,
                    collation,
                    nullable,
                    ansi_padded
                ],
            )?;
        }
        Ok((user_type_id, object_id))
    })();
    if autocommit {
        match result {
            Ok(ids) => {
                db.execute_batch("COMMIT")?;
                Ok(ids)
            }
            Err(error) => {
                let _ = db.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    } else {
        result
    }
}

/// The executor must reject live dependencies before invoking this catalog
/// removal. There is no CREATE TYPE/DROP TYPE batch path yet.
#[allow(dead_code)]
pub(crate) fn drop_table_type(
    db: &Connection,
    schema: &str,
    name: &str,
    autocommit: bool,
) -> Result<()> {
    if autocommit {
        db.execute_batch("BEGIN TRANSACTION")?;
    }
    let result = (|| -> Result<()> {
        let object_id: Option<i32> = db.query_row(
            "SELECT max(type_table_object_id) FROM main.__msduck_table_types WHERE schema_id=(SELECT schema_id FROM main.__msduck_schemas WHERE lower(name)=lower(?)) AND lower(name)=lower(?)",
            duckdb::params![schema, name],
            |row| row.get(0),
        )?;
        let Some(object_id) = object_id else {
            bail!("table type does not exist")
        };
        db.execute(
            "DELETE FROM main.__msduck_table_type_columns WHERE object_id=?",
            [object_id],
        )?;
        db.execute(
            "DELETE FROM main.__msduck_table_types WHERE type_table_object_id=?",
            [object_id],
        )?;
        Ok(())
    })();
    if autocommit {
        match result {
            Ok(()) => {
                db.execute_batch("COMMIT")?;
                Ok(())
            }
            Err(error) => {
                let _ = db.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    } else {
        result
    }
}

pub fn lower(expr: &mut Expr) -> Result<(), String> {
    let Expr::Function(function) = expr else {
        return Ok(());
    };
    let name = function.name.to_string().to_ascii_uppercase();
    if !matches!(name.as_str(), "TYPE_ID" | "TYPE_NAME") {
        return Ok(());
    }
    let value = crate::function_args::unary(function, &name)?
        .unwrap()
        .clone();
    let value = if name == "TYPE_NAME" {
        crate::assignment::convert(value, &DataType::Int(None), false)
    } else {
        value
    };
    *expr =
        crate::engine::unary_function(&format!("__msduck_{}", name.to_ascii_lowercase()), value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{TableColumn, create_table_type, drop_table_type};
    use sqlparser::ast::Statement;

    fn columns() -> Vec<sqlparser::ast::ColumnDef> {
        let statement = sqlparser::parser::Parser::parse_sql(
            &crate::dialect::ServerDialect,
            "CREATE TABLE scratch (id INT NOT NULL, label NVARCHAR(12) NULL, payload VARBINARY(5) NULL)",
        )
        .unwrap()
        .remove(0);
        let Statement::CreateTable(table) = statement else {
            panic!("expected CREATE TABLE")
        };
        table.columns
    }

    #[test]
    fn table_type_catalog_matches_retained_relationships_and_survives_reopen() {
        let path = std::env::temp_dir().join(format!(
            "msduck-table-type-catalog-{}-{}.duckdb",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let definitions = columns();
        let specs = definitions
            .iter()
            .enumerate()
            .map(|(index, column)| TableColumn {
                name: &column.name.value,
                data_type: &column.data_type,
                nullable: index != 0,
                collation_name: None,
            })
            .collect::<Vec<_>>();
        let server = crate::server::Server::open(path.to_str().unwrap()).unwrap();
        let db = server.connection().unwrap();
        db.execute_batch("CREATE TABLE dbo.regular(x INTEGER)")
            .unwrap();
        crate::object_catalog::sync(&db).unwrap();
        let (user_type_id, object_id) =
            create_table_type(&db, "dbo", "CatalogProbe", &specs, true).unwrap();
        assert_eq!(user_type_id, 257);
        let create_schema = sqlparser::parser::Parser::parse_sql(
            &crate::dialect::ServerDialect,
            "CREATE SCHEMA app",
        )
        .unwrap()
        .remove(0);
        crate::schema_catalog::execute(&db, &create_schema, true).unwrap();
        let (app_type_id, app_object_id) =
            create_table_type(&db, "app", "CatalogProbe", &specs, true).unwrap();
        assert_eq!(app_type_id, 258);
        assert_ne!(app_object_id, object_id);
        assert_eq!(
            db.query_row(
                "SELECT schema_id FROM sys.types WHERE user_type_id=?",
                [app_type_id],
                |r| r.get::<_, i32>(0)
            )
            .unwrap(),
            5
        );
        assert_eq!(
            db.query_row("SELECT __msduck_type_id('app.CatalogProbe')", [], |r| r
                .get::<_, i32>(0))
                .unwrap(),
            app_type_id
        );
        let type_row: (u8, i32, i32, i16, bool, bool, bool) = db.query_row(
            "SELECT system_type_id,user_type_id,schema_id,max_length,is_nullable,is_user_defined,is_table_type FROM sys.types WHERE user_type_id=?",
            [user_type_id],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?)),
        ).unwrap();
        assert_eq!(type_row, (243, user_type_id, 1, -1, false, true, true));
        let table_row: (i32,bool) = db.query_row(
            "SELECT type_table_object_id,is_memory_optimized FROM sys.table_types WHERE user_type_id=?",
            [user_type_id],|r| Ok((r.get(0)?,r.get(1)?)),
        ).unwrap();
        assert_eq!(table_row, (object_id, false));
        let object_row: (String,i32,String,String,bool) = db.query_row(
            "SELECT name,schema_id,type,type_desc,is_ms_shipped FROM sys.objects WHERE object_id=?",
            [object_id],|r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)),
        ).unwrap();
        assert_eq!(
            object_row,
            (
                format!("TT_CatalogProbe_{object_id:08X}"),
                4,
                "TT".into(),
                "TYPE_TABLE".into(),
                true
            )
        );
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../reference/table-type-catalog.json")).unwrap();
        let reference = &fixture["runs"][0]["observations"];
        let expected = reference
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["name"] == "columns")
            .unwrap()["result"]["sets"][0]["rows"]
            .as_array()
            .unwrap();
        let mut statement = db.prepare("SELECT name,column_id,system_type_id,user_type_id,max_length,precision,scale,collation_name,is_nullable,is_ansi_padded FROM sys.columns WHERE object_id=? ORDER BY column_id").unwrap();
        let actual = statement
            .query_map([object_id], |r| {
                Ok(serde_json::json!([
                    r.get::<_, String>(0)?,
                    r.get::<_, i32>(1)?,
                    r.get::<_, u8>(2)?,
                    r.get::<_, i32>(3)?,
                    r.get::<_, i16>(4)?,
                    r.get::<_, u8>(5)?,
                    r.get::<_, u8>(6)?,
                    r.get::<_, Option<String>>(7)?,
                    r.get::<_, bool>(8)?,
                    r.get::<_, bool>(9)?
                ]))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect::<Vec<_>>();
        let expected = expected[..3]
            .iter()
            .map(|row| {
                serde_json::json!([
                    row[2], row[3], row[4], row[5], row[6], row[7], row[8], row[9], row[10],
                    row[11]
                ])
            })
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
        assert_eq!(
            db.query_row("SELECT __msduck_type_id('dbo.CatalogProbe')", [], |r| r
                .get::<_, i32>(0))
                .unwrap(),
            user_type_id
        );
        assert_eq!(
            db.query_row("SELECT __msduck_type_name(?)", [user_type_id], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "CatalogProbe"
        );
        assert_eq!(
            db.query_row("SELECT __msduck_col_name(?,2)", [object_id], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "label"
        );
        assert_eq!(db.query_row("SELECT count(*) FROM sys.columns c JOIN sys.objects o USING(object_id) WHERE o.name='regular'",[],|r|r.get::<_,i64>(0)).unwrap(),1);
        drop(statement);
        drop(db);
        drop(server);
        let server = crate::server::Server::open(path.to_str().unwrap()).unwrap();
        let db = server.connection().unwrap();
        assert_eq!(
            db.query_row(
                "SELECT type_table_object_id FROM sys.table_types WHERE user_type_id=?",
                [user_type_id],
                |r| r.get::<_, i32>(0)
            )
            .unwrap(),
            object_id
        );
        assert_eq!(
            db.query_row(
                "SELECT type_table_object_id FROM sys.table_types WHERE user_type_id=?",
                [app_type_id],
                |r| r.get::<_, i32>(0)
            )
            .unwrap(),
            app_object_id
        );
        drop_table_type(&db, "dbo", "CatalogProbe", true).unwrap();
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM sys.table_types WHERE user_type_id=?",
                [user_type_id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM sys.table_types WHERE user_type_id=?",
                [app_type_id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        drop_table_type(&db, "app", "CatalogProbe", true).unwrap();
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM sys.columns WHERE object_id=?",
                [object_id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        drop(db);
        drop(server);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn table_type_catalog_mutations_roll_back_and_reject_duplicates() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let db = server.connection().unwrap();
        let definitions = columns();
        let column = TableColumn {
            name: "id",
            data_type: &definitions[0].data_type,
            nullable: false,
            collation_name: None,
        };
        db.execute_batch("BEGIN TRANSACTION").unwrap();
        let (id, object) =
            create_table_type(&db, "dbo", "RollbackProbe", &[column], false).unwrap();
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM sys.table_types WHERE user_type_id=?",
                [id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        db.execute_batch("ROLLBACK").unwrap();
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM sys.table_types WHERE user_type_id=?",
                [id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM sys.objects WHERE object_id=?",
                [object],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        db.execute_batch("BEGIN TRANSACTION").unwrap();
        let invalid = TableColumn {
            name: "id",
            data_type: &definitions[0].data_type,
            nullable: false,
            collation_name: Some("SQL_Latin1_General_CP1_CI_AS"),
        };
        assert!(create_table_type(&db, "dbo", "InvalidProbe", &[invalid], false).is_err());
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM sys.table_types WHERE name='InvalidProbe'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        db.execute_batch("ROLLBACK").unwrap();
        let column = TableColumn {
            name: "id",
            data_type: &definitions[0].data_type,
            nullable: false,
            collation_name: None,
        };
        create_table_type(&db, "dbo", "RollbackProbe", &[column], true).unwrap();
        let column = TableColumn {
            name: "id",
            data_type: &definitions[0].data_type,
            nullable: false,
            collation_name: None,
        };
        assert!(create_table_type(&db, "dbo", "rollbackprobe", &[column], true).is_err());
        db.execute_batch("BEGIN TRANSACTION").unwrap();
        drop_table_type(&db, "dbo", "RollbackProbe", false).unwrap();
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM sys.table_types WHERE name='RollbackProbe'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        db.execute_batch("ROLLBACK").unwrap();
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM sys.table_types WHERE name='RollbackProbe'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
    }

    #[test]
    fn builtins_round_trip_and_volatile_arguments_run_once() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let db = server.connection().unwrap();
        assert_eq!(db.query_row("SELECT count(*) FROM sys.types WHERE __msduck_type_id(name)=user_type_id AND __msduck_type_name(user_type_id)=name",[],|r|r.get::<_,i64>(0)).unwrap(),34);
        db.execute_batch("CREATE SEQUENCE type_calls; CREATE SEQUENCE type_name_calls")
            .unwrap();
        assert_eq!(db.query_row("SELECT count(*) FROM range(6000) WHERE __msduck_type_id(CASE WHEN nextval('type_calls')%2=0 THEN 'int' ELSE NULL END)=56",[],|r|r.get::<_,i64>(0)).unwrap(),3000);
        assert_eq!(
            db.query_row("SELECT currval('type_calls')", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            6000
        );
        assert_eq!(db.query_row("SELECT count(*) FROM range(6000) WHERE __msduck_type_name(CASE WHEN nextval('type_name_calls')%2=0 THEN 56 ELSE NULL END)='int'",[],|r|r.get::<_,i64>(0)).unwrap(),3000);
        assert_eq!(
            db.query_row("SELECT currval('type_name_calls')", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            6000
        );
    }
}
