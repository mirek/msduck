//! rowversion and timestamp columns.
//!
//! Each database has one counter, the private sequence
//! `main.__msduck_rowversion`. A rowversion column is a binary(8) column whose
//! default is the counter's next value as eight big-endian bytes; INSERT uses
//! the default (NULL and DEFAULT values included) and every UPDATE of a row
//! assigns it again. A fresh database starts at @@DBTS 0x00000000000007D0
//! (2000) as SQL Server does, and values are never reused, also after
//! rollback.
use super::names::{self, Column, Table};
use super::{NAME, atomically, constraint_error, error};
use crate::engine::{Execution, Parameter, Session, ext};
use anyhow::Result;
use duckdb::{
    core::{DataChunkHandle, Inserter, LogicalTypeId as Id},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use sqlparser::ast::*;
use std::collections::HashMap;

/// The database-wide counter. Its first value is 2001 (0x7D1).
const SEQUENCE: &str = "main.__msduck_rowversion";
const BYTES: &str = "__msduck_rowversion_bytes";
const FIRST: i64 = 2001;

/// `__msduck_rowversion_bytes(BIGINT) -> BLOB`: eight big-endian bytes.
struct Bytes;

impl VScalar for Bytes {
    type State = ();

    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let source = input.flat_vector(0);
        let values = unsafe { source.as_slice_with_len::<i64>(len) };
        let mut out = output.flat_vector();
        for (row, value) in values.iter().enumerate() {
            if source.row_is_null(row as u64) {
                out.set_null(row);
            } else {
                out.insert(row, value.to_be_bytes().as_slice());
            }
        }
        Ok(())
    }

    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![Id::Bigint.into()],
            Id::Blob.into(),
        )]
    }
}

pub(super) fn register(db: &duckdb::Connection) -> Result<()> {
    db.register_scalar_function::<Bytes>(BYTES)?;
    Ok(())
}

pub(super) fn bootstrap(db: &duckdb::Connection) -> Result<()> {
    db.execute_batch(&format!(
        "CREATE SEQUENCE IF NOT EXISTS {SEQUENCE} START WITH {FIRST} MINVALUE 1 NO CYCLE"
    ))?;
    Ok(())
}

/// The column default, which also generates UPDATE values.
fn generate() -> Expr {
    Expr::Function(Function {
        name: ObjectName::from(vec![Ident::new(BYTES)]),
        uses_odbc_syntax: false,
        parameters: FunctionArguments::None,
        args: FunctionArguments::List(FunctionArgumentList {
            duplicate_treatment: None,
            args: vec![FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Function(
                Function {
                    name: ObjectName::from(vec![Ident::new("nextval")]),
                    uses_odbc_syntax: false,
                    parameters: FunctionArguments::None,
                    args: FunctionArguments::List(FunctionArgumentList {
                        duplicate_treatment: None,
                        args: vec![FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(
                            Value::SingleQuotedString(SEQUENCE.into()).into(),
                        )))],
                        clauses: vec![],
                    }),
                    filter: None,
                    null_treatment: None,
                    over: None,
                    within_group: vec![],
                },
            )))],
            clauses: vec![],
        }),
        filter: None,
        null_treatment: None,
        over: None,
        within_group: vec![],
    })
}

/// `rowversion`, or `timestamp`, which in T-SQL is its synonym.
pub(super) fn is_type(data_type: &DataType) -> bool {
    match data_type {
        DataType::Custom(name, modifiers) => {
            modifiers.is_empty()
                && matches!(name.0.as_slice(), [part] if part.as_ident().is_some_and(|id| id.value.eq_ignore_ascii_case("rowversion")))
        }
        DataType::Timestamp(None, TimezoneInfo::None) => true,
        _ => false,
    }
}

/// Whether a physical column is a rowversion column (by its default).
pub(super) fn is_column(column: &Column) -> bool {
    column
        .default
        .as_deref()
        .is_some_and(|default| default.contains(BYTES) && default.contains("__msduck_rowversion'"))
}

fn duplicate(table: &str, column: &str) -> anyhow::Error {
    error(
        2738,
        2,
        format!(
            "A table can only have one timestamp column. Because table '{table}' already has one, the column '{column}' cannot be added."
        ),
    )
}

/// Turn a rowversion column definition into its binary(8) storage with the
/// generating default. SQL Server's rowversion columns are NOT NULL unless
/// declared NULL; `implicit_not_null` is false for ALTER TABLE ADD, whose
/// existing rows are filled after the column exists.
fn plan(table: &str, column: &mut ColumnDef, implicit_not_null: bool) -> Result<()> {
    if column
        .options
        .iter()
        .any(|option| matches!(option.option, ColumnOption::Default(_)))
    {
        return Err(constraint_error(
            1755,
            format!(
                "Defaults cannot be created on columns of data type timestamp. Table '{table}', column '{}'.",
                column.name.value
            ),
        ));
    }
    column.data_type = DataType::Binary(Some(8));
    if implicit_not_null
        && !column
            .options
            .iter()
            .any(|option| matches!(option.option, ColumnOption::Null | ColumnOption::NotNull))
    {
        column.options.push(ColumnOptionDef {
            name: None,
            option: ColumnOption::NotNull,
        });
    }
    column.options.push(ColumnOptionDef {
        name: None,
        option: ColumnOption::Default(generate()),
    });
    Ok(())
}

/// Plan the rowversion columns of CREATE TABLE; returns their names.
pub(super) fn plan_create(table: &mut CreateTable) -> Result<Vec<Ident>> {
    let name = table
        .name
        .0
        .last()
        .and_then(|part| part.as_ident())
        .map(|id| id.value.clone())
        .unwrap_or_default();
    let mut planned = vec![];
    for column in &mut table.columns {
        if !is_type(&column.data_type) {
            continue;
        }
        if !planned.is_empty() {
            return Err(duplicate(&name, &column.name.value));
        }
        plan(&name, column, true)?;
        planned.push(column.name.clone());
    }
    Ok(planned)
}

/// Record the SQL Server type of created rowversion columns: their user
/// type is `timestamp` (sys.columns and sys.types name it so), while the
/// system type stays binary(8), which is what result metadata describes.
pub(super) fn record(db: &duckdb::Connection, table: &ObjectName, columns: &[Ident]) -> Result<()> {
    for column in columns {
        db.execute(
            "UPDATE main.__msduck_declared_columns d SET user_type_id=t.user_type_id \
             FROM sys.types t WHERE t.name='timestamp' \
               AND d.object_id=__msduck_object_id(?,'U') \
               AND d.column_id=(SELECT max(c.column_id) FROM main.__msduck_columns c \
                 WHERE c.object_id=d.object_id AND lower(c.name)=lower(?))",
            [table.to_string(), column.value.clone()],
        )?;
    }
    Ok(())
}

/// ALTER TABLE ADD of a rowversion column fills existing rows with new
/// values; a table keeps at most one such column.
pub(super) fn alter_table(
    session: &mut Session,
    statement: &mut Statement,
    parameters: &mut HashMap<String, Parameter>,
) -> Result<Option<Execution>> {
    let Statement::AlterTable(alter) = statement else {
        return Ok(None);
    };
    if !alter.operations.iter().any(|operation| {
        matches!(operation, AlterTableOperation::AddColumn { column_def, .. } if is_type(&column_def.data_type))
    }) {
        return Ok(None);
    }
    let Some(target) = names::table(session, &alter.name) else {
        return Ok(None);
    };
    let mut existing = names::columns(&session.db, &target)?.iter().any(is_column);
    let mut added = vec![];
    for operation in &mut alter.operations {
        let AlterTableOperation::AddColumn { column_def, .. } = operation else {
            continue;
        };
        if !is_type(&column_def.data_type) {
            continue;
        }
        if existing {
            return Err(duplicate(&target.name, &column_def.name.value));
        }
        existing = true;
        plan(&target.name, column_def, false)?;
        added.push(column_def.name.clone());
    }
    let name = alter.name.clone();
    let statement = statement.clone();
    atomically(session, |session| {
        let execution = ext::reenter(session, NAME, |session| {
            session.execute(statement, parameters)
        })?;
        // DuckDB leaves existing rows NULL for a volatile default.
        let quote = |name: &str| format!("\"{}\"", name.replace('"', "\"\""));
        for column in &added {
            session.db.execute_batch(&format!(
                "UPDATE {}.{} SET {column} = {} WHERE {column} IS NULL",
                quote(&target.schema),
                quote(&target.name),
                generate(),
                column = quote(&column.value),
            ))?;
        }
        record(&session.db, &name, &added)?;
        Ok(execution)
    })
    .map(Some)
}

fn is_null(value: &Expr) -> bool {
    matches!(value, Expr::Value(v) if matches!(v.value, Value::Null))
}

fn is_default(value: &Expr) -> bool {
    matches!(value, Expr::Identifier(id) if id.quote_style.is_none() && id.value.eq_ignore_ascii_case("DEFAULT"))
}

fn explicit() -> anyhow::Error {
    error(
        273,
        1,
        "Cannot insert an explicit value into a timestamp column. Use INSERT with a column list to exclude the timestamp column, or insert a DEFAULT into the timestamp column.".into(),
    )
}

/// INSERT: NULL and DEFAULT for the rowversion column become DEFAULT; any
/// other value fails with 273. Omitted, it takes its default.
pub(super) fn insert(
    db: &duckdb::Connection,
    table: &Table,
    insert: &mut Insert,
    columns: &[Column],
) -> Result<()> {
    let Some(rowversion) = columns.iter().find(|column| is_column(column)) else {
        return Ok(());
    };
    let Some(source) = insert.source.as_mut() else {
        return Ok(());
    };
    let slot = if insert.columns.is_empty() {
        // Positional values skip identity and computed columns.
        let computed = names::computed(db, table)?;
        let positional = columns
            .iter()
            .filter(|column| {
                !computed.contains(&column.name.to_lowercase())
                    && !crate::identity::is_default(column.default.as_deref())
            })
            .collect::<Vec<_>>();
        // Wildcards and set operations have no width before binding; they
        // are left to the ordinary path.
        let width = match source.body.as_ref() {
            SetExpr::Values(values) => values.rows.first().map(|row| row.len()),
            SetExpr::Select(select)
                if select.projection.iter().all(|item| {
                    matches!(
                        item,
                        SelectItem::UnnamedExpr(_) | SelectItem::ExprWithAlias { .. }
                    )
                }) =>
            {
                Some(select.projection.len())
            }
            _ => None,
        };
        let Some(width) = width else {
            return Ok(());
        };
        if width != positional.len() {
            return Err(error(
                213,
                1,
                "Column name or number of supplied values does not match table definition.".into(),
            ));
        }
        positional
            .iter()
            .position(|column| column.name.eq_ignore_ascii_case(&rowversion.name))
    } else {
        insert.columns.iter().position(|name| {
            name.0
                .last()
                .and_then(|part| part.as_ident())
                .is_some_and(|id| id.value.eq_ignore_ascii_case(&rowversion.name))
        })
    };
    let Some(slot) = slot else {
        return Ok(());
    };
    match source.body.as_mut() {
        SetExpr::Values(values) => {
            for row in &mut values.rows {
                match row.get_mut(slot) {
                    Some(value) if is_null(value) || is_default(value) => {
                        *value = Expr::Identifier(Ident::new("DEFAULT"));
                    }
                    Some(_) => return Err(explicit()),
                    None => {}
                }
            }
            Ok(())
        }
        // INSERT ... SELECT may supply NULL, which also generates a value.
        SetExpr::Select(select)
            if select.projection.iter().all(|item| {
                matches!(
                    item,
                    SelectItem::UnnamedExpr(_) | SelectItem::ExprWithAlias { .. }
                )
            }) =>
        {
            match select.projection.get_mut(slot) {
                Some(
                    SelectItem::UnnamedExpr(value) | SelectItem::ExprWithAlias { expr: value, .. },
                ) if is_null(value) => {
                    *value = generate();
                    Ok(())
                }
                _ => Err(explicit()),
            }
        }
        _ => Err(explicit()),
    }
}

/// The table an UPDATE writes: its target, or the FROM table its alias names.
fn update_target(session: &Session, update: &Update) -> Option<Table> {
    let TableFactor::Table { name, alias, .. } = &update.table.relation else {
        return None;
    };
    if alias.is_none()
        && let [part] = name.0.as_slice()
        && let Some(id) = part.as_ident()
        && let Some(UpdateTableFromKind::AfterSet(from) | UpdateTableFromKind::BeforeSet(from)) =
            &update.from
    {
        for table in from {
            for factor in
                std::iter::once(&table.relation).chain(table.joins.iter().map(|j| &j.relation))
            {
                if let TableFactor::Table {
                    name: source,
                    alias: Some(alias),
                    ..
                } = factor
                    && alias.name.value.eq_ignore_ascii_case(&id.value)
                {
                    return names::table(session, source);
                }
            }
        }
    }
    names::table(session, name)
}

/// UPDATE: every updated row gets a new value; assigning the column fails
/// with 272.
pub(super) fn update(session: &Session, statement: &mut Statement) -> Result<()> {
    let Statement::Update(update) = statement else {
        return Ok(());
    };
    let Some(target) = update_target(session, update) else {
        return Ok(());
    };
    let columns = names::columns(&session.db, &target)?;
    let Some(rowversion) = columns.iter().find(|column| is_column(column)) else {
        return Ok(());
    };
    let assigns = |name: &ObjectName| {
        name.0
            .last()
            .and_then(|part| part.as_ident())
            .is_some_and(|id| id.value.eq_ignore_ascii_case(&rowversion.name))
    };
    if update
        .assignments
        .iter()
        .any(|assignment| match &assignment.target {
            AssignmentTarget::ColumnName(name) => assigns(name),
            AssignmentTarget::Tuple(names) => names.iter().any(assigns),
        })
    {
        return Err(error(272, 1, "Cannot update a timestamp column.".into()));
    }
    update.assignments.push(Assignment {
        target: AssignmentTarget::ColumnName(ObjectName::from(vec![Ident::with_quote(
            '"',
            &rowversion.name,
        )])),
        value: generate(),
    });
    Ok(())
}

/// The counter's last value: the seed before first use.
fn last(db: &duckdb::Connection) -> Result<i64> {
    Ok(db.query_row(
        "SELECT coalesce(last_value, start_value - 1) FROM duckdb_sequences() \
         WHERE database_name=current_database() AND schema_name='main' AND sequence_name='__msduck_rowversion'",
        [],
        |row| row.get(0),
    )?)
}

fn varbinary(value: i64) -> Expr {
    Expr::Cast {
        kind: CastKind::Cast,
        expr: Box::new(Expr::Value(
            Value::HexStringLiteral(format!("{value:016X}")).into(),
        )),
        data_type: DataType::Varbinary(Some(BinaryLength::IntegerLength { length: 8 })),
        format: None,
    }
}

/// `@@DBTS` (the last value) and `MIN_ACTIVE_ROWVERSION()` (the next one;
/// no transaction holds an unpublished value here).
pub(super) fn rewrite(session: &Session, expr: &mut Expr) -> Result<()> {
    match expr {
        Expr::Identifier(id) if id.value.eq_ignore_ascii_case("@@DBTS") => {
            *expr = varbinary(last(&session.db)?);
        }
        Expr::Function(function)
            if function
                .name
                .to_string()
                .eq_ignore_ascii_case("MIN_ACTIVE_ROWVERSION")
                && matches!(&function.args, FunctionArguments::List(list) if list.args.is_empty()) =>
        {
            *expr = varbinary(last(&session.db)? + 1);
        }
        _ => {}
    }
    Ok(())
}
