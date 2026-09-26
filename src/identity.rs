//! Integer IDENTITY allocation backed by persistent, non-transactional sequences.
use anyhow::{Result, bail, ensure};
use duckdb::Connection;
use sqlparser::{ast::*, dialect::GenericDialect, parser::Parser};
use std::ops::ControlFlow;

pub const EXPLICIT: &str =
    "Cannot insert explicit value for identity column when IDENTITY_INSERT is set to OFF.";
pub const UPDATE: &str = "Cannot update identity column.";
const PREFIX: &str = "main.__msduck_identity_";
pub fn catalog(db: &Connection) -> duckdb::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS main.__msduck_identity_definitions(sequence_name VARCHAR PRIMARY KEY, seed BIGINT NOT NULL, increment_value BIGINT NOT NULL)")
}

pub(crate) fn sequence_name(value: Option<&str>) -> Option<String> {
    let value = value?;
    let mut parser = Parser::new(&GenericDialect {}).try_with_sql(value).ok()?;
    let Expr::Function(f) = parser.parse_expr().ok()? else {
        return None;
    };
    if !f.name.to_string().eq_ignore_ascii_case("nextval") {
        return None;
    }
    let FunctionArguments::List(args) = f.args else {
        return None;
    };
    let [FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(v)))] = args.args.as_slice() else {
        return None;
    };
    let Value::SingleQuotedString(name) = &v.value else {
        return None;
    };
    let suffix = name.strip_prefix(PREFIX)?;
    (suffix.len() == 32 && suffix.bytes().all(|b| b.is_ascii_hexdigit())).then(|| name.clone())
}

pub fn is_default(value: Option<&str>) -> bool {
    sequence_name(value).is_some()
}

fn references_sequence(default: &str, name: &str) -> bool {
    let parsed = Parser::new(&GenericDialect {})
        .try_with_sql(default)
        .and_then(|mut parser| parser.parse_expr());
    let Ok(expr) = parsed else {
        // Unknown native default syntax must not strand a live private sequence.
        return default
            .to_ascii_lowercase()
            .contains(&name.to_ascii_lowercase());
    };
    visit_expressions(&expr, |expr| {
        let Expr::Function(function) = expr else {
            return ControlFlow::Continue(());
        };
        if !function.name.to_string().eq_ignore_ascii_case("nextval") {
            return ControlFlow::Continue(());
        }
        let FunctionArguments::List(arguments) = &function.args else {
            return ControlFlow::Continue(());
        };
        for argument in &arguments.args {
            let FunctionArg::Unnamed(FunctionArgExpr::Expr(value)) = argument else {
                continue;
            };
            if visit_expressions(value, |part| match part {
                Expr::Value(value)
                    if matches!(&value.value, Value::SingleQuotedString(text) if text.eq_ignore_ascii_case(name)) =>
                {
                    ControlFlow::Break(())
                }
                _ => ControlFlow::Continue(()),
            })
            .is_break()
            {
                return ControlFlow::Break(());
            }
        }
        ControlFlow::Continue(())
    })
    .is_break()
}

pub(crate) fn columns(db: &Connection, name: &ObjectName) -> Result<Vec<(String, String)>> {
    let parts = name
        .0
        .iter()
        .map(|p| p.as_ident().map(|p| p.value.as_str()))
        .collect::<Option<Vec<_>>>();
    let Some(parts) = parts else {
        return Ok(vec![]);
    };
    let (schema, table) = match parts.as_slice() {
        [table] => ("dbo", *table),
        [schema, table] => (*schema, *table),
        _ => return Ok(vec![]),
    };
    let mut stmt = db.prepare("SELECT column_name,column_default FROM information_schema.columns WHERE table_catalog=current_database() AND table_schema=? COLLATE NOCASE AND table_name=? COLLATE NOCASE")?;
    let mut sequences = vec![];
    for row in stmt.query_map([schema, table], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
    })? {
        let (column, default) = row?;
        if let Some(name) = sequence_name(default.as_deref()) {
            sequences.push((column, name));
        }
    }
    Ok(sequences)
}

fn table_sequences(db: &Connection, name: &ObjectName) -> Result<Vec<String>> {
    Ok(columns(db, name)?
        .into_iter()
        .map(|(_, sequence)| sequence)
        .collect())
}

pub(crate) fn drop_sequence(db: &Connection, name: &str) -> Result<()> {
    // Names are recognized private sequence defaults. Never cascade dependencies.
    // DuckDB's dependency check can lose a reference after an ALTER TABLE in the
    // same transaction. Inspect live defaults before removing the sequence so a
    // failed ALTER or DROP can roll back without stranding another table.
    let mut defaults = db.prepare(
        "SELECT table_schema,table_name,column_name,column_default FROM information_schema.columns WHERE table_catalog=current_database() AND column_default IS NOT NULL",
    )?;
    for row in defaults.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })? {
        let (schema, table, column, default) = row?;
        ensure!(
            !references_sequence(&default, name),
            "Cannot drop identity sequence {name}: still referenced by {schema}.{table}.{column}"
        );
    }
    db.execute_batch(&format!("DROP SEQUENCE {name}"))?;
    catalog(db)?;
    db.execute(
        "DELETE FROM main.__msduck_identity_definitions WHERE sequence_name=?",
        [name],
    )?;
    Ok(())
}

/// Private allocation objects share the table's DDL transaction.
pub fn drop_table(db: &Connection, statement: &Statement, autocommit: bool) -> Result<bool> {
    let Statement::Drop {
        object_type: ObjectType::Table,
        names,
        ..
    } = statement
    else {
        return Ok(false);
    };
    if autocommit {
        db.execute_batch("BEGIN TRANSACTION")?;
    }
    let result = (|| -> Result<()> {
        let mut sequences = std::collections::BTreeSet::new();
        for name in names {
            sequences.extend(table_sequences(db, name)?);
        }
        db.execute_batch(&statement.to_string())?;
        for name in sequences {
            drop_sequence(db, &name)?;
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

pub struct Reset {
    column: Ident,
    old: String,
    seed: i64,
    increment: i64,
    min: i64,
    max: i64,
}

/// Resolve original definitions before truncation can remove any rows.
pub fn plan_reset(db: &Connection, schema: &str, table: &str) -> Result<Vec<Reset>> {
    let mut stmt = db.prepare("SELECT column_name,column_default FROM information_schema.columns WHERE table_catalog=current_database() AND table_schema=? COLLATE NOCASE AND table_name=? COLLATE NOCASE")?;
    let columns = stmt
        .query_map([schema, table], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?
        .collect::<duckdb::Result<Vec<_>>>()?;
    drop(stmt);
    let mut resets = vec![];
    for (column, default) in columns {
        let Some(old) = sequence_name(default.as_deref()) else {
            continue;
        };
        let (seed, increment, min, max): (i64, i64, i64, i64) = db.query_row(
            "SELECT d.seed,d.increment_value,s.min_value,s.max_value FROM main.__msduck_identity_definitions d JOIN duckdb_sequences() s ON s.schema_name||'.'||s.sequence_name=d.sequence_name AND s.database_name=current_database() WHERE d.sequence_name=?",
            [&old], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
        )?;
        resets.push(Reset {
            column: Ident::with_quote('"', column),
            old,
            seed,
            increment,
            min,
            max,
        });
    }
    Ok(resets)
}

/// A fresh sequence lets rollback restore the old nontransactional counter.
pub fn reset(db: &Connection, target: &ObjectName, resets: Vec<Reset>) -> Result<()> {
    for Reset {
        column,
        old,
        seed,
        increment,
        min,
        max,
    } in resets
    {
        let uuid: String = db.query_row("SELECT CAST(uuid() AS VARCHAR)", [], |r| r.get(0))?;
        let name = format!("{PREFIX}{}", uuid.replace('-', ""));
        db.execute_batch(&format!("CREATE SEQUENCE {name} INCREMENT BY {increment} MINVALUE {min} MAXVALUE {max} START WITH {seed} NO CYCLE"))?;
        db.execute(
            "INSERT INTO main.__msduck_identity_definitions VALUES(?,?,?)",
            duckdb::params![name, seed, increment],
        )?;
        db.execute_batch(&format!("ALTER TABLE {target} ALTER COLUMN {column} SET DEFAULT nextval('{name}'); DROP SEQUENCE {old}"))?;
        db.execute(
            "DELETE FROM main.__msduck_identity_definitions WHERE sequence_name=?",
            [&old],
        )?;
    }
    Ok(())
}

pub(crate) struct Definition {
    name: String,
    seed: i64,
    increment: i64,
    sql: String,
}

impl Definition {
    pub(crate) fn seed(&self) -> i64 {
        self.seed
    }
    pub(crate) fn install(&self, db: &Connection) -> Result<()> {
        catalog(db)?;
        db.execute_batch(&self.sql)?;
        db.execute(
            "INSERT INTO main.__msduck_identity_definitions VALUES(?,?,?)",
            duckdb::params![self.name, self.seed, self.increment],
        )?;
        Ok(())
    }
}

/// Validate the identity and replace it with an allocator default and NOT NULL.
pub(crate) fn prepare(db: &Connection, column: &mut ColumnDef) -> Result<Option<Definition>> {
    let mut definition = None;
    for option in column.options.clone() {
        let ColumnOption::Identity(property) = option.option else {
            continue;
        };
        ensure!(
            definition.is_none(),
            "A table can contain only one identity column."
        );
        let IdentityPropertyKind::Identity(property) = property else {
            bail!("unsupported AUTOINCREMENT property")
        };
        ensure!(
            property.order.is_none(),
            "unsupported identity ordering option"
        );
        let (seed, increment) = match property.parameters {
            None => (1, 1),
            Some(IdentityPropertyFormatKind::FunctionCall(p)) => (
                p.seed.to_string().parse::<i64>()?,
                p.increment.to_string().parse::<i64>()?,
            ),
            _ => bail!("unsupported identity parameters"),
        };
        ensure!(
            increment != 0,
            "zero identity increments are not yet supported"
        );
        let (min, max) = match column.data_type {
            DataType::UTinyInt => (0, 255),
            DataType::SmallInt(_) => (i16::MIN as i64, i16::MAX as i64),
            DataType::Int(_) | DataType::Integer(_) => (i32::MIN as i64, i32::MAX as i64),
            DataType::BigInt(_) => (i64::MIN, i64::MAX),
            _ => bail!("unsupported identity data type: {}", column.data_type),
        };
        ensure!(
            (min..=max).contains(&seed),
            "Arithmetic overflow error converting IDENTITY seed."
        );
        ensure!(
            !column
                .options
                .iter()
                .any(|o| matches!(o.option, ColumnOption::Null | ColumnOption::Default(_))),
            "IDENTITY cannot have NULL or DEFAULT options"
        );
        let uuid: String = db.query_row("SELECT CAST(uuid() AS VARCHAR)", [], |r| r.get(0))?;
        let name = format!("{PREFIX}{}", uuid.replace('-', ""));
        let default = Parser::new(&GenericDialect {})
            .try_with_sql(&format!("nextval('{name}')"))?
            .parse_expr()?;
        column
            .options
            .retain(|o| !matches!(o.option, ColumnOption::Identity(_)));
        column.options.push(ColumnOptionDef {
            name: None,
            option: ColumnOption::Default(default),
        });
        if !column
            .options
            .iter()
            .any(|o| matches!(o.option, ColumnOption::NotNull))
        {
            column.options.push(ColumnOptionDef {
                name: None,
                option: ColumnOption::NotNull,
            });
        }
        definition = Some(Definition {
            sql: format!(
                "CREATE SEQUENCE {name} INCREMENT BY {increment} MINVALUE {min} MAXVALUE {max} START WITH {seed} NO CYCLE"
            ),
            name,
            seed,
            increment,
        });
    }
    Ok(definition)
}

pub(crate) fn restore_options(source: &ColumnDef, target: &mut ColumnDef) {
    let mut identities = source
        .options
        .iter()
        .filter(|o| matches!(o.option, ColumnOption::Identity(_)));
    for option in &mut target.options {
        if matches!(option.option, ColumnOption::Identity(_))
            && let Some(original) = identities.next()
        {
            *option = original.clone();
        }
    }
}

pub fn create(db: &Connection, statement: &Statement, autocommit: bool) -> Result<bool> {
    let Statement::CreateTable(original) = statement else {
        return Ok(false);
    };
    let mut table = original.clone();
    let mut definition = None;
    for column in &mut table.columns {
        if let Some(next) = prepare(db, column)? {
            ensure!(
                definition.is_none(),
                "A table can contain only one identity column."
            );
            definition = Some(next);
        }
    }
    let Some(definition) = definition else {
        return Ok(false);
    };
    if autocommit {
        db.execute_batch("BEGIN TRANSACTION")?;
    }
    let result = (|| -> Result<()> {
        definition.install(db)?;
        db.execute_batch(&Statement::CreateTable(table).to_string())?;
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nested_defaults_find_private_sequence_calls() {
        let name = format!("{PREFIX}{}", "0".repeat(32));
        assert!(references_sequence(
            &format!("nextval('{name}') + 1"),
            &name
        ));
        assert!(references_sequence(
            &format!("coalesce(nextval(CAST('{name}' AS VARCHAR)), 0)"),
            &name
        ));
        assert!(!references_sequence(&format!("'{name}'"), &name));
        assert!(!references_sequence("nextval('main.other')", &name));
    }
    #[test]
    fn nested_default_blocks_identity_column_drop_until_dependency_is_removed() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE SCHEMA dbo").unwrap();
        let parse = |sql| {
            Parser::parse_sql(&sqlparser::dialect::MsSqlDialect {}, sql)
                .unwrap()
                .remove(0)
        };
        create(
            &db,
            &parse("CREATE TABLE dbo.ids(id INT IDENTITY,v INT)"),
            true,
        )
        .unwrap();
        let name = table_sequences(
            &db,
            &ObjectName::from(vec![Ident::new("dbo"), Ident::new("ids")]),
        )
        .unwrap()
        .remove(0);
        db.execute_batch(&format!(
            "CREATE TABLE dbo.dependency(id INT DEFAULT nextval('{name}') + 1)"
        ))
        .unwrap();
        let Statement::AlterTable(alter) = parse("ALTER TABLE dbo.ids DROP COLUMN id") else {
            panic!()
        };
        assert!(
            crate::table_alter::execute(&db, &alter, true)
                .unwrap_err()
                .to_string()
                .contains("still referenced by dbo.dependency.id")
        );
        db.execute_batch(
            "INSERT INTO dbo.ids(v) VALUES(1); INSERT INTO dbo.dependency DEFAULT VALUES",
        )
        .unwrap();
        db.execute_batch("DROP TABLE dbo.dependency").unwrap();
        crate::table_alter::execute(&db, &alter, true).unwrap();
        let sequence_count: i64 = db
            .query_row("SELECT count(*) FROM duckdb_sequences()", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(sequence_count, 0);
    }
    #[test]
    fn table_drop_cleans_sequences_and_rollback_restores_both() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE SCHEMA dbo").unwrap();
        let parse = |sql| {
            Parser::parse_sql(&sqlparser::dialect::MsSqlDialect {}, sql)
                .unwrap()
                .remove(0)
        };
        let ddl = parse("CREATE TABLE dbo.ids(id INT IDENTITY(10,5))");
        create(&db, &ddl, true).unwrap();
        db.execute_batch("INSERT INTO dbo.ids DEFAULT VALUES; BEGIN")
            .unwrap();
        let drop = parse("DROP TABLE dbo.ids");
        drop_table(&db, &drop, false).unwrap();
        let count: i64 = db
            .query_row("SELECT count(*) FROM duckdb_sequences()", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
        db.execute_batch("ROLLBACK; INSERT INTO dbo.ids DEFAULT VALUES")
            .unwrap();
        let max: i32 = db
            .query_row("SELECT max(id) FROM dbo.ids", [], |r| r.get(0))
            .unwrap();
        assert_eq!(max, 15);
        let name = table_sequences(
            &db,
            &ObjectName::from(vec![Ident::new("dbo"), Ident::new("ids")]),
        )
        .unwrap()
        .remove(0);
        db.execute_batch(&format!(
            "CREATE TABLE dbo.dependency(id INT DEFAULT nextval('{name}') + 1)"
        ))
        .unwrap();
        assert!(
            drop_table(&db, &drop, true)
                .unwrap_err()
                .to_string()
                .contains("still referenced by dbo.dependency.id")
        );
        let count: i64 = db
            .query_row("SELECT count(*) FROM dbo.ids", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2);
        db.execute_batch("DROP TABLE dbo.dependency").unwrap();
        drop_table(&db, &drop, true).unwrap();
        let count: i64 = db
            .query_row("SELECT count(*) FROM duckdb_sequences()", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
        drop_table(&db, &parse("DROP TABLE IF EXISTS dbo.ids"), true).unwrap();
        create(&db, &ddl, true).unwrap();
        db.execute_batch("INSERT INTO dbo.ids DEFAULT VALUES")
            .unwrap();
        let id: i32 = db
            .query_row("SELECT id FROM dbo.ids", [], |r| r.get(0))
            .unwrap();
        assert_eq!(id, 10);
        assert!(!is_default(Some(
            "nextval('main.__msduck_identity_not_generated')"
        )));
    }
    #[test]
    fn concurrent_connections_allocate_distinct_values() {
        let db = Connection::open_in_memory().unwrap();
        let ddl = Parser::parse_sql(
            &sqlparser::dialect::MsSqlDialect {},
            "CREATE TABLE ids(id INT IDENTITY,v INT)",
        )
        .unwrap()
        .remove(0);
        assert!(create(&db, &ddl, true).unwrap());
        let threads = (0..4)
            .map(|_| {
                let db = db.try_clone().unwrap();
                std::thread::spawn(move || {
                    db.execute_batch("INSERT INTO ids(v) SELECT 1 FROM range(600)")
                        .unwrap()
                })
            })
            .collect::<Vec<_>>();
        for thread in threads {
            thread.join().unwrap();
        }
        let counts: (i64, i64, i32) = db
            .query_row(
                "SELECT count(*),count(DISTINCT id),max(id) FROM ids",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(counts, (2400, 2400, 2400));
        crate::identity_metadata::register(&db).unwrap();
        let other = db.try_clone().unwrap();
        let current: String = other
            .query_row(
                "SELECT CAST(__msduck_ident_current('main.ids') AS VARCHAR)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(current, "2400");
    }
    #[test]
    fn shared_sequences_survive_rollback_and_restart() {
        let path = std::env::temp_dir().join(format!(
            "msduck-identity-{}-{}.duckdb",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        {
            let db = Connection::open(&path).unwrap();
            let ddl = Parser::parse_sql(
                &sqlparser::dialect::MsSqlDialect {},
                "CREATE TABLE ids(id INT IDENTITY(10,5),v INT)",
            )
            .unwrap()
            .remove(0);
            assert!(create(&db, &ddl, true).unwrap());
            db.execute_batch("BEGIN; INSERT INTO ids(v) VALUES(1); ROLLBACK")
                .unwrap();
            let other = db.try_clone().unwrap();
            other.execute_batch("INSERT INTO ids(v) VALUES(2)").unwrap();
            let value: i32 = db
                .query_row("SELECT id FROM ids", [], |r| r.get(0))
                .unwrap();
            assert_eq!(value, 15);
        }
        let db = Connection::open(&path).unwrap();
        let state: (i64, Option<i64>) = db
            .query_row(
                "SELECT start_value,last_value FROM duckdb_sequences()",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, (20, Some(15)));
        let sequence: String = db
            .query_row(
                "SELECT schema_name||'.'||sequence_name FROM duckdb_sequences()",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let current: Result<i64, _> = db.query_row("SELECT currval(?)", [&sequence], |r| r.get(0));
        assert_eq!(current.unwrap(), 15);
        crate::identity_metadata::register(&db).unwrap();
        let current: String = db
            .query_row(
                "SELECT CAST(__msduck_ident_current('main.ids') AS VARCHAR)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(current, "15");
        db.execute_batch("INSERT INTO ids(v) VALUES(3)").unwrap();
        let value: i32 = db
            .query_row("SELECT max(id) FROM ids", [], |r| r.get(0))
            .unwrap();
        assert_eq!(value, 20);
        let default: String = db.query_row("SELECT column_default FROM information_schema.columns WHERE table_name='ids' AND column_name='id'", [], |r|r.get(0)).unwrap();
        assert!(is_default(Some(&default)), "{default}");
        crate::identity_metadata::register(&db).unwrap();
        let seed: String = db
            .query_row(
                "SELECT CAST(__msduck_ident_seed('main.ids') AS VARCHAR)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(seed, "10");
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
}
