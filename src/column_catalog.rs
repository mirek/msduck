//! Stable table column IDs, with live identity and nullability properties.
mod system_columns;

use duckdb::Connection;
use sqlparser::ast::*;

pub fn register(db: &Connection) -> anyhow::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS main.__msduck_columns(object_id INTEGER NOT NULL,column_id INTEGER NOT NULL,name VARCHAR NOT NULL,PRIMARY KEY(object_id,column_id));
        CREATE TABLE IF NOT EXISTS main.__msduck_declared_columns(object_id INTEGER NOT NULL,column_id INTEGER NOT NULL,system_type_id UTINYINT,user_type_id INTEGER,max_length SMALLINT,precision UTINYINT,scale UTINYINT,collation_name VARCHAR,PRIMARY KEY(object_id,column_id));
        CREATE TABLE IF NOT EXISTS main.__msduck_column_counters(object_id INTEGER PRIMARY KEY,max_column_id INTEGER NOT NULL);
        CREATE OR REPLACE VIEW main.__msduck_live_columns AS SELECT o.object_id,o.type_code,c.column_name AS name,CAST(c.ordinal_position AS INTEGER) AS ordinal,c.is_nullable='YES' AS is_nullable,__msduck_identity_sequence(c.column_default) IS NOT NULL AS is_identity FROM information_schema.columns c JOIN main.__msduck_schemas s ON lower(s.name)=lower(c.table_schema) JOIN main.__msduck_objects o ON o.schema_id=s.schema_id AND lower(o.name)=lower(c.table_name) WHERE c.table_catalog=current_database();
        CREATE OR REPLACE VIEW main.__msduck_column_info AS SELECT l.object_id,l.name,CASE WHEN l.type_code='V' THEN l.ordinal ELSE d.column_id END AS column_id,l.is_nullable,l.is_identity FROM main.__msduck_live_columns l LEFT JOIN main.__msduck_columns d ON d.object_id=l.object_id AND lower(d.name)=lower(l.name) WHERE l.type_code='V' OR d.column_id IS NOT NULL;
")?;
    system_columns::register(db)?;
    db.execute_batch(
        "CREATE OR REPLACE MACRO main.__msduck_col_name(obj,col) AS map_extract_value(
        (SELECT map(list(CAST(object_id AS VARCHAR)||chr(0)||CAST(column_id AS VARCHAR)),list(name))
         FROM (SELECT object_id,column_id,name FROM main.__msduck_column_info
               UNION ALL SELECT object_id,column_id,name FROM main.__msduck_builtin_columns)),
        CAST(obj AS VARCHAR)||chr(0)||CAST(col AS VARCHAR));",
    )?;
    Ok(())
}

pub fn sync(db: &Connection) -> duckdb::Result<()> {
    db.execute_batch("DELETE FROM main.__msduck_columns d WHERE NOT EXISTS(SELECT 1 FROM main.__msduck_live_columns l WHERE l.object_id=d.object_id AND l.type_code='U' AND lower(l.name)=lower(d.name));
        DELETE FROM main.__msduck_column_counters d WHERE NOT EXISTS(SELECT 1 FROM main.__msduck_objects o WHERE o.object_id=d.object_id AND o.type_code='U');
        INSERT INTO main.__msduck_column_counters SELECT object_id,0 FROM main.__msduck_objects o WHERE type_code='U' AND NOT EXISTS(SELECT 1 FROM main.__msduck_column_counters d WHERE d.object_id=o.object_id);
        INSERT INTO main.__msduck_columns SELECT l.object_id,CAST(d.max_column_id+row_number() OVER(PARTITION BY l.object_id ORDER BY l.ordinal) AS INTEGER),l.name FROM main.__msduck_live_columns l JOIN main.__msduck_column_counters d USING(object_id) WHERE l.type_code='U' AND NOT EXISTS(SELECT 1 FROM main.__msduck_columns c WHERE c.object_id=l.object_id AND lower(c.name)=lower(l.name));
        UPDATE main.__msduck_column_counters d SET max_column_id=c.maximum FROM (SELECT object_id,max(column_id) AS maximum FROM main.__msduck_columns GROUP BY object_id) c WHERE c.object_id=d.object_id AND c.maximum>d.max_column_id;
        DELETE FROM main.__msduck_declared_columns d WHERE NOT EXISTS(SELECT 1 FROM main.__msduck_column_info c WHERE c.object_id=d.object_id AND c.column_id=d.column_id);
        DELETE FROM main.__msduck_default_constraints d WHERE NOT EXISTS(SELECT 1 FROM main.__msduck_column_info c WHERE c.object_id=d.parent_object_id AND c.column_id=d.column_id)")
}

pub fn lower(expr: &mut Expr) -> Result<(), String> {
    let Expr::Function(function) = expr else {
        return Ok(());
    };
    let name = function.name.to_string().to_ascii_uppercase();
    let arity = match name.as_str() {
        "COL_NAME" => 2,
        "COLUMNPROPERTY" => 3,
        _ => return Ok(()),
    };
    let FunctionArguments::List(args) = &function.args else {
        return Err(format!("{name} requires {arity} arguments"));
    };
    if args.args.len() != arity {
        return Err(format!("{name} requires {arity} arguments"));
    }
    let values = args
        .args
        .iter()
        .map(|a| match a {
            FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => Ok(e.clone()),
            _ => Err(format!("{name} requires scalar arguments")),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut check = function.clone();
    if let FunctionArguments::List(args) = &mut check.args {
        args.args.truncate(1)
    }
    crate::function_args::unary(&check, &name)?;
    let mut values = values;
    values[0] = crate::assignment::convert(values[0].clone(), &DataType::Int(None), false);
    if name == "COL_NAME" {
        values[1] = crate::assignment::convert(values[1].clone(), &DataType::Int(None), false);
    }
    function.name = ObjectName::from(vec![Ident::new(format!(
        "__msduck_{}",
        name.to_ascii_lowercase()
    ))]);
    if let FunctionArguments::List(args) = &mut function.args {
        args.args = values
            .into_iter()
            .map(|e| FunctionArg::Unnamed(FunctionArgExpr::Expr(e)))
            .collect();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn column_high_water_survives_restart_and_arguments_run_once() {
        let path = std::env::temp_dir().join(format!(
            "msduck-columns-{}-{}.duckdb",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let run = |s: &mut crate::engine::Session, sql| {
            assert!(
                s.batch_response(sql, &std::collections::HashMap::new(), false, None)
                    .1,
                "{sql}"
            )
        };
        let server = crate::server::Server::open(path.to_str().unwrap()).unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        run(
            &mut session,
            "CREATE TABLE dbo.ids(a INT,b INT,c INT); ALTER TABLE dbo.ids DROP COLUMN c",
        );
        session
            .db
            .execute_batch("CREATE SEQUENCE calls; CREATE SEQUENCE property_calls")
            .unwrap();
        assert_eq!(session.db.query_row("SELECT count(*) FROM range(6000) WHERE __msduck_col_name(__msduck_object_id('ids',NULL),CASE WHEN nextval('calls')%2=0 THEN 1 ELSE NULL END) IS NOT NULL",[],|r|r.get::<_,i64>(0)).unwrap(),3000);
        assert_eq!(
            session
                .db
                .query_row("SELECT currval('calls')", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            6000
        );
        assert_eq!(session.db.query_row("SELECT count(*) FROM range(6000) WHERE __msduck_columnproperty(__msduck_object_id('ids',NULL),CASE WHEN nextval('property_calls')%2=0 THEN 'a' ELSE NULL END,'columnid') IS NOT NULL",[],|r|r.get::<_,i64>(0)).unwrap(),3000);
        assert_eq!(
            session
                .db
                .query_row("SELECT currval('property_calls')", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            6000
        );
        session
            .db
            .execute_batch("CREATE SEQUENCE precision_calls")
            .unwrap();
        assert_eq!(session.db.query_row("SELECT count(*) FROM range(6000) WHERE __msduck_columnproperty(__msduck_object_id('ids',NULL),'a',CASE WHEN nextval('precision_calls')%2=0 THEN 'precision' ELSE 'unknown' END)=10",[],|r|r.get::<_,i64>(0)).unwrap(),3000);
        assert_eq!(
            session
                .db
                .query_row("SELECT currval('precision_calls')", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            6000
        );
        drop(session);
        drop(server);
        let server = crate::server::Server::open(path.to_str().unwrap()).unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        assert_eq!(
            session
                .db
                .query_row(
                    "SELECT max_column_id_used FROM sys.tables WHERE name='ids'",
                    [],
                    |r| r.get::<_, i32>(0)
                )
                .unwrap(),
            3
        );
        run(&mut session, "ALTER TABLE dbo.ids ADD d INT");
        assert_eq!(
            session
                .db
                .query_row(
                    "SELECT max_column_id_used FROM sys.tables WHERE name='ids'",
                    [],
                    |r| r.get::<_, i32>(0)
                )
                .unwrap(),
            4
        );
        assert_eq!(
            session
                .db
                .query_row(
                    "SELECT __msduck_columnproperty(__msduck_object_id('ids',NULL),'d','columnid')",
                    [],
                    |r| r.get::<_, i32>(0)
                )
                .unwrap(),
            4
        );
        drop(session);
        drop(server);
        std::fs::remove_file(path).unwrap();
    }
}
