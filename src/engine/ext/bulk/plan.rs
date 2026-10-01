//! Binding an INSERT BULK statement to its target table, with the
//! diagnostics SQL Server reports for the statement itself.
use crate::engine::{Session, ext};
use anyhow::Result;
use msduck_core::{diagnostic::SqlError, types::Type};
use msduck_sql::dialect::ext::bulk::{self as syntax, InsertBulk, Options};
use sqlparser::ast::{ObjectName, ObjectNamePart, SetExpr, Statement, TableFactor};
use std::collections::HashMap;

/// A target column.
#[derive(Clone, Debug)]
pub(super) struct TargetColumn {
    pub name: String,
    pub nullable: bool,
    pub identity: bool,
    pub computed: bool,
    /// varchar(max), nvarchar(max) or varbinary(max).
    pub max: bool,
    /// A DEFAULT other than an identity generator.
    pub default: bool,
}

/// One column of the INSERT BULK column list, bound to the target.
#[derive(Clone, Debug)]
pub(super) struct Bound {
    /// Index into [`Plan::target_columns`].
    pub target: usize,
    pub declared: Type,
    /// The declaration as written, for the staging table.
    pub declared_text: String,
}

/// A bound INSERT BULK statement, waiting for its BulkLoadBCP message.
#[derive(Clone, Debug)]
pub(super) struct Plan {
    /// The target as the client named it; statements run through the
    /// engine name it this way, so temporary tables and the current
    /// database resolve as for any other statement.
    pub table: String,
    /// The backend schema and table, for catalog lookups.
    pub schema: String,
    pub backend: String,
    pub target_columns: Vec<TargetColumn>,
    pub columns: Vec<Bound>,
    pub options: Options,
}

impl Plan {
    pub fn target(&self, column: &Bound) -> &TargetColumn {
        &self.target_columns[column.target]
    }
    /// The bulk columns name the identity column: its values are kept,
    /// which is what clients' KEEP_IDENTITY option does.
    pub fn keeps_identity(&self) -> bool {
        self.columns
            .iter()
            .any(|column| self.target(column).identity)
    }
}

/// Why an INSERT BULK statement failed.
pub(super) enum Refusal {
    /// SQL Server diagnostics, then a failed DONE.
    Errors(Vec<SqlError>),
    /// A failed DONE without a diagnostic: SQL Server's response to a
    /// column the target does not have.
    Silent,
    /// Anything else (an unsupported shape or an internal failure).
    Other(anyhow::Error),
}

impl From<anyhow::Error> for Refusal {
    fn from(error: anyhow::Error) -> Self {
        match error.downcast::<SqlError>() {
            Ok(error) => Self::Errors(vec![error]),
            Err(error) => Self::Other(error),
        }
    }
}

impl From<duckdb::Error> for Refusal {
    fn from(error: duckdb::Error) -> Self {
        Self::Other(error.into())
    }
}

fn written(name: &ObjectName) -> String {
    name.0
        .iter()
        .map(|part| match part {
            ObjectNamePart::Identifier(ident) => ident.value.clone(),
            part => part.to_string(),
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// The backend schema and table a client's name refers to: the current
/// database qualifier is dropped and temporary tables map to their backend
/// tables, exactly as for a SELECT naming the table.
fn backend(session: &Session, table: &ObjectName) -> Result<(String, String)> {
    let mut statements =
        msduck_sql::batch::parse(&format!("SELECT * FROM {}", syntax::bracket_name(table)))?;
    let mut statement = statements.remove(0);
    session.qualify_databases(&mut statement)?;
    ext::rewrite(session, &mut statement, &HashMap::new())?;
    let Statement::Query(query) = &statement else {
        anyhow::bail!("unsupported INSERT BULK target {table}");
    };
    let SetExpr::Select(select) = query.body.as_ref() else {
        anyhow::bail!("unsupported INSERT BULK target {table}");
    };
    let Some(TableFactor::Table { name, .. }) = select.from.first().map(|from| &from.relation)
    else {
        anyhow::bail!("unsupported INSERT BULK target {table}");
    };
    let parts = name
        .0
        .iter()
        .map(|part| match part {
            ObjectNamePart::Identifier(ident) => Ok(ident.value.clone()),
            _ => Err(anyhow::anyhow!("unsupported INSERT BULK target {table}")),
        })
        .collect::<Result<Vec<_>>>()?;
    match parts.as_slice() {
        [table] => Ok(("dbo".into(), table.clone())),
        [schema, table] => Ok((schema.clone(), table.clone())),
        _ => anyhow::bail!("unsupported INSERT BULK target {table}"),
    }
}

fn target_columns(db: &duckdb::Connection, schema: &str, table: &str) -> Result<Vec<TargetColumn>> {
    let mut columns = db
        .prepare(
            "SELECT column_name, is_nullable = 'YES', column_default FROM information_schema.columns
             WHERE table_catalog = current_database() AND lower(table_schema) = lower(?)
               AND lower(table_name) = lower(?)
             ORDER BY ordinal_position",
        )?
        .query_map([schema, table], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, bool>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<duckdb::Result<Vec<_>>>()?;
    let computed = crate::computed_columns::names(db, schema, table)?;
    // Declared lengths that DuckDB storage does not retain.
    let name = format!(
        "{}.{}",
        sqlparser::ast::Ident::with_quote('"', schema),
        sqlparser::ast::Ident::with_quote('"', table)
    );
    let max = db
        .prepare(
            "SELECT lower(name) FROM sys.columns WHERE object_id = __msduck_object_id(?, NULL)
               AND max_length = -1 AND system_type_id IN (165, 167, 231)",
        )?
        .query_map([&name], |row| row.get::<_, String>(0))?
        .collect::<duckdb::Result<Vec<_>>>()?;
    Ok(columns
        .drain(..)
        .map(|(name, nullable, default)| {
            let key = name.to_lowercase();
            let identity = crate::identity::is_default(default.as_deref());
            let computed = computed.contains(&key);
            TargetColumn {
                nullable,
                identity,
                computed,
                max: max.contains(&key),
                default: default.is_some() && !identity && !computed,
                name,
            }
        })
        .collect())
}

/// Bind `statement` to its target, or report SQL Server's refusal.
pub(super) fn prepare(session: &mut Session, statement: InsertBulk) -> Result<Plan, Refusal> {
    // Declared types first: an unknown type fails the statement (2715).
    let mut declared = Vec::with_capacity(statement.columns.len());
    for (index, column) in statement.columns.iter().enumerate() {
        let text = column.data_type.to_string();
        let resolved = crate::engine::parameter_declarations(&format!("@c {text}"))
            .ok()
            .and_then(|mut declarations| declarations.pop())
            .map(|(_, kind)| kind);
        match resolved {
            Some(kind) => declared.push((kind, text)),
            None => {
                let mut error = SqlError::new(
                    2715,
                    2,
                    format!(
                        "Column, parameter, or variable #{}: Cannot find data type {text}.",
                        index + 1
                    ),
                );
                error.severity = 16;
                return Err(Refusal::Errors(vec![error]));
            }
        }
    }
    let (schema, backend) = backend(session, &statement.table)?;
    let exists = session
        .db
        .prepare(
            "SELECT count(*) FROM information_schema.tables WHERE table_catalog = current_database()
               AND lower(table_schema) = lower(?) AND lower(table_name) = lower(?)",
        )?
        .query_row([&schema, &backend], |row| row.get::<_, i64>(0))?
        > 0;
    if !exists {
        // Captured: SQL Server reports the missing object twice.
        let error = SqlError::new(
            208,
            1,
            format!("Invalid object name '{}'.", written(&statement.table)),
        );
        return Err(Refusal::Errors(vec![error.clone(), error]));
    }
    let target_columns = target_columns(&session.db, &schema, &backend)?;
    let mut columns: Vec<Bound> = Vec::with_capacity(statement.columns.len());
    for (column, (kind, text)) in statement.columns.iter().zip(declared) {
        let Some(target) = target_columns
            .iter()
            .position(|target| target.name.eq_ignore_ascii_case(&column.name.value))
        else {
            return Err(Refusal::Silent);
        };
        if columns.iter().any(|bound| bound.target == target) {
            return Err(Refusal::Errors(vec![SqlError::new(
                264,
                1,
                format!(
                    "The column name '{}' is specified more than once in the SET clause or column list of an INSERT. A column cannot be assigned more than one value in the same clause. Modify the clause to make sure that a column is updated only once. If this statement updates or inserts columns into a view, column aliasing can conceal the duplication in your code.",
                    target_columns[target].name
                ),
            )]));
        }
        if target_columns[target].computed {
            return Err(Refusal::Errors(vec![SqlError::new(
                271,
                1,
                format!(
                    "The column \"{}\" cannot be modified because it is either a computed column or is the result of a UNION operator.",
                    target_columns[target].name
                ),
            )]));
        }
        columns.push(Bound {
            target,
            declared: kind,
            declared_text: text,
        });
    }
    Ok(Plan {
        table: syntax::bracket_name(&statement.table),
        schema,
        backend,
        target_columns,
        columns,
        options: statement.options,
    })
}
