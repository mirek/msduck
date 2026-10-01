//! SQL Server diagnostics for constraint definition and enforcement, with the
//! numbers, states and texts of the pinned reference (docs/gaps-constraints.md).
use super::catalog::{Constraint, Table};
use crate::engine::StatementErrors;
use msduck_core::diagnostic::SqlError;

pub(crate) fn error(number: i32, state: u8, message: impl Into<String>) -> SqlError {
    SqlError::new(number, state, message)
}

fn severity(mut error: SqlError, severity: u8) -> SqlError {
    error.severity = severity;
    error
}

/// Several diagnostics from one statement; the last is the statement's error.
pub(crate) fn several(errors: Vec<SqlError>) -> anyhow::Error {
    StatementErrors(errors).into()
}

/// A definition failure followed by 1750.
pub(crate) fn not_created(first: SqlError) -> anyhow::Error {
    // The reference reports 1750 in state 0 after these and state 1 otherwise.
    let state = u8::from(!matches!(first.number, 1752 | 1769 | 1779 | 1781 | 8111));
    several(vec![
        first,
        error(
            1750,
            state,
            "Could not create constraint or index. See previous errors.",
        ),
    ])
}

/// A definition failure followed by 1750 in a given state.
pub(crate) fn not_created_in_state(first: SqlError, state: u8) -> anyhow::Error {
    several(vec![
        first,
        error(
            1750,
            state,
            "Could not create constraint or index. See previous errors.",
        ),
    ])
}

pub(crate) fn duplicate_object(name: &str) -> anyhow::Error {
    not_created(error(
        2714,
        5,
        format!("There is already an object named '{name}' in the database."),
    ))
}

pub(crate) fn duplicate_in_statement(name: &str) -> anyhow::Error {
    error(8168, 0, format!("Cannot create, drop, enable, or disable more than one constraint, column, index, or trigger named '{name}' in this context. Duplicate names are not allowed.")).into()
}

pub(crate) fn invalid_column(name: &str) -> anyhow::Error {
    error(207, 1, format!("Invalid column name '{name}'.")).into()
}

pub(crate) fn subquery() -> anyhow::Error {
    severity(
        error(
            1046,
            1,
            "Subqueries are not allowed in this context. Only scalar expressions are allowed.",
        ),
        15,
    )
    .into()
}

pub(crate) fn variable(name: &str) -> anyhow::Error {
    severity(
        error(
            137,
            2,
            format!("Must declare the scalar variable \"{name}\"."),
        ),
        15,
    )
    .into()
}

pub(crate) fn column_in_default(name: &str) -> anyhow::Error {
    severity(error(128, 1, format!("The name \"{name}\" is not permitted in this context. Valid expressions are constants, constant expressions, and (in some contexts) variables. Column names are not permitted.")), 15).into()
}

pub(crate) fn not_dropped(first: SqlError) -> anyhow::Error {
    several(vec![
        first,
        error(3727, 0, "Could not drop constraint. See previous errors."),
    ])
}

pub(crate) fn not_a_constraint(name: &str) -> anyhow::Error {
    not_dropped(error(3728, 1, format!("'{name}' is not a constraint.")))
}

pub(crate) fn other_table(name: &str, table: &str) -> anyhow::Error {
    not_dropped(error(
        3733,
        2,
        format!("Constraint '{name}' does not belong to table '{table}'."),
    ))
}

pub(crate) fn referenced_key(key: &str, child: &str, foreign: &str) -> anyhow::Error {
    not_dropped(error(
        3725,
        0,
        format!(
            "The constraint '{key}' is being referenced by table '{child}', foreign key constraint '{foreign}'."
        ),
    ))
}

fn not_toggled(first: SqlError) -> anyhow::Error {
    several(vec![
        first,
        error(
            4916,
            0,
            "Could not enable or disable the constraint. See previous errors.",
        ),
    ])
}

pub(crate) fn toggle_missing(name: &str) -> anyhow::Error {
    not_toggled(error(
        4917,
        0,
        format!("Constraint '{name}' does not exist."),
    ))
}

pub(crate) fn toggle_kind(name: &str) -> anyhow::Error {
    not_toggled(error(
        11415,
        1,
        format!(
            "Object '{name}' cannot be disabled or enabled. This action applies only to foreign key and check constraints."
        ),
    ))
}

pub(crate) fn referenced_table(name: &str) -> anyhow::Error {
    error(
        3726,
        1,
        format!(
            "Could not drop object '{name}' because it is referenced by a FOREIGN KEY constraint."
        ),
    )
    .into()
}

pub(crate) fn truncate_referenced(name: &str) -> anyhow::Error {
    error(
        4712,
        1,
        format!("Cannot truncate table '{name}' because it is being referenced by a FOREIGN KEY constraint."),
    )
    .into()
}

/// 5074 for each dependent object, then 4922.
pub(crate) fn dependent_column(column: &str, objects: &[String], operation: &str) -> anyhow::Error {
    let mut errors = objects
        .iter()
        .map(|object| {
            error(
                5074,
                1,
                format!("The object '{object}' is dependent on column '{column}'."),
            )
        })
        .collect::<Vec<_>>();
    errors.push(error(
        4922,
        9,
        format!("ALTER TABLE {operation} {column} failed because one or more objects access this column."),
    ));
    several(errors)
}

/// The statement verb in 547 diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verb {
    Insert,
    Update,
    Delete,
    Merge,
    AlterTable,
}

impl Verb {
    fn text(self) -> &'static str {
        match self {
            Self::Insert => "INSERT",
            Self::Update => "UPDATE",
            Self::Delete => "DELETE",
            Self::Merge => "MERGE",
            Self::AlterTable => "ALTER TABLE",
        }
    }
    /// The TDS DONE command of a failed DML statement, for 3621.
    pub fn command(self) -> Option<u16> {
        match self {
            Self::Insert => Some(0xc3),
            Self::Update => Some(0xc5),
            Self::Delete => Some(0xc4),
            Self::Merge | Self::AlterTable => None,
        }
    }
}

fn conflict(
    verb: Verb,
    kind: &str,
    name: &str,
    database: &str,
    table: &Table,
    column: Option<&str>,
) -> SqlError {
    let column = column.map_or(String::new(), |column| format!(", column '{column}'"));
    error(
        547,
        0,
        format!(
            "The {} statement conflicted with the {kind} constraint \"{name}\". The conflict occurred in database \"{database}\", table \"{}\"{column}.",
            verb.text(),
            table.display()
        ),
    )
}

pub(crate) fn check_conflict(verb: Verb, constraint: &Constraint, database: &str) -> SqlError {
    conflict(
        verb,
        "CHECK",
        &constraint.name,
        database,
        &constraint.table,
        constraint.column.as_deref(),
    )
}

/// A referencing row without its key: reported against the referenced table.
pub(crate) fn foreign_conflict(verb: Verb, constraint: &Constraint, database: &str) -> SqlError {
    let referenced = constraint.referenced.as_ref().unwrap_or(&constraint.table);
    let column = (constraint.referenced_columns.len() == 1)
        .then(|| constraint.referenced_columns[0].as_str());
    let kind = if constraint.self_referencing() {
        "FOREIGN KEY SAME TABLE"
    } else {
        "FOREIGN KEY"
    };
    conflict(verb, kind, &constraint.name, database, referenced, column)
}

/// A removed key that is still referenced: reported against the referencing
/// table.
pub(crate) fn reference_conflict(verb: Verb, constraint: &Constraint, database: &str) -> SqlError {
    let column = (constraint.columns.len() == 1).then(|| constraint.columns[0].as_str());
    let kind = if constraint.self_referencing() {
        "SAME TABLE REFERENCE"
    } else {
        "REFERENCE"
    };
    conflict(
        verb,
        kind,
        &constraint.name,
        database,
        &constraint.table,
        column,
    )
}

pub(crate) fn duplicate_key(table: &Table, index: &str, values: &str) -> anyhow::Error {
    not_created(error(
        1505,
        1,
        format!(
            "The CREATE UNIQUE INDEX statement terminated because a duplicate key was found for the object name '{}' and the index name '{index}'. The duplicate key value is ({values}).",
            table.display()
        ),
    ))
}

pub(crate) fn nullable_key(table: &str) -> anyhow::Error {
    not_created(error(
        8111,
        1,
        format!("Cannot define PRIMARY KEY constraint on nullable column in table '{table}'."),
    ))
}

pub(crate) fn second_primary_key(table: &str) -> anyhow::Error {
    not_created(error(
        1779,
        0,
        format!("Table '{table}' already has a primary key defined on it."),
    ))
}
