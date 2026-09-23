//! Live identity-definition lookup with NUMERIC(38,0) results.
use duckdb::{
    core::{DataChunkHandle, Inserter, LogicalTypeId as Id},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use sqlparser::{ast::*, dialect::MsSqlDialect, parser::Parser, tokenizer::Token};

fn key(value: &str) -> Option<String> {
    qualified_key(value, "dbo")
}
fn qualified_key(value: &str, default_schema: &str) -> Option<String> {
    let mut parser = Parser::new(&MsSqlDialect {}).try_with_sql(value).ok()?;
    let name = parser.parse_object_name(false).ok()?;
    if parser.peek_token().token != Token::EOF {
        return None;
    }
    let parts = name
        .0
        .iter()
        .map(|p| p.as_ident().map(|id| id.value.clone()))
        .collect::<Option<Vec<_>>>()?;
    let (schema, table) = match parts.as_slice() {
        [table] => (default_schema, table.as_str()),
        [schema, table] => (schema.as_str(), table.as_str()),
        _ => return None,
    };
    Some(format!(
        "{}\0{}",
        schema.to_lowercase(),
        table.to_lowercase()
    ))
}
struct Text<const KIND: u8>;
impl<const KIND: u8> VScalar for Text<KIND> {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let source = input.flat_vector(0);
        let mut out = output.flat_vector();
        for row in 0..len {
            if source.row_is_null(row as u64) {
                out.set_null(row);
                continue;
            }
            let mut raw =
                unsafe { source.as_slice_with_len::<duckdb::ffi::duckdb_string_t>(len)[row] };
            let bytes = unsafe {
                std::slice::from_raw_parts(
                    duckdb::ffi::duckdb_string_t_data(&mut raw).cast::<u8>(),
                    duckdb::ffi::duckdb_string_t_length(raw) as usize,
                )
            };
            let text = std::str::from_utf8(bytes)?;
            let value = match KIND {
                1 => key(text),
                2 => qualified_key(text, "sys"),
                _ => crate::identity::sequence_name(Some(text)),
            };
            if let Some(value) = value {
                out.insert(row, value.as_str());
            } else {
                out.set_null(row);
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![Id::Varchar.into()],
            Id::Varchar.into(),
        )]
    }
}
pub fn register(db: &duckdb::Connection) -> duckdb::Result<()> {
    crate::identity::catalog(db)?;
    db.register_scalar_function::<Text<1>>("__msduck_identity_key")?;
    db.register_scalar_function::<Text<0>>("__msduck_identity_sequence")?;
    db.register_scalar_function::<Text<2>>("__msduck_type_key")?;
    for (name, column) in [("ident_seed", "seed"), ("ident_incr", "increment_value")] {
        db.execute_batch(&format!("CREATE OR REPLACE MACRO main.__msduck_{name}(value) AS map_extract_value((SELECT map(list(lower(c.table_schema)||chr(0)||lower(c.table_name)),list(CAST(d.{column} AS DECIMAL(38,0)))) FROM information_schema.columns c JOIN main.__msduck_identity_definitions d ON d.sequence_name=__msduck_identity_sequence(c.column_default) WHERE c.table_catalog=current_database()),__msduck_identity_key(CAST(value AS VARCHAR)))"))?;
    }
    db.execute_batch("CREATE OR REPLACE MACRO main.__msduck_ident_current(value) AS map_extract_value((SELECT map(list(lower(c.table_schema)||chr(0)||lower(c.table_name)),list(CAST(coalesce(s.last_value,d.seed) AS DECIMAL(38,0)))) FROM information_schema.columns c JOIN main.__msduck_identity_definitions d ON d.sequence_name=__msduck_identity_sequence(c.column_default) JOIN duckdb_sequences() s ON s.database_name=current_database() AND s.schema_name||'.'||s.sequence_name=d.sequence_name WHERE c.table_catalog=current_database()),__msduck_identity_key(CAST(value AS VARCHAR)))")?;
    Ok(())
}
pub fn lower(expr: &mut Expr) -> Result<(), String> {
    let Expr::Function(f) = expr else {
        return Ok(());
    };
    for name in ["IDENT_SEED", "IDENT_INCR", "IDENT_CURRENT"] {
        if let Some(value) = crate::function_args::unary(f, name)? {
            *expr = crate::engine::unary_function(
                &format!("__msduck_{}", name.to_lowercase()),
                value.clone(),
            );
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn bigint_allocation_matches_wide_arithmetic_at_boundaries() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        super::register(&db).unwrap();
        let seeds = [i64::MIN, i64::MIN + 1, -1, 0, 1, i64::MAX - 1, i64::MAX];
        let increments = [i64::MIN, -2, -1, 1, 2, i64::MAX];
        for (i, seed) in seeds.iter().enumerate() {
            for (j, increment) in increments.iter().enumerate() {
                let table = format!("boundary_{i}_{j}");
                let ddl = sqlparser::parser::Parser::parse_sql(
                    &sqlparser::dialect::MsSqlDialect {},
                    &format!("CREATE TABLE {table}(id BIGINT IDENTITY({seed},{increment}))"),
                )
                .unwrap()
                .remove(0);
                crate::identity::create(&db, &ddl, true).unwrap();
                let mut next = i128::from(*seed);
                let mut last = *seed;
                for _ in 0..4 {
                    let value = db.query_row(
                        &format!("INSERT INTO {table} DEFAULT VALUES RETURNING id"),
                        [],
                        |r| r.get::<_, i64>(0),
                    );
                    if let Ok(expected) = i64::try_from(next) {
                        assert_eq!(value.unwrap(), expected, "{seed}/{increment}");
                        last = expected;
                        next += i128::from(*increment);
                    } else {
                        assert!(value.is_err(), "{seed}/{increment}");
                    }
                    let current: String = db
                        .query_row(
                            "SELECT CAST(__msduck_ident_current(?) AS VARCHAR)",
                            [format!("main.{table}")],
                            |r| r.get(0),
                        )
                        .unwrap();
                    assert_eq!(current, last.to_string(), "{seed}/{increment}");
                }
            }
        }
    }
    #[test]
    fn current_after_wal_recovery() {
        const CHILD: &str = "MSDUCK_IDENTITY_WAL_PROBE";
        if let Ok(path) = std::env::var(CHILD) {
            let db = duckdb::Connection::open(path).unwrap();
            super::register(&db).unwrap();
            db.execute_batch("CREATE SCHEMA dbo").unwrap();
            let ddl = sqlparser::parser::Parser::parse_sql(
                &sqlparser::dialect::MsSqlDialect {},
                "CREATE TABLE dbo.ids(id INT IDENTITY(10,5))",
            )
            .unwrap()
            .remove(0);
            crate::identity::create(&db, &ddl, true).unwrap();
            db.execute_batch("CHECKPOINT; INSERT INTO dbo.ids DEFAULT VALUES; INSERT INTO dbo.ids DEFAULT VALUES").unwrap();
            for (name, seed, increment) in [("upper", i64::MAX, 1), ("lower", i64::MIN, -1)] {
                let ddl = sqlparser::parser::Parser::parse_sql(
                    &sqlparser::dialect::MsSqlDialect {},
                    &format!("CREATE TABLE dbo.{name}(id BIGINT IDENTITY({seed},{increment}))"),
                )
                .unwrap()
                .remove(0);
                crate::identity::create(&db, &ddl, true).unwrap();
                db.execute_batch(&format!("INSERT INTO dbo.{name} DEFAULT VALUES"))
                    .unwrap();
            }
            db.execute_batch("CREATE TABLE dbo.added(v INT); INSERT INTO dbo.added VALUES(1),(2)")
                .unwrap();
            let sqlparser::ast::Statement::AlterTable(alter) =
                sqlparser::parser::Parser::parse_sql(
                    &crate::dialect::ServerDialect,
                    "ALTER TABLE dbo.added ADD id INT IDENTITY(10,5)",
                )
                .unwrap()
                .remove(0)
            else {
                panic!()
            };
            crate::table_alter::execute(&db, &alter, true).unwrap();
            // Leave committed WAL without running Connection's orderly close.
            std::process::exit(0);
        }
        let path = std::env::temp_dir().join(format!(
            "msduck-identity-wal-{}-{}.duckdb",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "identity_metadata::tests::current_after_wal_recovery",
                "--exact",
            ])
            .env(CHILD, &path)
            .status()
            .unwrap();
        assert!(status.success());
        assert!(std::path::Path::new(&format!("{}.wal", path.display())).exists());
        let db = duckdb::Connection::open(&path).unwrap();
        super::register(&db).unwrap();
        let current: String = db
            .query_row(
                "SELECT CAST(__msduck_ident_current('ids') AS VARCHAR)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(current, "15");
        db.execute_batch("INSERT INTO dbo.ids DEFAULT VALUES")
            .unwrap();
        let current: String = db
            .query_row(
                "SELECT CAST(__msduck_ident_current('ids') AS VARCHAR)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(current, "20");
        assert_eq!(
            db.query_row(
                "SELECT min(id),max(id),count(DISTINCT id) FROM dbo.added",
                [],
                |r| Ok((
                    r.get::<_, i32>(0)?,
                    r.get::<_, i32>(1)?,
                    r.get::<_, i64>(2)?
                ))
            )
            .unwrap(),
            (10, 15, 2)
        );
        assert_eq!(
            db.query_row(
                "SELECT CAST(__msduck_ident_current('added') AS VARCHAR)",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "15"
        );
        db.execute_batch("INSERT INTO dbo.added(v) VALUES(3)")
            .unwrap();
        assert_eq!(
            db.query_row("SELECT id FROM dbo.added WHERE v=3", [], |r| r
                .get::<_, i32>(0))
                .unwrap(),
            20
        );
        for (name, expected) in [("upper", i64::MAX), ("lower", i64::MIN)] {
            for _ in 0..2 {
                let current: String = db
                    .query_row(
                        "SELECT CAST(__msduck_ident_current(?) AS VARCHAR)",
                        [name],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert_eq!(current, expected.to_string());
                assert!(
                    db.execute_batch(&format!("INSERT INTO dbo.{name} DEFAULT VALUES"))
                        .is_err()
                );
            }
        }
        drop(db);
        let db = duckdb::Connection::open(&path).unwrap();
        super::register(&db).unwrap();
        for (name, expected) in [("upper", i64::MAX), ("lower", i64::MIN)] {
            let current: String = db
                .query_row(
                    "SELECT CAST(__msduck_ident_current(?) AS VARCHAR)",
                    [name],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(current, expected.to_string());
            assert!(
                db.execute_batch(&format!("INSERT INTO dbo.{name} DEFAULT VALUES"))
                    .is_err()
            );
        }
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn catalog_lookup_evaluates_each_name_once_across_chunks() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        super::register(&db).unwrap();
        db.execute_batch("CREATE SCHEMA dbo; CREATE SEQUENCE name_calls START 1")
            .unwrap();
        let ddl = sqlparser::parser::Parser::parse_sql(
            &sqlparser::dialect::MsSqlDialect {},
            "CREATE TABLE dbo.ids(id INT IDENTITY(10,5))",
        )
        .unwrap()
        .remove(0);
        crate::identity::create(&db, &ddl, true).unwrap();
        let wrong:i64=db.query_row("SELECT count(*) FROM (SELECT __msduck_ident_seed(CASE WHEN nextval('name_calls')%2=0 THEN 'ids' ELSE NULL END) AS value FROM range(6000)) WHERE value IS NOT NULL AND value<>10",[],|r|r.get(0)).unwrap();
        assert_eq!(wrong, 0);
        let calls: i64 = db
            .query_row("SELECT currval('name_calls')", [], |r| r.get(0))
            .unwrap();
        assert_eq!(calls, 6000);
    }
}
