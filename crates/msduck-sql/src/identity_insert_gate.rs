//! Pure INSERT permission preflight for an identity table.
//!
//! The root adapter resolves the table and columns before calling this module.
//! It must not execute a source expression before this preflight succeeds.

use msduck_core::diagnostic::SqlError;
use sqlparser::ast::{Expr, ObjectName, SetExpr, Statement, TableObject};

pub struct ResolvedTarget<'a, K> {
    /// Stable catalog identity, shared by alternate names of the same table.
    pub key: &'a K,
    pub schema: &'a str,
    pub table: &'a str,
    pub column_count: usize,
    pub identity_column: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Permit {
    NotApplicable,
    Generated,
    Explicit { source_column: usize },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GateError {
    Diagnostic { error: SqlError, done_command: u16 },
    Unsupported(&'static str),
}

impl GateError {
    fn diagnostic(number: i32, command: u16, message: String) -> Self {
        Self::Diagnostic {
            error: SqlError::new(number, 1, message),
            done_command: command,
        }
    }
}

fn is_default(value: &Expr) -> bool {
    matches!(value, Expr::Identifier(id) if id.quote_style.is_none() && id.value.eq_ignore_ascii_case("DEFAULT"))
}

/// Classify one INSERT from AST shape and declared catalog/session inputs.
/// Unknown target or source shapes are returned to the caller as unsupported,
/// since SQL Server's diagnostic precedence has not been captured for them.
pub fn preflight<K: Eq>(
    statement: &Statement,
    target: &ResolvedTarget<'_, K>,
    active: Option<&K>,
    resolve_column: impl Fn(&ObjectName) -> Option<usize>,
) -> Result<Permit, GateError> {
    let Statement::Insert(insert) = statement else {
        return Ok(Permit::NotApplicable);
    };
    if !matches!(insert.table, TableObject::TableName(_)) {
        return Err(GateError::Unsupported("unsupported INSERT target shape"));
    }
    let Some(identity) = target.identity_column else {
        return Ok(Permit::NotApplicable);
    };
    if identity >= target.column_count {
        return Err(GateError::Unsupported("invalid identity catalog position"));
    }
    let source = insert.source.as_ref().map(|query| query.body.as_ref());
    if source.is_none() && !insert.columns.is_empty() {
        return Err(GateError::Unsupported(
            "column list with DEFAULT VALUES is unprobed",
        ));
    }
    match source {
        None | Some(SetExpr::Values(_) | SetExpr::Select(_)) => {}
        _ => return Err(GateError::Unsupported("unsupported INSERT source shape")),
    }
    let is_on = active == Some(target.key);
    if let Some(SetExpr::Values(values)) = source {
        let width = if insert.columns.is_empty() {
            target.column_count
        } else {
            insert.columns.len()
        };
        if values.rows.is_empty() || values.rows.iter().any(|row| row.len() != width) {
            return Err(GateError::Unsupported(
                "unknown INSERT source arity precedence",
            ));
        }
    }
    if insert.columns.is_empty() {
        if source.is_none() {
            if !is_on {
                return Ok(Permit::Generated);
            }
            return Err(GateError::diagnostic(
                545,
                195,
                format!(
                    "Explicit value must be specified for identity column in table '{}' either when IDENTITY_INSERT is set to ON or when a replication user is inserting into a NOT FOR REPLICATION identity column.",
                    target.table
                ),
            ));
        }
        let Some(SetExpr::Values(_)) = source else {
            return Err(GateError::Unsupported(
                "unlisted INSERT SELECT precedence is unprobed",
            ));
        };
        // SQL Server reports 8101 for full-width positional VALUES, even
        // when the identity slot is DEFAULT or IDENTITY_INSERT is OFF.
        return Err(GateError::diagnostic(
            8101,
            253,
            format!(
                "An explicit value for the identity column in table '{}.{}' can only be specified when a column list is used and IDENTITY_INSERT is ON.",
                target.schema, target.table
            ),
        ));
    }
    let positions = insert
        .columns
        .iter()
        .map(&resolve_column)
        .collect::<Option<Vec<_>>>()
        .ok_or(GateError::Unsupported("unresolved INSERT column"))?;
    if positions
        .iter()
        .any(|position| *position >= target.column_count)
    {
        return Err(GateError::Unsupported("invalid INSERT catalog position"));
    }
    if positions
        .iter()
        .enumerate()
        .any(|(i, position)| positions[..i].contains(position))
    {
        return Err(GateError::Unsupported("duplicate INSERT target column"));
    }
    let identity_positions = positions
        .iter()
        .enumerate()
        .filter_map(|(source, column)| (*column == identity).then_some(source))
        .collect::<Vec<_>>();
    let source_column = match identity_positions.as_slice() {
        [] => None,
        [source] => Some(*source),
        _ => return Err(GateError::Unsupported("duplicate identity target column")),
    };
    if let (Some(source_column), Some(SetExpr::Values(values))) = (source_column, source)
        && values
            .rows
            .iter()
            .any(|row| is_default(&row[source_column]))
    {
        return Err(GateError::Unsupported(
            "explicit identity DEFAULT is unprobed",
        ));
    }
    match (is_on, source_column) {
        (true, Some(source_column)) => Ok(Permit::Explicit { source_column }),
        (true, None) => Err(GateError::diagnostic(
            545,
            195,
            format!(
                "Explicit value must be specified for identity column in table '{}' either when IDENTITY_INSERT is set to ON or when a replication user is inserting into a NOT FOR REPLICATION identity column.",
                target.table
            ),
        )),
        (false, Some(_)) => Err(GateError::diagnostic(
            544,
            195,
            format!(
                "Cannot insert explicit value for identity column in table '{}' when IDENTITY_INSERT is set to OFF.",
                target.table
            ),
        )),
        (false, None) => Ok(Permit::Generated),
    }
}
