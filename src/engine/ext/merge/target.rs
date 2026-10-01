//! The MERGE target table: its columns, storage conversions and defaults.
use crate::engine::Session;
use anyhow::{Result, anyhow, bail};
use msduck_core::diagnostic::SqlError;
use sqlparser::{ast::*, dialect::GenericDialect, parser::Parser};

/// A converted SET or VALUES expression.
pub(super) enum Converted {
    /// Native SQL of the stored value.
    Value(String),
    /// Native SQL of a `{value, error}` pair from a checked character
    /// conversion; `unicode` selects the value's decoding.
    Checked { value: String, unicode: bool },
}

pub(super) struct Column {
    pub name: String,
    /// The backend type, as `information_schema` reports it.
    pub physical: String,
    /// The backend type, or the SQL Server declaration it erases.
    declared: String,
    default: Option<String>,
    pub identity: bool,
    pub generated: bool,
    nullable: bool,
    utf16: bool,
}

impl Column {
    /// A computed column's backend expression.
    pub fn expression(&self) -> Option<&str> {
        self.generated.then_some(self.default.as_deref()).flatten()
    }
}

/// A PRIMARY KEY or UNIQUE constraint.
struct Key {
    primary: bool,
    columns: Vec<usize>,
}

pub(super) struct Target {
    pub schema: String,
    pub table: String,
    pub columns: Vec<Column>,
    /// Backend CHECK constraint expressions and their text.
    checks: Vec<(String, String)>,
    keys: Vec<Key>,
}

impl Target {
    pub fn load(session: &Session, name: &ObjectName) -> Result<Self> {
        let parts = name
            .0
            .iter()
            .map(|p| p.as_ident().map(|id| id.value.clone()))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| anyhow!("unsupported MERGE target {name}"))?;
        let (schema, table) = match parts.as_slice() {
            [table] => ("dbo".to_owned(), table.clone()),
            [schema, table] => (schema.clone(), table.clone()),
            _ => bail!("unsupported MERGE target {name}"),
        };
        let db = &session.db;
        let written = parts.join(".");
        let kind: Option<String> = db
            .query_row(
                "SELECT o.type FROM sys.objects o WHERE o.object_id=__msduck_object_id(?,NULL)",
                [ObjectName::from(vec![
                    Ident::with_quote('[', &schema),
                    Ident::with_quote('[', &table),
                ])
                .to_string()],
                |r| r.get(0),
            )
            .ok();
        match kind.as_deref().map(str::trim) {
            Some("U") => {}
            Some(_) => bail!("unsupported MERGE target {written}: only base tables can be merged"),
            None => {
                return Err(
                    SqlError::new(208, 1, format!("Invalid object name '{written}'.")).into(),
                );
            }
        }
        let mut statement = db.prepare("SELECT column_name,data_type,column_default,is_nullable='YES' FROM information_schema.columns WHERE table_catalog=current_database() AND table_schema=? COLLATE NOCASE AND table_name=? COLLATE NOCASE ORDER BY ordinal_position")?;
        let described = statement
            .query_map([&schema, &table], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, bool>(3)?,
                ))
            })?
            .collect::<duckdb::Result<Vec<_>>>()?;
        let nullable = described.iter().map(|c| c.3).collect::<Vec<_>>();
        let mut columns = described
            .into_iter()
            .map(|(name, kind, default, _)| (name, kind, default))
            .collect::<Vec<_>>();
        let physical = columns.iter().map(|c| c.1.clone()).collect::<Vec<_>>();
        let utf16 = crate::assignment::utf16_targets(&columns);
        crate::assignment::declared_targets(db, &schema, &table, &mut columns)?;
        let generated = crate::computed_columns::names(db, &schema, &table)?;
        let columns = columns
            .into_iter()
            .zip(physical)
            .zip(nullable)
            .map(|(((name, declared, default), physical), nullable)| Column {
                identity: crate::identity::is_default(default.as_deref()),
                generated: generated.contains(&name.to_lowercase()),
                utf16: utf16.contains(&name.to_lowercase()),
                nullable,
                name,
                physical,
                declared,
                default,
            })
            .collect::<Vec<_>>();
        if columns.iter().any(|c| c.name.eq_ignore_ascii_case("rowid")) {
            bail!(
                "unsupported MERGE target {written}: a column named rowid hides the row identity"
            );
        }
        let mut statement = db.prepare("SELECT constraint_type,constraint_text,expression,constraint_column_names FROM duckdb_constraints() WHERE database_name=current_database() AND schema_name=? COLLATE NOCASE AND table_name=? COLLATE NOCASE AND constraint_type IN ('CHECK','PRIMARY KEY','UNIQUE')")?;
        let constraints = statement
            .query_map([&schema, &table], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, duckdb::types::Value>(3)?,
                ))
            })?
            .collect::<duckdb::Result<Vec<_>>>()?;
        let mut checks = Vec::new();
        let mut keys = Vec::new();
        for (kind, text, expression, names) in constraints {
            if kind == "CHECK" {
                if let Some(expression) = expression {
                    checks.push((expression, text));
                }
                continue;
            }
            let duckdb::types::Value::List(names) = names else {
                continue;
            };
            let indexes = names
                .iter()
                .map(|name| match name {
                    duckdb::types::Value::Text(name) => columns
                        .iter()
                        .position(|c: &Column| c.name.eq_ignore_ascii_case(name)),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>();
            if let Some(columns) = indexes {
                keys.push(Key {
                    primary: kind == "PRIMARY KEY",
                    columns,
                });
            }
        }
        Ok(Self {
            schema,
            table,
            columns,
            checks,
            keys,
        })
    }

    /// Reject the new images before any write when they violate NOT NULL,
    /// CHECK or (with `keys`) PRIMARY KEY and UNIQUE constraints. Each
    /// failure terminates the statement, as a native violation would.
    pub fn check(&self, session: &Session, images: &str, keys: bool, database: &str) -> Result<()> {
        let db = &session.db;
        let terminate =
            |error: SqlError| crate::query_error::attach_context(error.into(), vec![], 0xc5);
        let written = "__msduck_kind IN (1, 2)";
        for (index, column) in self.columns.iter().enumerate() {
            if column.nullable || column.generated {
                continue;
            }
            let null: bool = db.query_row(
                &format!("SELECT EXISTS(SELECT 1 FROM {images} WHERE {written} AND \"__msduck_n{index}\" IS NULL)"),
                [],
                |r| r.get(0),
            )?;
            if null {
                return Err(terminate(SqlError::new(
                    515,
                    2,
                    format!(
                        "Cannot insert the value NULL into column '{}', table '{database}.{}.{}'; column does not allow nulls. UPDATE fails.",
                        column.name, self.schema, self.table
                    ),
                )));
            }
        }
        let row = self
            .columns
            .iter()
            .enumerate()
            .map(|(index, column)| {
                format!(
                    "\"__msduck_n{index}\" AS {}",
                    Ident::with_quote('"', &column.name)
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        for (expression, text) in &self.checks {
            let failed: bool = db.query_row(
                &format!("SELECT EXISTS(SELECT 1 FROM (SELECT {row} FROM {images} WHERE {written}) AS __c WHERE NOT COALESCE(({expression}), TRUE))"),
                [],
                |r| r.get(0),
            )?;
            if failed {
                return Err(terminate(SqlError::new(
                    547,
                    0,
                    format!(
                        "Constraint Error: CHECK constraint failed on table {} with expression {text}",
                        self.table
                    ),
                )));
            }
        }
        if !keys {
            return Ok(());
        }
        let table = self.backend_name();
        for key in &self.keys {
            let names = key
                .columns
                .iter()
                .map(|index| Ident::with_quote('"', &self.columns[*index].name).to_string())
                .collect::<Vec<_>>();
            let images_key = key
                .columns
                .iter()
                .map(|index| format!("\"__msduck_n{index}\""))
                .collect::<Vec<_>>();
            let shown = key
                .columns
                .iter()
                .enumerate()
                .map(|(position, index)| {
                    format!(
                        "'{}: ' || CAST(\"__msduck_k{position}\" AS VARCHAR)",
                        self.columns[*index].name.replace('\'', "''")
                    )
                })
                .collect::<Vec<_>>()
                .join(" || ', ' || ");
            let aliases = (0..key.columns.len())
                .map(|position| format!("\"__msduck_k{position}\""))
                .collect::<Vec<_>>();
            let present = aliases
                .iter()
                .map(|alias| format!("{alias} IS NOT NULL"))
                .collect::<Vec<_>>()
                .join(" AND ");
            let sql = format!(
                "SELECT {shown} FROM (SELECT {} FROM {table} WHERE rowid NOT IN (SELECT __msduck_tid FROM {images} WHERE __msduck_tid IS NOT NULL) UNION ALL SELECT {} FROM {images} WHERE {written}) AS __k({}) WHERE {present} GROUP BY ALL HAVING count(*) > 1 LIMIT 1",
                names.join(", "),
                images_key.join(", "),
                aliases.join(", ")
            );
            let duplicate: Option<String> = match db.query_row(&sql, [], |r| r.get(0)) {
                Ok(value) => Some(value),
                Err(duckdb::Error::QueryReturnedNoRows) => None,
                Err(error) => return Err(error.into()),
            };
            if let Some(value) = duplicate {
                return Err(terminate(SqlError {
                    number: 2627,
                    state: 1,
                    severity: 14,
                    message: format!(
                        "Constraint Error: Duplicate key \"{value}\" violates {} constraint.",
                        if key.primary { "primary key" } else { "unique" }
                    ),
                    message_utf16: None,
                }));
            }
        }
        Ok(())
    }

    pub fn backend_name(&self) -> String {
        format!(
            "{}.{}",
            Ident::with_quote('"', &self.schema),
            Ident::with_quote('"', &self.table)
        )
    }

    fn find(&self, name: &Ident) -> Result<usize> {
        self.columns
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(&name.value))
            .ok_or_else(|| {
                SqlError::new(207, 1, format!("Invalid column name '{}'.", name.value)).into()
            })
    }

    fn writable(&self, index: usize) -> Result<()> {
        if self.columns[index].generated {
            return Err(SqlError::new(
                271,
                1,
                format!(
                    "The column \"{}\" cannot be modified because it is either a computed column or is the result of a UNION operator.",
                    self.columns[index].name
                ),
            )
            .into());
        }
        Ok(())
    }

    /// The column an UPDATE SET clause assigns (`n` or `t.n`).
    pub fn assigned(&self, name: &ObjectName, qualifier: &Ident) -> Result<usize> {
        let parts = name
            .0
            .iter()
            .map(|p| p.as_ident())
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| anyhow!("unsupported MERGE assignment target {name}"))?;
        let column = match parts.as_slice() {
            [column] => column,
            [table, column]
                if table.value.eq_ignore_ascii_case(&qualifier.value)
                    || table.value.eq_ignore_ascii_case(&self.table) =>
            {
                column
            }
            _ => {
                return Err(SqlError::new(
                    4104,
                    1,
                    format!("The multi-part identifier \"{name}\" could not be bound."),
                )
                .into());
            }
        };
        let index = self.find(column)?;
        self.writable(index)?;
        if self.columns[index].identity {
            return Err(SqlError::new(
                8102,
                1,
                format!(
                    "Cannot update identity column '{}'.",
                    self.columns[index].name
                ),
            )
            .into());
        }
        Ok(index)
    }

    /// The columns an INSERT clause names, or every insertable column.
    pub fn insert_columns(&self, names: &[ObjectName]) -> Result<Vec<usize>> {
        if names.is_empty() {
            return Ok(self
                .columns
                .iter()
                .enumerate()
                .filter(|(_, c)| !c.generated && !c.identity)
                .map(|(index, _)| index)
                .collect());
        }
        let mut columns = Vec::new();
        for name in names {
            let column = name
                .0
                .last()
                .and_then(|p| p.as_ident())
                .ok_or_else(|| anyhow!("unsupported MERGE INSERT column {name}"))?;
            let index = self.find(column)?;
            self.writable(index)?;
            if self.columns[index].identity {
                return Err(SqlError::new(
                    544,
                    1,
                    format!(
                        "Cannot insert explicit value for identity column in table '{}' when IDENTITY_INSERT is set to OFF.",
                        self.table
                    ),
                )
                .into());
            }
            if columns.contains(&index) {
                return Err(SqlError::new(
                    264,
                    1,
                    format!(
                        "The column name '{}' is specified more than once in the SET clause or column list of an INSERT. A column cannot be assigned more than one value in the same clause. Modify the clause to make sure that a column is updated only once. If this statement updates or inserts columns into a view, column aliasing can conceal the duplication in your code.",
                        self.columns[index].name
                    ),
                )
                .into());
            }
            columns.push(index);
        }
        Ok(columns)
    }

    /// Convert a lowered value to column `index`'s storage, as INSERT and
    /// UPDATE do, so the staged value is exactly what will be stored.
    pub fn convert(
        &self,
        index: usize,
        value: Expr,
        money: bool,
        database: &str,
    ) -> Result<Converted> {
        let column = &self.columns[index];
        let Some(kind) = crate::assignment::storage_kind(&column.declared) else {
            return Ok(Converted::Value(format!(
                "CAST({value} AS {})",
                column.physical
            )));
        };
        let mut value = crate::storage_diagnostic::contextualize(
            crate::assignment::convert_for_storage(value, &kind, money, column.utf16),
            database,
            &self.schema,
            &self.table,
            &column.name,
        );
        // Character storage reports truncation (2628) as data, which keeps
        // the backend transaction usable; the caller raises it.
        if let Expr::Function(function) = &mut value {
            let checked = match function.name.to_string().as_str() {
                "__msduck_store_context_varchar" => Some(("__msduck_check_store_varchar", false)),
                "__msduck_store_context_char" => Some(("__msduck_check_store_char", false)),
                "__msduck_store_context_nvarchar" => Some(("__msduck_check_store_nvarchar", true)),
                "__msduck_store_context_nchar" => Some(("__msduck_check_store_nchar", true)),
                _ => None,
            };
            if let Some((name, unicode)) = checked {
                function.name = ObjectName::from(vec![Ident::new(name)]);
                return Ok(Converted::Checked {
                    value: value.to_string(),
                    unicode,
                });
            }
        }
        Ok(Converted::Value(format!(
            "CAST({value} AS {})",
            column.physical
        )))
    }

    /// Column `index`'s default as native SQL, or NULL.
    pub fn default(&self, index: usize) -> Result<String> {
        let column = &self.columns[index];
        let Some(default) = &column.default else {
            return Ok(format!("CAST(NULL AS {})", column.physical));
        };
        let value = Parser::new(&GenericDialect {})
            .try_with_sql(default)?
            .parse_expr()?;
        let value = match crate::assignment::storage_kind(&column.declared) {
            Some(kind) if !column.identity => {
                crate::assignment::convert_for_storage(value, &kind, false, column.utf16)
                    .to_string()
            }
            _ => value.to_string(),
        };
        Ok(format!("CAST({value} AS {})", column.physical))
    }
}
