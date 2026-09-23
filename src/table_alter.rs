//! Plan column changes as an atomic group of DuckDB DDL statements.
use anyhow::{Result, ensure};
pub use msduck_sql::ddl_syntax::alter_table as validate;
use sqlparser::ast::*;

#[cfg(test)]
pub fn execute(db: &duckdb::Connection, table: &AlterTable, autocommit: bool) -> Result<()> {
    execute_declared(db, table, table, autocommit)
}

pub fn execute_declared(
    db: &duckdb::Connection,
    table: &AlterTable,
    original: &AlterTable,
    autocommit: bool,
) -> Result<()> {
    validate(table)?;
    let mut commands = Vec::new();
    let mut guarded_adds = std::collections::HashMap::new();
    let mut definitions = Vec::new();
    let mut populations = Vec::new();
    for (operation, original) in table.operations.iter().zip(&original.operations) {
        match operation {
            AlterTableOperation::AddColumn { column_def, .. } => {
                let mut column = column_def.clone();
                let mut restored_default = None;
                let mut constant_unicode_default = false;
                if let AlterTableOperation::AddColumn {
                    column_def: source, ..
                } = original
                    && matches!(source.data_type, DataType::Time(..))
                {
                    for option in &mut column.options {
                        if let ColumnOption::Default(value) = &mut option.option {
                            restored_default = Some(value.clone());
                            *value =
                                crate::assignment::convert(value.clone(), &source.data_type, false);
                        }
                    }
                }
                if let AlterTableOperation::AddColumn {
                    column_def: source, ..
                } = original
                    && crate::character_storage::is_character(&source.data_type)
                    && source.options.iter().any(|option| {
                        matches!(option.option, ColumnOption::NotNull)
                            || crate::dialect::is_with_values(&option.option)
                    })
                {
                    let original_default = source.options.iter().find_map(|o| {
                        if let ColumnOption::Default(e) = &o.option {
                            Some(e)
                        } else {
                            None
                        }
                    });
                    if let Some(original_default) = original_default
                        && let Some(constant) = crate::character_storage::constant_default(
                            original_default,
                            &source.data_type,
                        )
                        .map_err(anyhow::Error::msg)?
                    {
                        constant_unicode_default =
                            crate::character_storage::unicode_storage_type(&source.data_type)
                                .is_some();
                        for option in &mut column.options {
                            if let ColumnOption::Default(value) = &mut option.option {
                                restored_default = Some(value.clone());
                                *value = constant.clone();
                            }
                        }
                    }
                }
                let identity = crate::identity::prepare(db, &mut column)?;
                let mut identity_default = None;
                if let Some(definition) = identity {
                    for option in &mut column.options {
                        if let ColumnOption::Default(expr) = &mut option.option {
                            identity_default = Some(expr.clone());
                            *expr = Expr::Value(
                                Value::Number(definition.seed().to_string(), false).into(),
                            );
                        }
                    }
                    definitions.push(definition);
                }
                let not_null = column
                    .options
                    .iter()
                    .any(|o| matches!(o.option, ColumnOption::NotNull));
                let with_values = column
                    .options
                    .iter()
                    .any(|o| crate::dialect::is_with_values(&o.option));
                let mut deferred_default = None;
                column.options.retain(|o| match &o.option {
                    option if crate::dialect::is_with_values(option) => false,
                    ColumnOption::NotNull => false,
                    ColumnOption::Default(expr) if !not_null && !with_values => {
                        deferred_default = Some(expr.clone());
                        false
                    }
                    _ => true,
                });
                // Nullable ADD DEFAULT leaves old rows NULL in SQL Server.
                // Install its default only after the column has been added.
                if constant_unicode_default {
                    // DuckDB rewrites expression defaults into ADD + UPDATE.
                    // IF NOT EXISTS selects its direct ADD path, safe for this
                    // generated constant. Preserve ordinary ADD semantics with
                    // an absence check inside the same catalog transaction.
                    guarded_adds.insert(commands.len(), column.name.value.clone());
                    commands.push(format!(
                        "ALTER TABLE {} ADD COLUMN IF NOT EXISTS {column}",
                        table.name
                    ));
                } else {
                    commands.push(format!("ALTER TABLE {} ADD COLUMN {column}", table.name));
                }
                if let Some(default) = deferred_default {
                    commands.push(format!(
                        "ALTER TABLE {} ALTER COLUMN {} SET DEFAULT {default}",
                        table.name, column.name
                    ));
                }
                if let Some(default) = restored_default {
                    commands.push(format!(
                        "ALTER TABLE {} ALTER COLUMN {} SET DEFAULT {default}",
                        table.name, column.name
                    ));
                }
                if not_null {
                    commands.push(format!(
                        "ALTER TABLE {} ALTER COLUMN {} SET NOT NULL",
                        table.name, column.name
                    ));
                }
                if let Some(default) = identity_default {
                    commands.push(format!(
                        "ALTER TABLE {} ALTER COLUMN {} SET DEFAULT {default}",
                        table.name, column.name
                    ));
                    // Populate only after all schema changes. DuckDB cannot enforce
                    // NOT NULL over uncommitted updates or alter their storage before commit.
                    populations.push(format!(
                        "UPDATE {} SET {} = {default}",
                        table.name, column.name
                    ));
                }
            }
            AlterTableOperation::AlterColumn { column_name, op } => {
                let mut operation = operation.clone();
                if let AlterColumnOperation::SetDataType { data_type, .. } = op
                    && (crate::engine::integral_type(data_type)
                        || crate::datetime2_cast::storage_scale(data_type).is_some()
                        || crate::datetimeoffset_cast::storage_scale(data_type).is_some()
                        || crate::variant_pack::is_storage(data_type))
                    && let AlterTableOperation::AlterColumn {
                        op: AlterColumnOperation::SetDataType { using, .. },
                        ..
                    } = &mut operation
                {
                    // Apply target semantics before backend storage: integer
                    // truncation or DATETIME2 scale rounding. Retain quoted
                    // column names through the AST.
                    *using = Some(crate::assignment::convert(
                        Expr::Identifier(column_name.clone()),
                        data_type,
                        false,
                    ));
                }
                if let AlterTableOperation::AlterColumn {
                    op:
                        AlterColumnOperation::SetDataType {
                            data_type: kind, ..
                        },
                    ..
                } = original
                    && (matches!(kind, DataType::Time(..))
                        || crate::character_storage::is_character(kind)
                        || msduck_sql::money_cast::money_type(kind).is_some())
                    && let AlterTableOperation::AlterColumn {
                        op: AlterColumnOperation::SetDataType { using, .. },
                        ..
                    } = &mut operation
                {
                    *using = Some(crate::assignment::convert_for_storage(
                        Expr::Identifier(column_name.clone()),
                        kind,
                        false,
                        crate::character_storage::unicode_storage_type(kind).is_some(),
                    ));
                }
                commands.push(format!("ALTER TABLE {} {operation}", table.name));
            }
            AlterTableOperation::DropColumn {
                column_names,
                if_exists,
                ..
            } => {
                for name in column_names {
                    commands.push(format!(
                        "ALTER TABLE {} DROP COLUMN {}{name}",
                        table.name,
                        if *if_exists { "IF EXISTS " } else { "" }
                    ));
                }
            }
            _ => unreachable!("validated operations"),
        }
    }
    if autocommit {
        db.execute_batch("BEGIN TRANSACTION")?;
    }
    let result = (|| -> Result<()> {
        let identities = crate::identity::columns(db, &table.name)?;
        ensure!(
            identities.len() + definitions.len() <= 1,
            "A table can contain only one identity column."
        );
        for operation in &table.operations {
            if let AlterTableOperation::AlterColumn { column_name, .. } = operation {
                ensure!(
                    !identities
                        .iter()
                        .any(|(name, _)| name.eq_ignore_ascii_case(&column_name.value)),
                    "ALTER COLUMN of an identity column is not yet supported"
                );
            }
        }
        for definition in definitions {
            definition.install(db)?;
        }
        for (index, command) in commands.into_iter().enumerate() {
            if let Some(column) = guarded_adds.get(&index) {
                let mut existing = db.prepare(&format!("SELECT * FROM {} LIMIT 0", table.name))?;
                existing.query([])?.next()?;
                ensure!(
                    !existing
                        .column_names()
                        .iter()
                        .any(|name| name.eq_ignore_ascii_case(column)),
                    "Column with name {column} already exists!"
                );
            }
            db.execute(&command, [])?;
        }
        for population in populations {
            db.execute(&population, [])?;
        }
        let remaining = crate::identity::columns(db, &table.name)?;
        for (_, sequence) in identities {
            if !remaining.iter().any(|(_, name)| *name == sequence) {
                crate::identity::drop_sequence(db, &sequence)?;
            }
        }
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
    #[test]
    fn mixed_actions_are_rejected_before_any_command() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE actions(a INTEGER,b INTEGER); INSERT INTO actions VALUES(1,2)",
        )
        .unwrap();
        let parse = |sql| {
            let Statement::AlterTable(table) =
                sqlparser::parser::Parser::parse_sql(&crate::dialect::ServerDialect, sql)
                    .unwrap()
                    .remove(0)
            else {
                panic!()
            };
            table
        };
        let mut mixed = parse("ALTER TABLE actions DROP COLUMN b");
        mixed
            .operations
            .extend(parse("ALTER TABLE actions ADD b BIGINT").operations);
        assert!(
            execute(&db, &mixed, true)
                .unwrap_err()
                .to_string()
                .contains("cannot mix")
        );
        assert_eq!(
            db.query_row("SELECT a,b FROM actions", [], |r| Ok((
                r.get::<_, i32>(0)?,
                r.get::<_, i32>(1)?
            )))
            .unwrap(),
            (1, 2)
        );
        let mut mixed = parse("ALTER TABLE actions ALTER COLUMN a BIGINT");
        mixed
            .operations
            .extend(parse("ALTER TABLE actions ALTER COLUMN b BIGINT").operations);
        assert!(validate(&mixed).is_err());
        assert!(validate(&parse("ALTER TABLE actions ALTER COLUMN a BIGINT NOT NULL")).is_ok());
    }
    use super::*;
    #[test]
    fn added_identity_populates_chunks_and_cleans_failed_ddl() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::identity_metadata::register(&db).unwrap();
        db.execute_batch("CREATE SCHEMA dbo; CREATE TABLE dbo.ids(v INT); INSERT INTO dbo.ids SELECT * FROM range(6000)").unwrap();
        let alter = |sql: &str| {
            let Statement::AlterTable(table) =
                sqlparser::parser::Parser::parse_sql(&crate::dialect::ServerDialect, sql)
                    .unwrap()
                    .remove(0)
            else {
                panic!()
            };
            table
        };
        assert!(
            execute(
                &db,
                &alter("ALTER TABLE dbo.ids ADD id SMALLINT IDENTITY(32767,1)"),
                true
            )
            .is_err()
        );
        assert!(
            execute(
                &db,
                &alter("ALTER TABLE dbo.ids ADD id INT IDENTITY,required INT NOT NULL"),
                true
            )
            .is_err()
        );
        for source in ["duckdb_sequences()", "main.__msduck_identity_definitions"] {
            assert_eq!(
                db.query_row(&format!("SELECT count(*) FROM {source}"), [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
        execute(
            &db,
            &alter("ALTER TABLE dbo.ids ADD id INT IDENTITY(10,5)"),
            true,
        )
        .unwrap();
        assert_eq!(
            db.query_row(
                "SELECT count(DISTINCT id),min(id),max(id) FROM dbo.ids",
                [],
                |r| Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i32>(1)?,
                    r.get::<_, i32>(2)?
                ))
            )
            .unwrap(),
            (6000, 10, 30005)
        );
        db.execute_batch("INSERT INTO dbo.ids(v) VALUES(6000)")
            .unwrap();
        assert_eq!(
            db.query_row("SELECT id FROM dbo.ids WHERE v=6000", [], |r| r
                .get::<_, i32>(0))
                .unwrap(),
            30010
        );
    }
    #[test]
    fn dropping_identity_column_cleans_and_restores_private_objects() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::identity_metadata::register(&db).unwrap();
        db.execute_batch("CREATE SCHEMA dbo").unwrap();
        let parse = |sql: &str| {
            sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::MsSqlDialect {}, sql)
                .unwrap()
                .remove(0)
        };
        crate::identity::create(
            &db,
            &parse("CREATE TABLE dbo.ids(id INT IDENTITY(10,5),v INT)"),
            true,
        )
        .unwrap();
        db.execute_batch("INSERT INTO dbo.ids(v) VALUES(1)")
            .unwrap();
        let Statement::AlterTable(alter) = parse("ALTER TABLE dbo.ids DROP COLUMN id") else {
            panic!()
        };
        let count = |sql| db.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
        db.execute_batch("BEGIN").unwrap();
        execute(&db, &alter, false).unwrap();
        assert_eq!(count("SELECT count(*) FROM duckdb_sequences()"), 0);
        assert_eq!(
            count("SELECT count(*) FROM main.__msduck_identity_definitions"),
            0
        );
        db.execute_batch("ROLLBACK; INSERT INTO dbo.ids(v) VALUES(2)")
            .unwrap();
        assert_eq!(count("SELECT max(id) FROM dbo.ids"), 15);
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
        assert!(execute(&db, &alter, true).is_err());
        assert_eq!(count("SELECT max(id) FROM dbo.ids"), 15);
        assert_eq!(
            count("SELECT count(*) FROM main.__msduck_identity_definitions"),
            1
        );
        db.execute_batch("DROP TABLE dbo.dependency").unwrap();
        execute(&db, &alter, true).unwrap();
        assert_eq!(count("SELECT count(*) FROM duckdb_sequences()"), 0);
        assert_eq!(
            count("SELECT count(*) FROM main.__msduck_identity_definitions"),
            0
        );
        db.execute_batch("INSERT INTO dbo.ids(v) VALUES(3)")
            .unwrap();
        assert_eq!(count("SELECT count(*) FROM dbo.ids"), 3);
    }
}
