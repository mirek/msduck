//! Persistent schema identities and live lookup functions.
use anyhow::Result;
use duckdb::Connection;
use sqlparser::ast::*;

pub fn register(db: &Connection) -> duckdb::Result<()> {
    db.execute_batch("CREATE SCHEMA IF NOT EXISTS sys;
        CREATE TABLE IF NOT EXISTS main.__msduck_schemas(name VARCHAR PRIMARY KEY, schema_id INTEGER UNIQUE NOT NULL, principal_id INTEGER NOT NULL);
        CREATE SEQUENCE IF NOT EXISTS main.__msduck_schema_ids START 5 MAXVALUE 2147483647 NO CYCLE;
        INSERT INTO main.__msduck_schemas VALUES ('dbo',1,1),('guest',2,2),('INFORMATION_SCHEMA',3,3),('sys',4,4) ON CONFLICT DO NOTHING;
        INSERT INTO main.__msduck_schemas SELECT schema_name,CAST(nextval('main.__msduck_schema_ids') AS INTEGER),1 FROM duckdb_schemas() s WHERE database_name=current_database() AND NOT internal AND lower(schema_name) NOT IN ('main','dbo','guest','sys','information_schema') AND NOT EXISTS(SELECT 1 FROM main.__msduck_schemas d WHERE lower(d.name)=lower(s.schema_name));
        CREATE OR REPLACE VIEW sys.schemas AS SELECT name,schema_id,principal_id FROM main.__msduck_schemas;
        CREATE OR REPLACE MACRO main.__msduck_schema_id(value) AS map_extract_value((SELECT map(list(lower(name)),list(schema_id)) FROM main.__msduck_schemas),lower(CAST(value AS VARCHAR)));
        CREATE OR REPLACE MACRO main.__msduck_schema_name(value) AS map_extract_value((SELECT map(list(schema_id),list(name)) FROM main.__msduck_schemas),value)")
}

pub fn execute(db: &Connection, statement: &Statement, autocommit: bool) -> Result<bool> {
    let (name, create) = match statement {
        Statement::CreateSchema {
            schema_name: SchemaName::Simple(name),
            ..
        } => (name, true),
        Statement::Drop {
            object_type: ObjectType::Schema,
            names,
            ..
        } if names.len() == 1 => (&names[0], false),
        _ => return Ok(false),
    };
    let [ObjectNamePart::Identifier(name)] = name.0.as_slice() else {
        anyhow::bail!("unsupported schema name")
    };
    if autocommit {
        db.execute_batch("BEGIN TRANSACTION")?;
    }
    let result = (|| -> Result<()> {
        db.execute_batch(&statement.to_string())?;
        if create {
            db.execute("INSERT INTO main.__msduck_schemas VALUES(?,CAST(nextval('main.__msduck_schema_ids') AS INTEGER),1)", [&name.value])?;
        } else {
            db.execute(
                "DELETE FROM main.__msduck_schemas WHERE lower(name)=lower(?)",
                [&name.value],
            )?;
        }
        if autocommit {
            db.execute_batch("COMMIT")?;
        }
        Ok(())
    })();
    if result.is_err() && autocommit {
        let _ = db.execute_batch("ROLLBACK");
    }
    result?;
    Ok(true)
}

pub fn lower(expr: &mut Expr) -> Result<(), String> {
    let Expr::Function(function) = expr else {
        return Ok(());
    };
    let name = function.name.to_string().to_ascii_uppercase();
    if !matches!(name.as_str(), "SCHEMA_ID" | "SCHEMA_NAME") {
        return Ok(());
    }
    let mut function = function.clone();
    if let FunctionArguments::List(args) = &mut function.args
        && args.args.is_empty()
    {
        let value = if name == "SCHEMA_ID" {
            Value::SingleQuotedString("dbo".into())
        } else {
            Value::Number("1".into(), false)
        };
        args.args
            .push(FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(
                value.into(),
            ))));
    }
    let mut value = crate::function_args::unary(&function, &name)?
        .ok_or_else(|| "invalid schema function".to_string())?
        .clone();
    if name == "SCHEMA_NAME" {
        value = crate::assignment::convert(value, &DataType::Int(None), false);
    }
    *expr =
        crate::engine::unary_function(&format!("__msduck_{}", name.to_ascii_lowercase()), value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schema_ids_survive_reopen_and_schema_ddl_rollback() {
        let path = std::env::temp_dir().join(format!(
            "msduck-schemas-{}-{}.duckdb",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let parse = |sql| {
            sqlparser::parser::Parser::parse_sql(&crate::dialect::ServerDialect, sql)
                .unwrap()
                .remove(0)
        };
        let db = Connection::open(&path).unwrap();
        db.execute_batch("CREATE SCHEMA existing").unwrap();
        register(&db).unwrap();
        let id: i32 = db
            .query_row("SELECT __msduck_schema_id('existing')", [], |r| r.get(0))
            .unwrap();
        assert!(id > 4);
        db.execute_batch("BEGIN").unwrap();
        execute(&db, &parse("DROP SCHEMA existing"), false).unwrap();
        assert_eq!(
            db.query_row("SELECT __msduck_schema_id('existing')", [], |r| r
                .get::<_, Option<i32>>(0))
                .unwrap(),
            None
        );
        db.execute_batch("ROLLBACK").unwrap();
        assert_eq!(
            db.query_row("SELECT __msduck_schema_id('existing')", [], |r| r
                .get::<_, i32>(0))
                .unwrap(),
            id
        );
        drop(db);
        let db = Connection::open(&path).unwrap();
        register(&db).unwrap();
        assert_eq!(
            db.query_row("SELECT __msduck_schema_id('existing')", [], |r| r
                .get::<_, i32>(0))
                .unwrap(),
            id
        );
        db.execute_batch("CREATE SEQUENCE calls").unwrap();
        let count:i64=db.query_row("SELECT count(*) FROM (SELECT __msduck_schema_name(CAST(nextval('calls') AS INTEGER)) AS name FROM range(6000)) WHERE name IS NOT NULL",[],|r|r.get(0)).unwrap();
        assert_eq!(count, 5);
        assert_eq!(
            db.query_row("SELECT currval('calls')", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            6000
        );
        execute(&db, &parse("DROP SCHEMA existing"), true).unwrap();
        execute(&db, &parse("CREATE SCHEMA existing"), true).unwrap();
        assert_ne!(
            db.query_row("SELECT __msduck_schema_id('existing')", [], |r| r
                .get::<_, i32>(0))
                .unwrap(),
            id
        );
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
}
