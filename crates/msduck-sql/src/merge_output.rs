//! Deterministic MERGE OUTPUT projection over selected, already evaluated actions.
//!
//! This file is path-imported until the SQL crate's shared export is available.
//! The root adapter must first validate/select MERGE actions, acquire pre-write
//! target images, convert INSERT/UPDATE values for storage, and evaluate each
//! nontrivial OUTPUT expression once. This module does no SQL evaluation or I/O.
//! The eventual `msduck-sql` export should map
//! `msduck_core::merge_action::ActionKind` into this module's `ActionKind` after
//! action selection. The root adapter still owns statement atomicity, sink
//! conversion/write, session row counts and DONE tokens; TDS encodes direct
//! descriptors separately. `OUTPUT INTO` has no direct result set, and a later
//! SELECT from its sink must use that table's own declarations.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActionKind {
    Insert,
    Update,
    Delete,
}

impl ActionKind {
    pub const fn sql_name(self) -> &'static str {
        match self {
            Self::Insert => "INSERT",
            Self::Update => "UPDATE",
            Self::Delete => "DELETE",
        }
    }
}

/// Values are already converted to their logical SQL types. `Unavailable` is
/// distinct from a NULL field in a present image, though both emit SQL NULL.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Cell<V> {
    Value(V),
    Null,
    Unavailable,
    Action(&'static str),
}

impl<V> Cell<V> {
    pub const fn is_sql_null(&self) -> bool {
        matches!(self, Self::Null | Self::Unavailable)
    }
}

/// `inserted` is the post-storage-conversion image. `deleted` is the original
/// target image. An absent image differs from a present row of NULL fields.
pub struct SelectedAction<V> {
    pub kind: ActionKind,
    pub deleted: Option<Vec<Option<V>>>,
    pub inserted: Option<Vec<Option<V>>>,
    pub source: Option<Vec<Option<V>>>,
    /// Results of complex OUTPUT expressions, in caller-specified slot order.
    pub evaluated: Vec<Option<V>>,
}

/// A logical field declaration is supplied by the binder. It must include the
/// type, bounded width, nullability and collation where applicable. The generic
/// payload deliberately carries no backend or TDS dependency.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Descriptor<D, C> {
    /// SQL Server declares `$action` as NVARCHAR(10), even for zero rows.
    Action {
        max_utf16_units: u16,
        collation: C,
    },
    Field(D),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Expression {
    Action,
    Inserted(usize),
    Deleted(usize),
    Source(usize),
    Evaluated(usize),
}

pub struct OutputColumn<D, C> {
    pub name: String,
    pub expression: Expression,
    /// Action columns must carry an Action descriptor; all others a Field.
    pub descriptor: Descriptor<D, C>,
}

pub struct SinkColumn<D> {
    pub name: String,
    pub declaration: D,
}

pub enum Destination<D> {
    Direct,
    /// Ordered destination columns. The root adapter performs assignment
    /// conversion and writes in the same atomic statement as the MERGE.
    Into(Vec<SinkColumn<D>>),
}

pub struct Projection<D, C> {
    pub columns: Vec<OutputColumn<D, C>>,
    pub destination: Destination<D>,
}

pub struct DirectOutput<D, C, V> {
    pub columns: Vec<(String, Descriptor<D, C>)>,
    pub rows: Vec<Vec<Cell<V>>>,
}

pub struct SinkAssignment<V> {
    /// Ordered sink column names and unconverted projected values.
    pub values: Vec<(String, Cell<V>)>,
}

pub struct IntoOutput<D, V> {
    pub columns: Vec<SinkColumn<D>>,
    pub assignments: Vec<SinkAssignment<V>>,
}

/// An INTO operation has no direct result set. Reading the sink later requires
/// a fresh catalog/result descriptor and is outside this projection contract.
pub enum Projected<D, C, V> {
    Direct(DirectOutput<D, C, V>),
    Into(IntoOutput<D, V>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProjectionError {
    WrongImage { row: usize, action: ActionKind },
    WrongDescriptor { column: usize },
    WrongActionWidth { column: usize },
    MissingField { row: usize, column: usize },
    MissingEvaluated { row: usize, slot: usize },
    SinkArity { projected: usize, sink: usize },
}

fn field<V: Clone>(image: Option<&[Option<V>]>, index: usize) -> Option<Cell<V>> {
    match image {
        None => Some(Cell::Unavailable),
        Some(values) => values.get(index).map(|value| match value {
            Some(value) => Cell::Value(value.clone()),
            None => Cell::Null,
        }),
    }
}

pub fn project<D: Clone, C: Clone, V: Clone>(
    projection: Projection<D, C>,
    actions: &[SelectedAction<V>],
) -> Result<Projected<D, C, V>, ProjectionError> {
    for (index, column) in projection.columns.iter().enumerate() {
        if matches!(column.expression, Expression::Action)
            != matches!(column.descriptor, Descriptor::Action { .. })
        {
            return Err(ProjectionError::WrongDescriptor { column: index });
        }
        if let Descriptor::Action {
            max_utf16_units, ..
        } = column.descriptor
            && max_utf16_units != 10
        {
            return Err(ProjectionError::WrongActionWidth { column: index });
        }
    }
    if let Destination::Into(columns) = &projection.destination
        && columns.len() != projection.columns.len()
    {
        return Err(ProjectionError::SinkArity {
            projected: projection.columns.len(),
            sink: columns.len(),
        });
    }

    let mut rows = Vec::with_capacity(actions.len());
    for (row_index, action) in actions.iter().enumerate() {
        let valid_images = match action.kind {
            ActionKind::Insert => action.deleted.is_none() && action.inserted.is_some(),
            ActionKind::Update => action.deleted.is_some() && action.inserted.is_some(),
            ActionKind::Delete => action.deleted.is_some() && action.inserted.is_none(),
        };
        if !valid_images {
            return Err(ProjectionError::WrongImage {
                row: row_index,
                action: action.kind,
            });
        }
        let mut row = Vec::with_capacity(projection.columns.len());
        for (column_index, column) in projection.columns.iter().enumerate() {
            let value = match column.expression {
                Expression::Action => Some(Cell::Action(action.kind.sql_name())),
                Expression::Inserted(index) => field(action.inserted.as_deref(), index),
                Expression::Deleted(index) => field(action.deleted.as_deref(), index),
                Expression::Source(index) => field(action.source.as_deref(), index),
                Expression::Evaluated(index) => {
                    let Some(value) = action.evaluated.get(index) else {
                        return Err(ProjectionError::MissingEvaluated {
                            row: row_index,
                            slot: index,
                        });
                    };
                    Some(match value {
                        Some(value) => Cell::Value(value.clone()),
                        None => Cell::Null,
                    })
                }
            }
            .ok_or(ProjectionError::MissingField {
                row: row_index,
                column: column_index,
            })?;
            row.push(value);
        }
        rows.push(row);
    }

    Ok(match projection.destination {
        Destination::Direct => Projected::Direct(DirectOutput {
            columns: projection
                .columns
                .into_iter()
                .map(|column| (column.name, column.descriptor))
                .collect(),
            rows,
        }),
        Destination::Into(columns) => {
            let assignments = rows
                .into_iter()
                .map(|values| SinkAssignment {
                    values: columns
                        .iter()
                        .zip(values)
                        .map(|(column, value)| (column.name.clone(), value))
                        .collect(),
                })
                .collect();
            Projected::Into(IntoOutput {
                columns,
                assignments,
            })
        }
    })
}
