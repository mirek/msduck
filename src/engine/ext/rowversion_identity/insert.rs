//! SET IDENTITY_INSERT, explicit identity values and INSERT bookkeeping.
//!
//! The SET operation and the INSERT gate are the deterministic rules of
//! crates/msduck-sql/src/identity_insert*.rs with their catalog adapters
//! (src/identity_insert_*.rs). An INSERT the gate permits to write the
//! identity column runs with `crate::insert::with_explicit_identity`; each
//! explicit value passes through `__msduck_identity_note`, which records it
//! for the session, and afterwards the table's allocator advances past the
//! extreme value (`__msduck_identity_advance`, never rolled back).
use super::{execute, names, rowversion, scope, write};
use crate::engine::{Execution, Parameter, Session};
use anyhow::{Result, anyhow};
use duckdb::{
    core::{DataChunkHandle, Inserter, LogicalTypeId as Id},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use msduck_core::diagnostic::SqlError;
use sqlparser::ast::*;
use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex},
};

const NOTE: &str = "__msduck_identity_note";

/// Explicit identity values seen per session token during one INSERT.
#[derive(Clone, Copy, Debug, Default)]
struct Seen {
    last: Option<i128>,
    min: Option<i128>,
    max: Option<i128>,
}

impl Seen {
    fn add(&mut self, value: i128) {
        self.last = Some(value);
        self.min = Some(self.min.map_or(value, |min| min.min(value)));
        self.max = Some(self.max.map_or(value, |max| max.max(value)));
    }
}

static SEEN: LazyLock<Mutex<HashMap<u64, Seen>>> = LazyLock::new(Default::default);

fn seen() -> std::sync::MutexGuard<'static, HashMap<u64, Seen>> {
    SEEN.lock().unwrap_or_else(|e| e.into_inner())
}

/// Drop a finished session's record.
pub(super) fn forget(token: u64) {
    seen().remove(&token);
}

/// `__msduck_identity_note(token, value) -> value`: records `value` for the
/// session `token` and returns it unchanged. Overloads keep integer, float
/// and text inputs in their own type, so assignment conversion to the
/// identity column is the ordinary one.
struct Note;

impl VScalar for Note {
    type State = ();

    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let tokens = input.flat_vector(0);
        let source = input.flat_vector(1);
        let mut out = output.flat_vector();
        let mut values = Vec::with_capacity(len);
        match source.logical_type().id() {
            Id::Bigint => {
                let input = unsafe { source.as_slice_with_len::<i64>(len) };
                let output = unsafe { out.as_mut_slice_with_len::<i64>(len) };
                output.copy_from_slice(input);
                for (row, value) in input.iter().enumerate() {
                    values.push((!source.row_is_null(row as u64)).then_some(i128::from(*value)));
                }
            }
            Id::Hugeint => {
                // hugeint_t is {lower: u64, upper: i64}.
                let input = unsafe { source.as_slice_with_len::<[u64; 2]>(len) };
                let output = unsafe { out.as_mut_slice_with_len::<[u64; 2]>(len) };
                output.copy_from_slice(input);
                for (row, [lower, upper]) in input.iter().enumerate() {
                    let value = (i128::from(*upper as i64) << 64) | i128::from(*lower);
                    values.push((!source.row_is_null(row as u64)).then_some(value));
                }
            }
            Id::Double => {
                let input = unsafe { source.as_slice_with_len::<f64>(len) };
                let output = unsafe { out.as_mut_slice_with_len::<f64>(len) };
                output.copy_from_slice(input);
                for (row, value) in input.iter().enumerate() {
                    values.push(
                        (!source.row_is_null(row as u64) && value.is_finite())
                            .then_some(value.trunc() as i128),
                    );
                }
            }
            _ => {
                for row in 0..len {
                    if source.row_is_null(row as u64) {
                        values.push(None);
                        continue;
                    }
                    let mut raw = unsafe {
                        source.as_slice_with_len::<duckdb::ffi::duckdb_string_t>(len)[row]
                    };
                    let bytes = unsafe {
                        std::slice::from_raw_parts(
                            duckdb::ffi::duckdb_string_t_data(&mut raw).cast::<u8>(),
                            duckdb::ffi::duckdb_string_t_length(raw) as usize,
                        )
                    };
                    out.insert(row, bytes);
                    let text = String::from_utf8_lossy(bytes);
                    let text = text.trim();
                    values.push(text.parse::<i128>().ok().or_else(|| {
                        text.parse::<f64>()
                            .ok()
                            .filter(|v| v.is_finite())
                            .map(|v| v.trunc() as i128)
                    }));
                }
            }
        }
        for (row, value) in values.iter().enumerate() {
            if value.is_none() && source.row_is_null(row as u64) {
                out.set_null(row);
            }
        }
        if len > 0 && !tokens.row_is_null(0) {
            let token = unsafe { tokens.as_slice_with_len::<i64>(len)[0] } as u64;
            let mut seen = seen();
            let entry = seen.entry(token).or_default();
            for value in values.into_iter().flatten() {
                entry.add(value);
            }
        }
        Ok(())
    }

    fn signatures() -> Vec<ScalarFunctionSignature> {
        [Id::Bigint, Id::Hugeint, Id::Double, Id::Varchar]
            .into_iter()
            .map(|kind| {
                ScalarFunctionSignature::exact(vec![Id::Bigint.into(), kind.into()], kind.into())
            })
            .collect()
    }

    fn volatile() -> bool {
        true
    }
}

pub(super) fn register(db: &duckdb::Connection) -> Result<()> {
    db.register_scalar_function::<Note>(NOTE)?;
    Ok(())
}

fn note(token: u64, value: Expr) -> Expr {
    let argument = |expr| FunctionArg::Unnamed(FunctionArgExpr::Expr(expr));
    Expr::Function(Function {
        name: ObjectName::from(vec![Ident::new(NOTE)]),
        uses_odbc_syntax: false,
        parameters: FunctionArguments::None,
        args: FunctionArguments::List(FunctionArgumentList {
            duplicate_treatment: None,
            args: vec![
                argument(Expr::Value(Value::Number(token.to_string(), false).into())),
                argument(value),
            ],
            clauses: vec![],
        }),
        filter: None,
        null_treatment: None,
        over: None,
        within_group: vec![],
    })
}

/// A typed gate or SET diagnostic, completed with SQL Server's DONE command.
fn diagnostic(error: SqlError, command: u16) -> anyhow::Error {
    crate::query_error::attach_context(error.into(), vec![], command)
}

/// SET IDENTITY_INSERT [schema.]table ON|OFF.
pub(super) fn set(session: &mut Session, statement: &Statement) -> Result<Option<Execution>> {
    if !matches!(
        statement,
        Statement::Set(Set::SetSessionParam(SetSessionParamKind::IdentityInsert(_)))
    ) {
        return Ok(None);
    }
    let database = session.database();
    let (id, name) = (i64::from(database.database_id), database.name.clone());
    let state = session.ext.rowversion_identity.insert_mut();
    match write::session::apply(&session.db, id, &name, state, statement) {
        Ok(Some(applied)) => Ok(Some(Execution::statement(
            vec![],
            None,
            applied.done_command,
        ))),
        Ok(None) => Ok(None),
        Err(write::session::ApplyError::Diagnostic {
            error,
            done_command,
        }) => Err(diagnostic(error, done_command)),
        Err(write::session::ApplyError::Backend(error)) => Err(error.into()),
        Err(error) => Err(anyhow!("unsupported SET IDENTITY_INSERT: {error}")),
    }
}

/// How an INSERT treats the identity column.
enum Mode {
    /// The target has no identity column.
    Plain,
    /// Generated values from `sequence`.
    Generated { sequence: String },
    /// Explicit values (IDENTITY_INSERT ON) in source column `column`.
    Explicit { sequence: String, column: usize },
}

fn is_on(session: &Session, table: &names::Table) -> Result<bool> {
    let Some(active) = session.ext.rowversion_identity.insert().active() else {
        return Ok(false);
    };
    if i64::from(session.database().database_id) != active.database_id {
        return Ok(false);
    }
    let object_id: Option<i32> = session.db.query_row(
        "SELECT __msduck_object_id(?,'U')",
        [ObjectName::from(vec![
            Ident::with_quote('[', &table.schema),
            Ident::with_quote('[', &table.name),
        ])
        .to_string()],
        |row| row.get(0),
    )?;
    Ok(object_id == Some(active.table.object_id))
}

/// Apply the IDENTITY_INSERT gate when it can decide something: the session
/// has a table ON, the INSERT lists the identity column, or it supplies
/// positional values.
fn mode(
    session: &Session,
    statement: &Statement,
    table: &names::Table,
    identity: &names::Column,
    sequence: String,
) -> Result<Mode> {
    let Statement::Insert(insert) = statement else {
        return Ok(Mode::Generated { sequence });
    };
    let lists = insert.columns.iter().any(|name| {
        name.0
            .last()
            .and_then(|part| part.as_ident())
            .is_some_and(|id| id.value.eq_ignore_ascii_case(&identity.name))
    });
    let on = is_on(session, table)?;
    let positional = insert.columns.is_empty() && insert.source.is_some();
    if !(on || lists || positional) {
        return Ok(Mode::Generated { sequence });
    }
    // An explicit identity value may be neither NULL nor DEFAULT.
    if on
        && let Some(position) = insert.columns.iter().position(|name| {
            name.0
                .last()
                .and_then(|part| part.as_ident())
                .is_some_and(|id| id.value.eq_ignore_ascii_case(&identity.name))
        })
        && let Some(SetExpr::Values(values)) = insert.source.as_ref().map(|query| query.body.as_ref())
        && values.rows.iter().any(|row| {
            row.get(position).is_some_and(|value| {
                matches!(value, Expr::Value(v) if matches!(v.value, Value::Null))
                    || matches!(value, Expr::Identifier(id) if id.quote_style.is_none() && id.value.eq_ignore_ascii_case("DEFAULT"))
            })
        })
    {
        return Err(diagnostic(
            SqlError::new(
                339,
                1,
                "DEFAULT or NULL are not allowed as explicit identity values.",
            ),
            253,
        ));
    }
    let database = session.database();
    match write::preflight(
        &session.db,
        i64::from(database.database_id),
        &database.name,
        session.ext.rowversion_identity.insert(),
        statement,
    ) {
        Ok(Some(write::Permit::Explicit { source_column })) => Ok(Mode::Explicit {
            sequence,
            column: source_column,
        }),
        Ok(_) => Ok(Mode::Generated { sequence }),
        Err(write::PreflightError::Diagnostic {
            error,
            done_command,
        }) => Err(diagnostic(error, done_command)),
        // Shapes whose SQL Server precedence is unprobed keep the existing
        // path while the setting is OFF, and fail explicitly while ON.
        Err(write::PreflightError::Unsupported(_)) if !on => Ok(Mode::Generated { sequence }),
        Err(write::PreflightError::Backend(error)) => Err(error.into()),
        Err(error) => Err(anyhow!("unsupported INSERT under IDENTITY_INSERT: {error}")),
    }
}

/// Wrap the explicit identity source values in [`NOTE`]. Returns false for
/// source shapes without one expression per row in that position.
fn record_values(insert: &mut Insert, column: usize, token: u64) -> bool {
    let Some(source) = insert.source.as_mut() else {
        return false;
    };
    match source.body.as_mut() {
        SetExpr::Values(values) => {
            for row in &mut values.rows {
                let Some(value) = row.get_mut(column) else {
                    return false;
                };
                *value = note(token, value.clone());
            }
            true
        }
        SetExpr::Select(select) => {
            let simple = select.projection.iter().all(|item| {
                matches!(
                    item,
                    SelectItem::UnnamedExpr(_) | SelectItem::ExprWithAlias { .. }
                )
            });
            match select.projection.get_mut(column) {
                Some(
                    SelectItem::UnnamedExpr(value) | SelectItem::ExprWithAlias { expr: value, .. },
                ) if simple => {
                    *value = note(token, value.clone());
                    true
                }
                _ => false,
            }
        }
        _ => false,
    }
}

/// SQL Server's message for an exhausted identity column.
fn overflow(
    session: &Session,
    table: &names::Table,
    column: &str,
    error: anyhow::Error,
) -> anyhow::Error {
    let text = error.to_string();
    if !(text.contains("reached maximum value of sequence")
        || text.contains("reached minimum value of sequence"))
        || !text.contains("__msduck_identity_")
    {
        return error;
    }
    let type_name: Option<String> = session
        .db
        .query_row(
            "SELECT t.name FROM sys.columns c JOIN sys.types t ON t.user_type_id=c.user_type_id \
             WHERE c.object_id=__msduck_object_id(?,'U') AND lower(c.name)=lower(?)",
            [
                ObjectName::from(vec![
                    Ident::with_quote('[', &table.schema),
                    Ident::with_quote('[', &table.name),
                ])
                .to_string(),
                column.to_owned(),
            ],
            |row| row.get(0),
        )
        .ok();
    let Some(type_name) = type_name else {
        return error;
    };
    diagnostic(
        SqlError::new(
            8115,
            1,
            format!("Arithmetic overflow error converting IDENTITY to data type {type_name}."),
        ),
        0xc3,
    )
}

/// The identity column's extreme stored values, for explicit values whose
/// source shape cannot be recorded row by row.
fn extremes(session: &Session, table: &names::Table, column: &str) -> Result<Seen> {
    let quote = |name: &str| format!("\"{}\"", name.replace('"', "\"\""));
    let (min, max): (Option<String>, Option<String>) = session.db.query_row(
        &format!(
            "SELECT CAST(min({column}) AS VARCHAR), CAST(max({column}) AS VARCHAR) FROM {}.{}",
            quote(&table.schema),
            quote(&table.name),
            column = quote(column)
        ),
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let parse = |value: Option<String>| value.and_then(|v| v.parse::<i128>().ok());
    Ok(Seen {
        last: None,
        min: parse(min),
        max: parse(max),
    })
}

/// Every INSERT into a table of the current database: rowversion values,
/// the IDENTITY_INSERT gate, and the session's identity values.
pub(super) fn run(
    session: &mut Session,
    statement: &mut Statement,
    parameters: &mut HashMap<String, Parameter>,
) -> Result<Option<Execution>> {
    let Statement::Insert(insert) = statement else {
        return Ok(None);
    };
    let TableObject::TableName(name) = &insert.table else {
        return Ok(None);
    };
    let Some(table) = names::table(session, name) else {
        return Ok(None);
    };
    let columns = names::columns(&session.db, &table)?;
    if columns.is_empty() {
        return Ok(None);
    }
    let identity = columns
        .iter()
        .find(|column| crate::identity::is_default(column.default.as_deref()))
        .cloned();
    let mode = match &identity {
        None => Mode::Plain,
        Some(identity) => {
            let sequence = crate::identity::sequence_name(identity.default.as_deref())
                .expect("recognized identity default");
            mode(session, statement, &table, identity, sequence)?
        }
    };
    if let Statement::Insert(insert) = statement {
        rowversion::insert(&session.db, &table, insert, &columns)?;
    }
    let token = session.ext.token;
    match mode {
        Mode::Plain => {
            let execution = execute(session, statement.clone(), parameters)?;
            session.ext.rowversion_identity.record(None);
            Ok(Some(execution))
        }
        Mode::Generated { sequence } => {
            let identity = identity.expect("identity column");
            let _lock = scope::lock(&sequence, token);
            let before = scope::last_value(&session.db, &sequence)?;
            let execution = execute(session, statement.clone(), parameters)
                .map_err(|error| overflow(session, &table, &identity.name, error))?;
            let after = scope::last_value(&session.db, &sequence)?;
            if after != before {
                session.ext.rowversion_identity.record(after);
            }
            Ok(Some(execution))
        }
        Mode::Explicit { sequence, column } => {
            let identity = identity.expect("identity column");
            let mut statement = statement.clone();
            let Statement::Insert(insert) = &mut statement else {
                unreachable!()
            };
            let recorded = record_values(insert, column, token);
            seen().remove(&token);
            let result = crate::insert::with_explicit_identity(&table.schema, &table.name, || {
                execute(session, statement, parameters)
            });
            let noted = seen().remove(&token).unwrap_or_default();
            let execution = result?;
            if execution.count == Some(0) {
                return Ok(Some(execution));
            }
            let values = if recorded {
                noted
            } else {
                extremes(session, &table, &identity.name)?
            };
            // Advance past the extreme value. A value outside the allocator's
            // range (possible only for wide decimal columns) leaves it
            // unchanged rather than failing the caller's transaction.
            let (min, max): (i64, i64) = session.db.query_row(
                "SELECT min_value, max_value FROM duckdb_sequences() \
                 WHERE database_name=current_database() AND schema_name||'.'||sequence_name=?",
                [&sequence],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            for value in [values.min, values.max].into_iter().flatten() {
                if let Ok(value) = i64::try_from(value)
                    && (min..=max).contains(&value)
                {
                    session.db.query_row(
                        "SELECT __msduck_identity_advance(?, ?)",
                        duckdb::params![sequence, value],
                        |row| row.get::<_, bool>(0),
                    )?;
                }
            }
            let last = values.last.or(values.max);
            if last.is_some() {
                session.ext.rowversion_identity.record(last);
            }
            Ok(Some(execution))
        }
    }
}
