//! Change a table's native PRIMARY KEY and UNIQUE constraints. DuckDB can add
//! a primary key but cannot add a UNIQUE constraint or drop either, so the
//! table is rebuilt under the same name: its catalog identity (object and
//! column ids, which are keyed by name), rows, defaults and indexes survive.
use super::catalog::{Table, quote};
use crate::engine::Session;
use anyhow::{Context, Result, bail};
use sqlparser::ast::*;

/// A native key: primary or unique, over column names.
#[derive(Clone, Debug)]
pub(crate) struct Key {
    pub primary: bool,
    pub columns: Vec<String>,
}

fn key_columns(columns: &[IndexColumn]) -> Result<Vec<String>> {
    columns
        .iter()
        .map(|column| match &column.column.expr {
            Expr::Identifier(ident) => Ok(ident.value.clone()),
            other => bail!("unsupported native key column {other}"),
        })
        .collect()
}

fn index_column(name: &str) -> IndexColumn {
    IndexColumn {
        column: OrderByExpr {
            expr: Expr::Identifier(Ident::with_quote('"', name)),
            options: OrderByOptions {
                sort: None,
                nulls_first: None,
            },
            with_fill: None,
        },
        operator_class: None,
    }
}

/// Rebuild `table` with the keys `edit` leaves.
pub(crate) fn keys(
    session: &mut Session,
    table: &Table,
    edit: impl FnOnce(&mut Vec<Key>) -> Result<()>,
) -> Result<()> {
    let db = &session.db;
    let sql: String = db
        .query_row(
            "SELECT sql FROM duckdb_tables() WHERE database_name=current_database() AND schema_name=? AND table_name=?",
            [&table.schema, &table.name],
            |row| row.get(0),
        )
        .with_context(|| format!("table {} has no native definition", table.display()))?;
    let statements =
        sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::DuckDbDialect {}, &sql)
            .with_context(|| format!("unsupported native definition of {}", table.display()))?;
    let Some(Statement::CreateTable(mut create)) = statements.into_iter().next() else {
        bail!("unsupported native definition of {}", table.display());
    };
    let mut keys = Vec::new();
    for column in &mut create.columns {
        let name = column.name.value.clone();
        column.options.retain(|option| match &option.option {
            ColumnOption::PrimaryKey(_) => {
                keys.push(Key {
                    primary: true,
                    columns: vec![name.clone()],
                });
                false
            }
            ColumnOption::Unique(_) => {
                keys.push(Key {
                    primary: false,
                    columns: vec![name.clone()],
                });
                false
            }
            _ => true,
        });
    }
    let mut constraints = Vec::new();
    for constraint in create.constraints.drain(..) {
        match &constraint {
            TableConstraint::PrimaryKey(key) => keys.push(Key {
                primary: true,
                columns: key_columns(&key.columns)?,
            }),
            TableConstraint::Unique(key) => keys.push(Key {
                primary: false,
                columns: key_columns(&key.columns)?,
            }),
            _ => constraints.push(constraint),
        }
    }
    let primary_before: Vec<String> = keys
        .iter()
        .filter(|key| key.primary)
        .flat_map(|key| key.columns.iter().map(|c| c.to_lowercase()))
        .collect();
    edit(&mut keys)?;
    // SQL Server key columns stay NOT NULL after their primary key is dropped.
    for column in &mut create.columns {
        if primary_before.contains(&column.name.value.to_lowercase())
            && !column
                .options
                .iter()
                .any(|o| matches!(o.option, ColumnOption::NotNull))
        {
            column.options.push(ColumnOptionDef {
                name: None,
                option: ColumnOption::NotNull,
            });
        }
    }
    for key in &keys {
        let columns = key.columns.iter().map(|c| index_column(c)).collect();
        constraints.push(if key.primary {
            TableConstraint::PrimaryKey(PrimaryKeyConstraint {
                name: None,
                index_name: None,
                index_type: None,
                columns,
                include: vec![],
                index_options: vec![],
                characteristics: None,
            })
        } else {
            TableConstraint::Unique(UniqueConstraint {
                name: None,
                index_name: None,
                index_type_display: KeyOrIndexDisplay::None,
                index_type: None,
                columns,
                include: vec![],
                index_options: vec![],
                characteristics: None,
                nulls_distinct: NullsDistinctOption::None,
            })
        });
    }
    create.constraints = constraints;
    let stored: Vec<String> = create
        .columns
        .iter()
        .filter(|column| {
            !column.options.iter().any(|o| {
                matches!(
                    o.option,
                    ColumnOption::Generated { .. } | ColumnOption::Alias(_)
                )
            })
        })
        .map(|column| quote(&column.name.value))
        .collect();
    let temporary = format!("__msduck_rebuild_{}", table.id);
    create.name = ObjectName(vec![
        ObjectNamePart::Identifier(Ident::with_quote('"', &table.schema)),
        ObjectNamePart::Identifier(Ident::with_quote('"', &temporary)),
    ]);
    let indexes = {
        let mut statement = db.prepare(
            "SELECT sql FROM duckdb_indexes() WHERE database_name=current_database() AND schema_name=? AND table_name=? AND sql IS NOT NULL",
        )?;
        let rows =
            statement.query_map([&table.schema, &table.name], |row| row.get::<_, String>(0))?;
        rows.collect::<duckdb::Result<Vec<_>>>()?
    };
    let list = stored.join(",");
    db.execute_batch(&create.to_string())?;
    db.execute_batch(&format!(
        "INSERT INTO {}.{} ({list}) SELECT {list} FROM {} ORDER BY rowid",
        quote(&table.schema),
        quote(&temporary),
        table.sql()
    ))?;
    if let Err(error) = db.execute_batch(&format!("DROP TABLE {}", table.sql())) {
        bail!(
            "unsupported key change on {}: the table is referenced by a native foreign key ({error})",
            table.display()
        );
    }
    db.execute_batch(&format!(
        "ALTER TABLE {}.{} RENAME TO {}",
        quote(&table.schema),
        quote(&temporary),
        quote(&table.name)
    ))?;
    for index in indexes {
        db.execute_batch(&index)?;
    }
    crate::object_catalog::sync(db)?;
    crate::index_catalog::sync(db)?;
    db.execute(
        "UPDATE main.__msduck_objects SET modify_date=CAST(current_timestamp AS TIMESTAMP) WHERE object_id=?",
        [table.id],
    )?;
    Ok(())
}
