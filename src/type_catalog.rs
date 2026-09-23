//! Built-in SQL Server type identities for metadata discovery.
use sqlparser::ast::*;

pub fn register(db: &duckdb::Connection) -> duckdb::Result<()> {
    db.execute_batch(include_str!("type_catalog.sql"))
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
