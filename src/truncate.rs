//! Whole-table truncation with SQL Server's foreign-key restriction.
use anyhow::{Result, bail, ensure};
pub use msduck_sql::ddl_syntax::truncate as validate;
use sqlparser::ast::Truncate;

pub fn execute(db: &duckdb::Connection, statement: &Truncate, autocommit: bool) -> Result<()> {
    validate(statement)?;
    let target = &statement.table_names[0].name;
    let parts = target
        .0
        .iter()
        .map(|part| {
            part.as_ident()
                .map(|id| id.value.as_str())
                .ok_or_else(|| anyhow::anyhow!("unsupported table identifier"))
        })
        .collect::<Result<Vec<_>>>()?;
    let (schema, table) = match parts.as_slice() {
        [table] => ("dbo", *table),
        [schema, table] => (*schema, *table),
        _ => bail!("unsupported TRUNCATE target"),
    };
    if autocommit {
        db.execute_batch("BEGIN TRANSACTION")?;
    }
    let result = (|| -> Result<()> {
        let exists: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM duckdb_tables() WHERE database_name=current_database() AND schema_name=? COLLATE NOCASE AND table_name=? COLLATE NOCASE AND NOT internal)", [schema, table], |row| row.get(0))?;
        ensure!(exists, "Cannot truncate {target}: table does not exist");
        let referenced: bool = db.query_row("SELECT EXISTS(
            SELECT 1 FROM information_schema.referential_constraints r
            JOIN information_schema.table_constraints p ON
                p.constraint_catalog=r.unique_constraint_catalog AND
                p.constraint_schema=r.unique_constraint_schema AND p.constraint_name=r.unique_constraint_name
            JOIN information_schema.table_constraints c ON
                c.constraint_catalog=r.constraint_catalog AND c.constraint_schema=r.constraint_schema AND c.constraint_name=r.constraint_name
            WHERE p.table_catalog=current_database() AND p.table_schema=? COLLATE NOCASE AND p.table_name=? COLLATE NOCASE
                AND (p.table_schema<>c.table_schema OR p.table_name<>c.table_name))", [schema, table], |row| row.get(0))?;
        ensure!(
            !referenced,
            "Cannot truncate table {target} because it is referenced by a FOREIGN KEY constraint"
        );
        let resets = crate::identity::plan_reset(db, schema, table)?;
        db.execute(&statement.to_string(), [])?;
        crate::identity::reset(db, target, resets)?;
        if autocommit {
            db.execute_batch("COMMIT")?;
        }
        Ok(())
    })();
    if result.is_err() && autocommit {
        let _ = db.execute_batch("ROLLBACK");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlparser::{ast::Statement, dialect::MsSqlDialect, parser::Parser};

    #[test]
    fn identity_reset_is_transactional_and_persistent() {
        let path = std::env::temp_dir().join(format!(
            "msduck-truncate-{}-{}.duckdb",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let parse = |sql: &str| Parser::parse_sql(&MsSqlDialect {}, sql).unwrap().remove(0);
        let db = duckdb::Connection::open(&path).unwrap();
        crate::identity_metadata::register(&db).unwrap();
        db.execute_batch("CREATE SCHEMA dbo").unwrap();
        crate::identity::create(
            &db,
            &parse("CREATE TABLE dbo.ids(id INT IDENTITY(10,-3) PRIMARY KEY)"),
            true,
        )
        .unwrap();
        let Statement::Truncate(truncate) = parse("TRUNCATE TABLE dbo.ids") else {
            panic!()
        };
        db.execute_batch(
            "INSERT INTO dbo.ids DEFAULT VALUES; INSERT INTO dbo.ids DEFAULT VALUES; BEGIN",
        )
        .unwrap();
        execute(&db, &truncate, false).unwrap();
        db.execute_batch(
            "INSERT INTO dbo.ids DEFAULT VALUES; ROLLBACK; INSERT INTO dbo.ids DEFAULT VALUES",
        )
        .unwrap();
        assert_eq!(
            db.query_row("SELECT min(id) FROM dbo.ids", [], |r| r.get::<_, i32>(0))
                .unwrap(),
            4
        );
        for _ in 0..3 {
            execute(&db, &truncate, true).unwrap();
            assert_eq!(
                db.query_row("SELECT count(*) FROM duckdb_sequences()", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                1
            );
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM main.__msduck_identity_definitions",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
                1
            );
            db.execute_batch("INSERT INTO dbo.ids DEFAULT VALUES")
                .unwrap();
            assert_eq!(
                db.query_row("SELECT id FROM dbo.ids", [], |r| r.get::<_, i32>(0))
                    .unwrap(),
                10
            );
        }
        execute(&db, &truncate, true).unwrap();
        drop(db);
        let db = duckdb::Connection::open(&path).unwrap();
        crate::identity_metadata::register(&db).unwrap();
        assert_eq!(
            db.query_row(
                "SELECT CAST(__msduck_ident_current('ids') AS VARCHAR)",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "10"
        );
        db.execute_batch("INSERT INTO dbo.ids DEFAULT VALUES")
            .unwrap();
        assert_eq!(
            db.query_row("SELECT id FROM dbo.ids", [], |r| r.get::<_, i32>(0))
                .unwrap(),
            10
        );
        // A dependent object must prevent reset and preserve rows/allocation.
        let name: String = db
            .query_row(
                "SELECT schema_name||'.'||sequence_name FROM duckdb_sequences()",
                [],
                |r| r.get(0),
            )
            .unwrap();
        db.execute_batch(&format!(
            "CREATE TABLE dbo.dependency(id INT DEFAULT nextval('{name}'))"
        ))
        .unwrap();
        assert!(execute(&db, &truncate, true).is_err());
        assert_eq!(
            db.query_row("SELECT count(*) FROM dbo.ids", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        db.execute_batch("INSERT INTO dbo.ids DEFAULT VALUES")
            .unwrap();
        assert_eq!(
            db.query_row("SELECT min(id) FROM dbo.ids", [], |r| r.get::<_, i32>(0))
                .unwrap(),
            7
        );
        db.execute_batch(
            "DROP TABLE dbo.dependency; DELETE FROM main.__msduck_identity_definitions; BEGIN",
        )
        .unwrap();
        assert!(execute(&db, &truncate, false).is_err());
        assert_eq!(
            db.query_row("SELECT count(*) FROM dbo.ids", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
        db.execute_batch("ROLLBACK").unwrap();
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
}
