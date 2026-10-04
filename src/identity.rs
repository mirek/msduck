//! Integer IDENTITY allocation backed by persistent, non-transactional sequences.
use anyhow::{Result, bail, ensure};
use duckdb::Connection;
use sqlparser::{ast::*, dialect::GenericDialect, parser::Parser};

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

fn table_key(name: &ObjectName) -> Option<(String, String)> {
    let parts = name
        .0
        .iter()
        .map(|part| part.as_ident().map(|id| id.value.as_str()))
        .collect::<Option<Vec<_>>>()?;
    match parts.as_slice() {
        [table] => Some(("dbo".into(), (*table).into())),
        [schema, table] => Some(((*schema).into(), (*table).into())),
        _ => None,
    }
}

// Defaults are catalog SQL, never commands to execute. Only statically known
// sequence arguments establish dependencies; uncertainty must stay explicit.
fn sequence_argument(expr: &Expr) -> Result<Option<String>> {
    match expr {
        Expr::Value(value) => match &value.value {
            Value::SingleQuotedString(text) => {
                ensure!(
                    text.len() <= 1024,
                    "Private sequence reference exceeds name analysis limit"
                );
                Ok(Some(text.clone()))
            }
            Value::Null => Ok(None),
            _ => bail!("Unsupported private sequence default argument"),
        },
        Expr::Nested(expr) => sequence_argument(expr),
        Expr::Cast {
            expr,
            data_type:
                DataType::Char(_)
                | DataType::Varchar(_)
                | DataType::Character(_)
                | DataType::CharacterVarying(_)
                | DataType::Text
                | DataType::String(_),
            ..
        } => sequence_argument(expr),
        Expr::BinaryOp {
            left,
            op: BinaryOperator::StringConcat,
            right,
        } => match (sequence_argument(left)?, sequence_argument(right)?) {
            (Some(mut left), Some(right)) => {
                ensure!(
                    left.len()
                        .checked_add(right.len())
                        .is_some_and(|len| len <= 1024),
                    "Private sequence reference exceeds name analysis limit"
                );
                left.push_str(&right);
                Ok(Some(left))
            }
            _ => Ok(None),
        },
        _ => bail!("Unsupported dynamic private sequence default argument"),
    }
}

fn sequence_key(name: &str, schema: &str) -> Result<(String, String)> {
    let mut parser = Parser::new(&GenericDialect {}).try_with_sql(name)?;
    let name = parser.parse_object_name(false)?;
    ensure!(
        parser.peek_token().token == sqlparser::tokenizer::Token::EOF,
        "Unsupported trailing private sequence reference syntax"
    );
    let parts = name
        .0
        .iter()
        .map(|part| part.as_ident().map(|id| id.value.as_str()))
        .collect::<Option<Vec<_>>>();
    match parts.as_deref() {
        Some([name]) => Ok((schema.into(), (*name).into())),
        Some([schema, name]) => Ok(((*schema).into(), (*name).into())),
        _ => bail!("Unsupported qualified private sequence reference"),
    }
}

fn references_sequence(default: &str, name: &str, bindings: &[(String, String)]) -> Result<bool> {
    let mut parser = Parser::new(&GenericDialect {}).try_with_sql(default)?;
    let expr = parser.parse_expr()?;
    ensure!(
        parser.peek_token().token == sqlparser::tokenizer::Token::EOF,
        "Unsupported native default dependency syntax"
    );
    let expected = sequence_key(name, "")?;
    let mut found = false;
    let mut failure = None;
    let _ = visit_expressions(&expr, |expr| {
        let Expr::Function(function) = expr else {
            return std::ops::ControlFlow::Continue(());
        };
        if !function
            .name
            .0
            .last()
            .and_then(ObjectNamePart::as_ident)
            .is_some_and(|id| {
                id.value.eq_ignore_ascii_case("nextval") || id.value.eq_ignore_ascii_case("currval")
            })
        {
            return std::ops::ControlFlow::Continue(());
        }
        let result = (|| -> Result<bool> {
            let FunctionArguments::List(arguments) = &function.args else {
                bail!("Unsupported private sequence default arguments");
            };
            let [FunctionArg::Unnamed(FunctionArgExpr::Expr(value))] = arguments.args.as_slice()
            else {
                bail!("Unsupported private sequence default arguments");
            };
            let Some(name) = sequence_argument(value)? else {
                return Ok(false);
            };
            let mut actual = sequence_key(&name, "")?;
            if actual.0.is_empty() {
                // DuckDB binds defaults when created, and keeps their SQL text
                // unqualified even after the connection's search path changes.
                // Its dependency catalog identifies bound sequences per table,
                // not per column. Ambiguous same-name bindings remain explicit.
                let mut matches = bindings
                    .iter()
                    .filter(|(_, name)| name.eq_ignore_ascii_case(&actual.1));
                let binding = matches.next().ok_or_else(|| {
                    anyhow::anyhow!("Unknown bound private sequence default reference")
                })?;
                ensure!(
                    matches.next().is_none(),
                    "Ambiguous bound private sequence default reference"
                );
                actual.0.clone_from(&binding.0);
            }
            Ok(actual.0.eq_ignore_ascii_case(&expected.0)
                && actual.1.eq_ignore_ascii_case(&expected.1))
        })();
        match result {
            Ok(false) => std::ops::ControlFlow::Continue(()),
            Ok(true) => {
                found = true;
                std::ops::ControlFlow::Break(())
            }
            Err(error) => {
                failure = Some(error);
                std::ops::ControlFlow::Break(())
            }
        }
    });
    if let Some(error) = failure {
        return Err(error);
    }
    Ok(found)
}

fn references_bound_macro(default: &str, macros: &[(String, String)]) -> Result<bool> {
    if macros.is_empty() {
        return Ok(false);
    }
    let mut parser = Parser::new(&GenericDialect {}).try_with_sql(default)?;
    let expr = parser.parse_expr()?;
    ensure!(
        parser.peek_token().token == sqlparser::tokenizer::Token::EOF,
        "Unsupported native default dependency syntax"
    );
    let mut found = false;
    let _ = visit_expressions(&expr, |expr| {
        if let Expr::Function(function) = expr {
            let mut parts = function
                .name
                .0
                .iter()
                .rev()
                .filter_map(ObjectNamePart::as_ident);
            if let Some(name) = parts.next() {
                let schema = parts.next();
                found = macros.iter().any(|(bound_schema, bound_name)| {
                    name.value.eq_ignore_ascii_case(bound_name)
                        && schema
                            .is_none_or(|schema| schema.value.eq_ignore_ascii_case(bound_schema))
                });
                if found {
                    return std::ops::ControlFlow::Break(());
                }
            }
        }
        std::ops::ControlFlow::Continue(())
    });
    Ok(found)
}

/// Live column defaults referring to a private sequence. No sequence is read or advanced.
pub(crate) fn dependent_columns(
    db: &Connection,
    name: &str,
) -> Result<Vec<(String, String, String)>> {
    let expected = sequence_key(name, "")?;
    let opaque: i64 = db.query_row("SELECT count(*) FROM duckdb_dependencies() d JOIN duckdb_sequences() s ON d.objid=s.sequence_oid LEFT JOIN duckdb_tables() t ON d.refobjid=t.table_oid AND t.database_oid=s.database_oid WHERE s.database_name=current_database() AND s.schema_name=? COLLATE NOCASE AND s.sequence_name=? COLLATE NOCASE AND t.table_oid IS NULL", [&expected.0,&expected.1], |row| row.get(0))?;
    ensure!(
        opaque == 0,
        "Unsupported bound private sequence dependent object"
    );
    let mut dependencies = db.prepare("SELECT DISTINCT t.schema_name,t.table_name,s.schema_name,s.sequence_name FROM duckdb_dependencies() d JOIN duckdb_sequences() s ON d.objid=s.sequence_oid JOIN duckdb_tables() t ON d.refobjid=t.table_oid AND t.database_oid=s.database_oid WHERE t.database_name=current_database()")?;
    let bindings = dependencies
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<duckdb::Result<Vec<_>>>()?;
    let mut macro_dependencies = db.prepare("SELECT DISTINCT t.schema_name,t.table_name,f.schema_name,f.function_name FROM duckdb_dependencies() d JOIN duckdb_functions() f ON d.objid=f.function_oid JOIN duckdb_tables() t ON d.refobjid=t.table_oid WHERE t.database_name=current_database() AND f.function_type='macro'")?;
    let macros = macro_dependencies
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<duckdb::Result<Vec<_>>>()?;
    let mut statement = db.prepare("SELECT table_schema,table_name,column_name,column_default FROM information_schema.columns WHERE table_catalog=current_database() AND column_default IS NOT NULL")?;
    let mut dependents = Vec::new();
    for row in statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })? {
        let (table_schema, table, column, default) = row?;
        let table_bindings = bindings
            .iter()
            .filter(|(schema, name, _, _)| {
                schema.eq_ignore_ascii_case(&table_schema) && name.eq_ignore_ascii_case(&table)
            })
            .map(|(_, _, schema, name)| (schema.clone(), name.clone()))
            .collect::<Vec<_>>();
        if table_bindings.iter().any(|(schema, name)| {
            schema.eq_ignore_ascii_case(&expected.0) && name.eq_ignore_ascii_case(&expected.1)
        }) {
            let table_macros = macros
                .iter()
                .filter(|(schema, name, _, _)| {
                    schema.eq_ignore_ascii_case(&table_schema) && name.eq_ignore_ascii_case(&table)
                })
                .map(|(_, _, schema, name)| (schema.clone(), name.clone()))
                .collect::<Vec<_>>();
            ensure!(
                !references_bound_macro(&default, &table_macros)?,
                "Unsupported bound macro private sequence default dependency"
            );
        }
        if references_sequence(&default, name, &table_bindings)? {
            dependents.push((table_schema, table, column));
        }
    }
    for (schema, table, sequence_schema, sequence) in &bindings {
        if sequence_schema.eq_ignore_ascii_case(&expected.0)
            && sequence.eq_ignore_ascii_case(&expected.1)
        {
            ensure!(
                dependents
                    .iter()
                    .any(|(dependent_schema, dependent_table, _)| {
                        schema.eq_ignore_ascii_case(dependent_schema)
                            && table.eq_ignore_ascii_case(dependent_table)
                    }),
                "Unsupported unattributed bound private sequence dependency"
            );
        }
    }
    Ok(dependents)
}

fn ensure_no_dependents(db: &Connection, name: &str, dropping: &[(String, String)]) -> Result<()> {
    for (schema, table, column) in dependent_columns(db, name)? {
        if dropping.iter().any(|(drop_schema, drop_table)| {
            schema.eq_ignore_ascii_case(drop_schema) && table.eq_ignore_ascii_case(drop_table)
        }) {
            continue;
        }
        bail!(
            "Cannot drop private IDENTITY sequence {name}: still referenced by {schema}.{table}.{column}"
        );
    }
    Ok(())
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
    // Inspect live defaults before removal; native dependency tracking can lose
    // default edges following ALTER inside the same transaction. Never cascade.
    ensure_no_dependents(db, name, &[])?;
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
        let dropping = names.iter().filter_map(table_key).collect::<Vec<_>>();
        for sequence in &sequences {
            ensure_no_dependents(db, sequence, &dropping)?;
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
    #[test]
    fn opaque_bound_macro_defaults_preserve_destructive_ddl_atomicity() {
        for same_table in [false, true] {
            for expression in ["main.hidden()", "coalesce(main.hidden(),1)+1"] {
                let db = Connection::open_in_memory().unwrap();
                db.execute_batch("CREATE SCHEMA dbo").unwrap();
                let parse = |sql: &str| {
                    Parser::parse_sql(&sqlparser::dialect::MsSqlDialect {}, sql)
                        .unwrap()
                        .remove(0)
                };
                create(
                    &db,
                    &parse("CREATE TABLE dbo.ids(id INT IDENTITY(10,5),v INT,keeper INT)"),
                    true,
                )
                .unwrap();
                db.execute_batch("INSERT INTO dbo.ids(v,keeper) VALUES(1,7)")
                    .unwrap();
                let name = table_sequences(
                    &db,
                    &ObjectName::from(vec![Ident::new("dbo"), Ident::new("ids")]),
                )
                .unwrap()
                .remove(0);
                db.execute_batch(&format!("CREATE MACRO main.hidden() AS nextval('{name}')"))
                    .unwrap();
                if same_table {
                    db.execute_batch(&format!(
                        "ALTER TABLE dbo.ids ADD COLUMN hidden_value BIGINT DEFAULT ({expression})"
                    ))
                    .unwrap();
                } else {
                    db.execute_batch(&format!(
                        "CREATE TABLE dbo.dep(d BIGINT DEFAULT ({expression}))"
                    ))
                    .unwrap();
                }
                let allocation: i64 = db
                    .query_row("SELECT last_value FROM duckdb_sequences()", [], |row| {
                        row.get(0)
                    })
                    .unwrap();
                db.execute_batch("BEGIN").unwrap();
                let error = dependent_columns(&db, &name).unwrap_err();
                assert!(error.to_string().contains("Unsupported bound macro"));
                let Statement::AlterTable(mut drop) = parse("ALTER TABLE dbo.ids DROP COLUMN v")
                else {
                    panic!()
                };
                let Statement::AlterTable(id) = parse("ALTER TABLE dbo.ids DROP COLUMN id") else {
                    panic!()
                };
                drop.operations.extend(id.operations);
                assert!(crate::table_alter::execute(&db, &drop, false).is_err());
                assert!(drop_table(&db, &parse("DROP TABLE dbo.ids"), false).is_err());
                assert_eq!(
                    db.query_row("SELECT id,v,keeper FROM dbo.ids", [], |row| Ok((
                        row.get::<_, i32>(0)?,
                        row.get::<_, i32>(1)?,
                        row.get::<_, i32>(2)?
                    )))
                    .unwrap(),
                    (10, 1, 7)
                );
                assert_eq!(
                    db.query_row("SELECT last_value FROM duckdb_sequences()", [], |row| row
                        .get::<_, i64>(
                        0
                    ))
                    .unwrap(),
                    allocation
                );
                let Statement::AlterTable(unrelated) = parse("ALTER TABLE dbo.ids DROP COLUMN v")
                else {
                    panic!()
                };
                crate::table_alter::execute(&db, &unrelated, false).unwrap();
                db.execute_batch("ROLLBACK").unwrap();
                assert_eq!(
                    db.query_row("SELECT v FROM dbo.ids", [], |row| row.get::<_, i32>(0))
                        .unwrap(),
                    1
                );
                if same_table {
                    db.execute_batch("ALTER TABLE dbo.ids DROP COLUMN hidden_value")
                        .unwrap();
                } else {
                    db.execute_batch("DROP TABLE dbo.dep").unwrap();
                }
                drop_table(&db, &parse("DROP TABLE dbo.ids"), true).unwrap();
            }
        }
    }

    #[test]
    fn ordinary_builtin_defaults_allow_identity_removal() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE SCHEMA dbo").unwrap();
        let parse = |sql: &str| {
            Parser::parse_sql(&sqlparser::dialect::MsSqlDialect {}, sql)
                .unwrap()
                .remove(0)
        };
        create(
            &db,
            &parse("CREATE TABLE dbo.ids(id INT IDENTITY(10,5),keeper INT)"),
            true,
        )
        .unwrap();
        db.execute_batch("INSERT INTO dbo.ids(keeper) VALUES(7); ALTER TABLE dbo.ids ADD COLUMN magnitude INT DEFAULT abs(-3); ALTER TABLE dbo.ids ADD COLUMN units INT DEFAULT length('abc'); ALTER TABLE dbo.ids ADD COLUMN at_time TIMESTAMP DEFAULT current_timestamp").unwrap();
        let Statement::AlterTable(drop) = parse("ALTER TABLE dbo.ids DROP COLUMN id") else {
            panic!()
        };
        crate::table_alter::execute(&db, &drop, true).unwrap();
        assert_eq!(
            db.query_row(
                "SELECT keeper,magnitude,units,at_time IS NOT NULL FROM dbo.ids",
                [],
                |row| Ok((
                    row.get::<_, i32>(0)?,
                    row.get::<_, i32>(1)?,
                    row.get::<_, i32>(2)?,
                    row.get::<_, bool>(3)?
                ))
            )
            .unwrap(),
            (7, 3, 3, true)
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM duckdb_sequences()", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn currval_dependencies_preflight_before_destructive_ddl() {
        for qualified in [true, false] {
            let db = Connection::open_in_memory().unwrap();
            db.execute_batch("CREATE SCHEMA dbo").unwrap();
            let parse = |sql: &str| {
                Parser::parse_sql(&sqlparser::dialect::MsSqlDialect {}, sql)
                    .unwrap()
                    .remove(0)
            };
            create(
                &db,
                &parse("CREATE TABLE dbo.ids(id INT IDENTITY(10,5),v INT,keeper INT)"),
                true,
            )
            .unwrap();
            db.execute_batch("INSERT INTO dbo.ids(v,keeper) VALUES(1,7)")
                .unwrap();
            let name = table_sequences(
                &db,
                &ObjectName::from(vec![Ident::new("dbo"), Ident::new("ids")]),
            )
            .unwrap()
            .remove(0);
            let argument = if qualified {
                name.as_str()
            } else {
                name.strip_prefix("main.").unwrap()
            };
            db.execute_batch(&format!("SET schema='dbo'; CREATE TABLE dbo.dep(d BIGINT DEFAULT currval('{argument}')); BEGIN")).unwrap();
            let dependents = dependent_columns(&db, &name).unwrap();
            assert!(
                dependents
                    .iter()
                    .any(|(schema, table, column)| schema == "dbo"
                        && table == "dep"
                        && column == "d")
            );
            let Statement::AlterTable(mut drop) = parse("ALTER TABLE dbo.ids DROP COLUMN v") else {
                panic!()
            };
            let Statement::AlterTable(id) = parse("ALTER TABLE dbo.ids DROP COLUMN id") else {
                panic!()
            };
            drop.operations.extend(id.operations);
            assert!(crate::table_alter::execute(&db, &drop, false).is_err());
            assert!(drop_table(&db, &parse("DROP TABLE dbo.ids"), false).is_err());
            assert_eq!(
                db.query_row("SELECT id,v,keeper FROM dbo.ids", [], |row| Ok((
                    row.get::<_, i32>(0)?,
                    row.get::<_, i32>(1)?,
                    row.get::<_, i32>(2)?
                )))
                .unwrap(),
                (10, 1, 7)
            );
            db.execute_batch("INSERT INTO dbo.dep DEFAULT VALUES; COMMIT")
                .unwrap();
            assert_eq!(
                db.query_row("SELECT d FROM dbo.dep", [], |row| row.get::<_, i64>(0))
                    .unwrap(),
                10
            );
            assert_eq!(
                db.query_row("SELECT last_value FROM duckdb_sequences()", [], |row| row
                    .get::<_, i64>(
                    0
                ))
                .unwrap(),
                10
            );
            db.execute_batch("DROP TABLE dbo.dep").unwrap();
            drop_table(&db, &parse("DROP TABLE dbo.ids"), true).unwrap();
            assert_eq!(
                db.query_row("SELECT count(*) FROM duckdb_sequences()", [], |row| row
                    .get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
    }

    #[test]
    fn bound_default_dependencies_survive_schema_changes_and_shadowing() {
        for context in ["SET schema='dbo'", "SET search_path='dbo,main'"] {
            let db = Connection::open_in_memory().unwrap();
            db.execute_batch("CREATE SCHEMA dbo").unwrap();
            let parse = |sql: &str| {
                Parser::parse_sql(&sqlparser::dialect::MsSqlDialect {}, sql)
                    .unwrap()
                    .remove(0)
            };
            create(
                &db,
                &parse("CREATE TABLE dbo.ids(id INT IDENTITY(10,5),v INT,keeper INT)"),
                true,
            )
            .unwrap();
            db.execute_batch("INSERT INTO dbo.ids(v,keeper) VALUES(1,7)")
                .unwrap();
            let name = table_sequences(
                &db,
                &ObjectName::from(vec![Ident::new("dbo"), Ident::new("ids")]),
            )
            .unwrap()
            .remove(0);
            let basename = name.strip_prefix("main.").unwrap();
            db.execute_batch(&format!("CREATE TABLE main.dep(d BIGINT DEFAULT nextval('{basename}')); CREATE SEQUENCE dbo.{basename} START 100; {context}; CREATE TABLE dbo.shadow(d BIGINT DEFAULT nextval('{basename}')); BEGIN" )).unwrap();
            let dependents = dependent_columns(&db, &name).unwrap();
            assert!(dependents.iter().any(|(schema, table, column)| {
                schema == "main" && table == "dep" && column == "d"
            }));
            assert!(!dependents.iter().any(|(_, table, _)| table == "shadow"));
            let Statement::AlterTable(mut alter) = parse("ALTER TABLE dbo.ids DROP COLUMN v")
            else {
                panic!("expected ALTER TABLE");
            };
            let Statement::AlterTable(id) = parse("ALTER TABLE dbo.ids DROP COLUMN id") else {
                panic!("expected ALTER TABLE");
            };
            alter.operations.extend(id.operations);
            assert!(crate::table_alter::execute(&db, &alter, false).is_err());
            assert!(drop_table(&db, &parse("DROP TABLE dbo.ids"), false).is_err());
            assert_eq!(
                db.query_row("SELECT id,v,keeper FROM dbo.ids", [], |row| {
                    Ok((
                        row.get::<_, i32>(0)?,
                        row.get::<_, i32>(1)?,
                        row.get::<_, i32>(2)?,
                    ))
                })
                .unwrap(),
                (10, 1, 7)
            );
            assert_eq!(
                db.query_row(
                    "SELECT last_value FROM duckdb_sequences() WHERE schema_name='main'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
                10
            );
            db.execute_batch("INSERT INTO main.dep DEFAULT VALUES; INSERT INTO dbo.shadow DEFAULT VALUES; COMMIT").unwrap();
            assert_eq!(
                db.query_row("SELECT d FROM main.dep", [], |row| row.get::<_, i64>(0))
                    .unwrap(),
                15
            );
            assert_eq!(
                db.query_row("SELECT d FROM dbo.shadow", [], |row| row.get::<_, i64>(0))
                    .unwrap(),
                100
            );
            db.execute_batch("DROP TABLE main.dep").unwrap();
            drop_table(&db, &parse("DROP TABLE dbo.ids"), true).unwrap();
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM duckdb_sequences() WHERE schema_name='main'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
                0
            );
            db.execute_batch("INSERT INTO dbo.shadow DEFAULT VALUES")
                .unwrap();
            assert_eq!(
                db.query_row("SELECT max(d) FROM dbo.shadow", [], |row| row
                    .get::<_, i64>(0))
                    .unwrap(),
                101
            );
        }
    }

    #[test]
    fn ambiguous_unqualified_bound_defaults_remain_explicit() {
        let db = Connection::open_in_memory().unwrap();
        let basename = "__msduck_identity_11111111111111111111111111111111";
        db.execute_batch(&format!("CREATE SCHEMA dbo; CREATE SEQUENCE main.{basename} START 10; CREATE SEQUENCE dbo.{basename} START 100; CREATE TABLE main.dep(a BIGINT DEFAULT nextval('main.{basename}'),b BIGINT DEFAULT nextval('dbo.{basename}'),c BIGINT DEFAULT nextval('{basename}'))")).unwrap();
        let error = dependent_columns(&db, &format!("main.{basename}")).unwrap_err();
        assert!(error.to_string().contains("Ambiguous bound"));
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM duckdb_sequences() WHERE last_value IS NULL",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            2
        );
        db.execute_batch("INSERT INTO main.dep DEFAULT VALUES")
            .unwrap();
        assert_eq!(
            db.query_row("SELECT a,b,c FROM main.dep", [], |row| Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?
            )))
            .unwrap(),
            (10, 100, 11)
        );
    }

    #[test]
    fn dependency_preflight_keeps_prior_ddl_and_caller_transaction_usable() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE SCHEMA dbo").unwrap();
        let parse = |sql: &str| {
            Parser::parse_sql(&sqlparser::dialect::MsSqlDialect {}, sql)
                .unwrap()
                .remove(0)
        };
        create(
            &db,
            &parse("CREATE TABLE dbo.ids(id INT IDENTITY(10,5),keeper INT)"),
            true,
        )
        .unwrap();
        db.execute_batch("INSERT INTO dbo.ids(keeper) VALUES(7)")
            .unwrap();
        let name = table_sequences(
            &db,
            &ObjectName::from(vec![Ident::new("dbo"), Ident::new("ids")]),
        )
        .unwrap()
        .remove(0);
        db.execute_batch(&format!("CREATE TABLE dbo.dependent(d BIGINT DEFAULT (nextval('{name}')+1)); BEGIN; ALTER TABLE dbo.ids ADD COLUMN prior_change INT")).unwrap();
        assert!(drop_table(&db, &parse("DROP TABLE dbo.ids"), false).is_err());
        assert_eq!(
            db.query_row(
                "SELECT id,keeper,prior_change IS NULL FROM dbo.ids",
                [],
                |row| Ok((
                    row.get::<_, i32>(0)?,
                    row.get::<_, i32>(1)?,
                    row.get::<_, bool>(2)?
                ))
            )
            .unwrap(),
            (10, 7, true)
        );
        assert_eq!(
            db.query_row("SELECT last_value FROM duckdb_sequences()", [], |row| row
                .get::<_, i64>(
                0
            ))
            .unwrap(),
            10
        );
        db.execute_batch("UPDATE dbo.ids SET prior_change=9; COMMIT")
            .unwrap();
        assert_eq!(
            db.query_row("SELECT prior_change FROM dbo.ids", [], |row| row
                .get::<_, i32>(0))
                .unwrap(),
            9
        );
        db.execute_batch("DROP TABLE dbo.dependent").unwrap();
        drop_table(&db, &parse("DROP TABLE dbo.ids"), true).unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM duckdb_sequences()", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn native_default_dependencies_cover_nested_quoted_cast_and_constant_names() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE SCHEMA dbo").unwrap();
        let ddl = Parser::parse_sql(
            &sqlparser::dialect::MsSqlDialect {},
            "CREATE TABLE dbo.ids(id INT IDENTITY(10,5), keeper INT)",
        )
        .unwrap()
        .remove(0);
        create(&db, &ddl, true).unwrap();
        db.execute_batch("INSERT INTO dbo.ids(keeper) VALUES(1)")
            .unwrap();
        let name = table_sequences(
            &db,
            &ObjectName::from(vec![Ident::new("dbo"), Ident::new("ids")]),
        )
        .unwrap()
        .remove(0);
        let basename = name.strip_prefix("main.").unwrap();
        for (index, expression) in [
            format!("nextval('{}')+1", name.to_ascii_uppercase()),
            format!("nextval('main.\"{basename}\"')"),
            format!("nextval('{name}'::VARCHAR)+1"),
            format!("nextval('main.'||'{basename}')"),
            format!("\"nextval\"('{name}')"),
            format!("nextval('{basename}')"),
            format!("currval('{}')+1", name.to_ascii_uppercase()),
            format!("currval('{basename}'::VARCHAR)"),
        ]
        .iter()
        .enumerate()
        {
            db.execute_batch(&format!(
                "CREATE TABLE dbo.dep_{index}(d BIGINT DEFAULT ({expression}))"
            ))
            .unwrap();
        }
        db.execute_batch(&format!(
            "CREATE TABLE dbo.literal(d VARCHAR DEFAULT '{name}')"
        ))
        .unwrap();
        let dependents = dependent_columns(&db, &name).unwrap();
        assert_eq!(dependents.len(), 9);
        for index in 0..8 {
            assert!(
                dependents
                    .iter()
                    .any(|(schema, table, column)| schema == "dbo"
                        && table == &format!("dep_{index}")
                        && column == "d")
            );
        }
        assert!(!dependents.iter().any(|(_, table, _)| table == "literal"));
        assert_eq!(
            db.query_row("SELECT last_value FROM duckdb_sequences()", [], |row| row
                .get::<_, i64>(
                0
            ))
            .unwrap(),
            10
        );
        assert!(references_sequence("nextval(lower('main.example'))", &name, &[]).is_err());
        assert!(!references_sequence("nextval(NULL)", &name, &[]).unwrap());
    }

    use super::*;
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
            "CREATE TABLE dbo.dependency(id INT DEFAULT nextval('{name}'))"
        ))
        .unwrap();
        assert!(drop_table(&db, &drop, true).is_err());
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
