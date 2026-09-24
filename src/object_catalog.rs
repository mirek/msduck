//! Persistent IDs for user tables and views, synchronized with transactional DDL.
use anyhow::Result;
use duckdb::Connection;
use sqlparser::ast::*;

pub fn register(db: &Connection) -> duckdb::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS main.__msduck_objects(object_id INTEGER PRIMARY KEY, schema_id INTEGER NOT NULL, name VARCHAR NOT NULL, type_code VARCHAR NOT NULL, create_date TIMESTAMP NOT NULL, modify_date TIMESTAMP NOT NULL, UNIQUE(schema_id,name));
        CREATE TABLE IF NOT EXISTS main.__msduck_table_properties(object_id INTEGER PRIMARY KEY,lob_data_space_id INTEGER NOT NULL DEFAULT 0 CHECK(lob_data_space_id IN (0,1)));
        CREATE SEQUENCE IF NOT EXISTS main.__msduck_object_ids START 100000001 MAXVALUE 2147483647 NO CYCLE;
        CREATE OR REPLACE VIEW main.__msduck_live_objects AS SELECT s.schema_id,t.table_name AS name,CASE WHEN t.table_type='VIEW' THEN 'V' ELSE 'U' END AS type_code FROM information_schema.tables t JOIN main.__msduck_schemas s ON lower(s.name)=lower(t.table_schema) WHERE t.table_catalog=current_database() AND t.table_type IN ('BASE TABLE','VIEW') AND lower(t.table_schema) NOT IN ('main','sys','information_schema','temp');
        CREATE OR REPLACE VIEW sys.objects AS SELECT name,object_id,CAST(NULL AS INTEGER) AS principal_id,schema_id,CAST(0 AS INTEGER) AS parent_object_id,rpad(type_code,2,' ') AS type,CASE WHEN type_code='U' THEN 'USER_TABLE' ELSE 'VIEW' END AS type_desc,create_date,modify_date,false AS is_ms_shipped,false AS is_published,false AS is_schema_published FROM main.__msduck_objects;
        CREATE OR REPLACE MACRO main.__msduck_object_id(value,kind) AS map_extract_value((SELECT map(list(key),list(object_id)) FROM (SELECT lower(s.name)||chr(0)||lower(o.name)||chr(0)||k.kind AS key,o.object_id FROM main.__msduck_objects o JOIN main.__msduck_schemas s USING(schema_id) CROSS JOIN LATERAL (VALUES (''),(o.type_code)) k(kind))),__msduck_identity_key(CAST(value AS VARCHAR))||chr(0)||upper(rtrim(coalesce(CAST(kind AS VARCHAR),''))));
        CREATE OR REPLACE MACRO main.__msduck_object_name(value) AS map_extract_value((SELECT map(list(object_id),list(name)) FROM main.__msduck_objects),value);
        CREATE OR REPLACE MACRO main.__msduck_object_schema_name(value) AS map_extract_value((SELECT map(list(object_id),list(s.name)) FROM main.__msduck_objects o JOIN main.__msduck_schemas s USING(schema_id)),value)")?;
    crate::column_catalog::register(db)?;
    db.execute_batch(include_str!("table_catalog.sql"))?;
    sync(db)
}

pub fn sync(db: &Connection) -> duckdb::Result<()> {
    db.execute_batch("DELETE FROM main.__msduck_objects o WHERE NOT EXISTS(SELECT 1 FROM main.__msduck_live_objects l WHERE l.schema_id=o.schema_id AND lower(l.name)=lower(o.name) AND l.type_code=o.type_code);
        INSERT INTO main.__msduck_objects SELECT CAST(nextval('main.__msduck_object_ids') AS INTEGER),l.schema_id,l.name,l.type_code,CAST(current_timestamp AS TIMESTAMP),CAST(current_timestamp AS TIMESTAMP) FROM main.__msduck_live_objects l WHERE NOT EXISTS(SELECT 1 FROM main.__msduck_objects o WHERE o.schema_id=l.schema_id AND lower(o.name)=lower(l.name))")?;
    crate::column_catalog::sync(db)?;
    sync_lob(db)
}

/// Logical default-filegroup identity survives dropping the last MAX column.
/// Run again after recording declarations for a successful DDL statement. All
/// writes use the caller's transaction; a dropped/recreated object gets new state.
pub fn sync_lob(db: &Connection) -> duckdb::Result<()> {
    db.execute_batch("DELETE FROM main.__msduck_table_properties p WHERE NOT EXISTS(SELECT 1 FROM main.__msduck_objects o WHERE o.object_id=p.object_id AND o.type_code='U');
        INSERT INTO main.__msduck_table_properties SELECT object_id,0 FROM main.__msduck_objects o WHERE type_code='U' AND NOT EXISTS(SELECT 1 FROM main.__msduck_table_properties p WHERE p.object_id=o.object_id);
        UPDATE main.__msduck_table_properties p SET lob_data_space_id=1 WHERE lob_data_space_id=0 AND EXISTS(SELECT 1 FROM main.__msduck_declared_columns c WHERE c.object_id=p.object_id AND c.max_length=-1 AND c.system_type_id IN (165,167,231))")
}

pub fn is_ddl(statement: &Statement) -> bool {
    matches!(
        statement,
        Statement::CreateTable(_)
            | Statement::CreateView(_)
            | Statement::AlterView { .. }
            | Statement::AlterTable(_)
            | Statement::CreateIndex(_)
    ) || matches!(
        statement,
        Statement::Drop {
            object_type: ObjectType::Table | ObjectType::View,
            ..
        }
    )
}

pub fn touch(db: &Connection, statement: &Statement) -> Result<()> {
    let name = match statement {
        Statement::AlterTable(table) => &table.name,
        Statement::AlterView { name, .. } => name,
        Statement::CreateView(view) if view.or_replace || view.or_alter => &view.name,
        Statement::CreateIndex(index) => &index.table_name,
        _ => return Ok(()),
    };
    let parts = name
        .0
        .iter()
        .map(|p| p.as_ident().map(|i| i.value.as_str()))
        .collect::<Option<Vec<_>>>();
    let Some(parts) = parts else { return Ok(()) };
    let (schema, table) = match parts.as_slice() {
        [table] => ("dbo", *table),
        [schema, table] => (*schema, *table),
        _ => return Ok(()),
    };
    db.execute("UPDATE main.__msduck_objects SET modify_date=CAST(current_timestamp AS TIMESTAMP) WHERE schema_id=(SELECT schema_id FROM main.__msduck_schemas WHERE lower(name)=lower(?)) AND lower(name)=lower(?)",[schema,table])?;
    Ok(())
}

pub fn lower(expr: &mut Expr) -> Result<(), String> {
    let Expr::Function(f) = expr else {
        return Ok(());
    };
    let name = f.name.to_string().to_ascii_uppercase();
    if name == "OBJECT_ID" {
        let mut f = f.clone();
        let FunctionArguments::List(args) = &mut f.args else {
            return Err("OBJECT_ID requires one or two arguments".into());
        };
        let kind = match args.args.len() {
            1 => Expr::Value(Value::Null.into()),
            2 => match args.args.pop().unwrap() {
                FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => e,
                _ => return Err("OBJECT_ID requires scalar arguments".into()),
            },
            _ => return Err("OBJECT_ID requires one or two arguments".into()),
        };
        let value = crate::function_args::unary(&f, "OBJECT_ID")?
            .unwrap()
            .clone();
        *expr = crate::engine::binary_function("__msduck_object_id", value, kind);
    } else if matches!(name.as_str(), "OBJECT_NAME" | "OBJECT_SCHEMA_NAME") {
        let value = crate::function_args::unary(f, &name)?.unwrap().clone();
        *expr = crate::engine::unary_function(
            &format!("__msduck_{}", name.to_ascii_lowercase()),
            crate::assignment::convert(value, &DataType::Int(None), false),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn lob_identity_survives_column_drop_restart_and_transaction_rollback() {
        let path = std::env::temp_dir().join(format!(
            "msduck-lob-{}-{}.duckdb",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let run = |s: &mut crate::engine::Session, sql| {
            assert!(
                s.batch_response(sql, &Default::default(), false, None).1,
                "{sql}"
            );
        };
        let lob = |db: &duckdb::Connection, name: &str| {
            db.query_row(
                "SELECT lob_data_space_id FROM sys.tables WHERE name=?",
                [name],
                |r| r.get::<_, i32>(0),
            )
            .unwrap()
        };
        let server = crate::server::Server::open(path.to_str().unwrap()).unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        run(
            &mut session,
            "CREATE TABLE dbo.lob_history(id INT,payload VARCHAR(MAX)); ALTER TABLE dbo.lob_history DROP COLUMN payload; CREATE TABLE dbo.lob_pending(id INT)",
        );
        let old_id: i32 = session
            .db
            .query_row("SELECT __msduck_object_id('lob_history',NULL)", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(lob(&session.db, "lob_history"), 1);
        let observer = server.connection().unwrap();
        run(
            &mut session,
            "BEGIN TRAN; ALTER TABLE dbo.lob_pending ADD payload NVARCHAR(MAX)",
        );
        assert_eq!(lob(&session.db, "lob_pending"), 1);
        assert_eq!(lob(&observer, "lob_pending"), 0);
        run(&mut session, "ROLLBACK");
        assert_eq!(lob(&session.db, "lob_pending"), 0);
        drop(observer);
        drop(session);
        drop(server);

        let server = crate::server::Server::open(path.to_str().unwrap()).unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        assert_eq!(lob(&session.db, "lob_history"), 1);
        assert_eq!(lob(&session.db, "lob_pending"), 0);
        run(
            &mut session,
            "DROP TABLE dbo.lob_history; CREATE TABLE dbo.lob_history(id INT)",
        );
        assert_eq!(lob(&session.db, "lob_history"), 0);
        assert_eq!(
            session
                .db
                .query_row(
                    "SELECT count(*) FROM main.__msduck_table_properties WHERE object_id=?",
                    [old_id],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        drop(session);
        drop(server);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn object_ids_survive_restart_and_arguments_run_once() {
        let path = std::env::temp_dir().join(format!(
            "msduck-objects-{}-{}.duckdb",
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
        run(&mut session, "CREATE TABLE dbo.ids(id INT IDENTITY(10,5))");
        let id: i32 = session
            .db
            .query_row("SELECT __msduck_object_id('ids',NULL)", [], |r| r.get(0))
            .unwrap();
        run(&mut session, "BEGIN TRAN; DROP TABLE dbo.ids; ROLLBACK");
        assert_eq!(
            session
                .db
                .query_row("SELECT __msduck_object_id('ids',NULL)", [], |r| r
                    .get::<_, i32>(0))
                .unwrap(),
            id
        );
        let other = server.connection().unwrap();
        run(&mut session, "BEGIN TRAN; CREATE TABLE dbo.pending(v INT)");
        assert_eq!(
            other
                .query_row("SELECT __msduck_object_id('pending',NULL)", [], |r| r
                    .get::<_, Option<i32>>(0))
                .unwrap(),
            None
        );
        run(&mut session, "COMMIT");
        assert!(
            other
                .query_row("SELECT __msduck_object_id('pending',NULL)", [], |r| r
                    .get::<_, Option<i32>>(0))
                .unwrap()
                .is_some()
        );
        drop(other);
        session.db.execute_batch("CREATE SEQUENCE calls").unwrap();
        let count=session.db.query_row("SELECT count(*) FROM range(6000) WHERE __msduck_object_id(CASE WHEN nextval('calls')%2=0 THEN 'ids' ELSE NULL END,NULL) IS NOT NULL",[],|r|r.get::<_,i64>(0)).unwrap();
        assert_eq!(count, 3000);
        assert_eq!(
            session
                .db
                .query_row("SELECT currval('calls')", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            6000
        );
        session
            .db
            .execute_batch("CREATE SEQUENCE cross_calls")
            .unwrap();
        assert_eq!(session.db.query_row("SELECT count(*) FROM range(5) a CROSS JOIN range(7) b WHERE nextval('cross_calls')%2=0",[],|r|r.get::<_,i64>(0)).unwrap(),17);
        assert_eq!(
            session
                .db
                .query_row("SELECT currval('cross_calls')", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            35
        );
        drop(session);
        drop(server);
        let server = crate::server::Server::open(path.to_str().unwrap()).unwrap();
        let db = server.connection().unwrap();
        assert_eq!(
            db.query_row("SELECT __msduck_object_id('ids',NULL)", [], |r| r
                .get::<_, i32>(0))
                .unwrap(),
            id
        );
        drop(db);
        drop(server);
        std::fs::remove_file(path).unwrap();
    }
}
