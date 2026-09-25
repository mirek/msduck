//! Pure MERGE action selection over a caller-provided join snapshot.
//!
//! Binding, SQL expression evaluation, TOP candidate choice and physical writes
//! belong to other layers. The caller supplies stable row identities, pre-write
//! images, ordered arms and already evaluated arm outcomes. This function never
//! invokes an expression or mutates a database.

use std::collections::HashSet;
use std::hash::Hash;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Relation {
    Matched,
    SourceOnly,
    TargetOnly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

/// An arm's predicate is evaluated by the binder against the original snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Arm {
    pub relation: Relation,
    pub action: ActionKind,
}

/// Identity and image are distinct: equal-valued rows may have distinct IDs.
#[derive(Debug, PartialEq, Eq)]
pub struct Row<I, V> {
    pub id: I,
    pub image: V,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Candidate<TargetId, SourceId, TargetRow, SourceRow> {
    Matched {
        target: Row<TargetId, TargetRow>,
        source: Row<SourceId, SourceRow>,
    },
    SourceOnly {
        source: Row<SourceId, SourceRow>,
    },
    TargetOnly {
        target: Row<TargetId, TargetRow>,
    },
}

impl<TargetId, SourceId, TargetRow, SourceRow> Candidate<TargetId, SourceId, TargetRow, SourceRow> {
    pub const fn relation(&self) -> Relation {
        match self {
            Self::Matched { .. } => Relation::Matched,
            Self::SourceOnly { .. } => Relation::SourceOnly,
            Self::TargetOnly { .. } => Relation::TargetOnly,
        }
    }
}

/// `Apply` carries the one pre-evaluated new target image for INSERT/UPDATE.
/// DELETE has no new image. No expression is evaluated in this module.
#[derive(Debug, PartialEq, Eq)]
pub enum ArmEvaluation<TargetRow> {
    NotQualified,
    Apply { new_target: Option<TargetRow> },
}

#[derive(Debug, PartialEq, Eq)]
pub struct EvaluatedCandidate<TargetId, SourceId, TargetRow, SourceRow> {
    pub candidate: Candidate<TargetId, SourceId, TargetRow, SourceRow>,
    /// One entry per arm, including arms for other relation categories.
    pub arms: Vec<ArmEvaluation<TargetRow>>,
}

/// TOP ordering is unspecified by SQL Server. The caller explicitly supplies
/// the candidate indices retained after its own TOP resolution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Selection<'a> {
    All,
    Indices(&'a [usize]),
}

#[derive(Debug, PartialEq, Eq)]
pub struct PlannedAction<TargetId, SourceId, TargetRow, SourceRow> {
    pub candidate_index: usize,
    pub arm_index: usize,
    pub kind: ActionKind,
    pub target_id: Option<TargetId>,
    pub source_id: Option<SourceId>,
    pub old_target: Option<TargetRow>,
    pub new_target: Option<TargetRow>,
    pub source_image: Option<SourceRow>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Plan<TargetId, SourceId, TargetRow, SourceRow> {
    pub actions: Vec<PlannedAction<TargetId, SourceId, TargetRow, SourceRow>>,
}

impl<TargetId, SourceId, TargetRow, SourceRow> Plan<TargetId, SourceId, TargetRow, SourceRow> {
    pub fn affected_rows(&self) -> usize {
        self.actions.len()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum PlanError<TargetId> {
    InvalidArm {
        arm_index: usize,
    },
    InvalidSelection {
        candidate_index: usize,
    },
    RepeatedSelection {
        candidate_index: usize,
    },
    WrongArmCount {
        candidate_index: usize,
    },
    WrongRelation {
        candidate_index: usize,
        arm_index: usize,
    },
    WrongNewImage {
        candidate_index: usize,
        arm_index: usize,
    },
    DuplicateTarget {
        target_id: TargetId,
    },
}

/// SQL Server error 8672, observed in `reference/merge-execution.json`.
pub const DUPLICATE_TARGET_MESSAGE: &str = "The MERGE statement attempted to UPDATE or DELETE the same row more than once. This happens when a target row matches more than one source row. A MERGE statement cannot UPDATE/DELETE the same row of the target table multiple times. Refine the ON clause to ensure a target row matches at most one source row, or use the GROUP BY clause to group the source rows.";

impl<TargetId> PlanError<TargetId> {
    /// Only duplicate-target selection is a SQL diagnostic here. Other variants
    /// indicate a malformed binder input, not a user-facing SQL Server error.
    pub const fn sql_diagnostic(&self) -> Option<(i32, u8, u8, &'static str)> {
        match self {
            Self::DuplicateTarget { .. } => Some((8672, 1, 16, DUPLICATE_TARGET_MESSAGE)),
            _ => None,
        }
    }
}

fn valid_arm(arm: Arm) -> bool {
    matches!(
        (arm.relation, arm.action),
        (Relation::Matched, ActionKind::Update | ActionKind::Delete)
            | (Relation::SourceOnly, ActionKind::Insert)
            | (
                Relation::TargetOnly,
                ActionKind::Update | ActionKind::Delete
            )
    )
}

/// Select at most one ordered arm per candidate and reject a repeated target
/// UPDATE/DELETE before the caller receives any action to execute.
pub fn select_actions<TargetId, SourceId, TargetRow, SourceRow>(
    arms: &[Arm],
    candidates: Vec<EvaluatedCandidate<TargetId, SourceId, TargetRow, SourceRow>>,
    selection: Selection<'_>,
) -> Result<Plan<TargetId, SourceId, TargetRow, SourceRow>, PlanError<TargetId>>
where
    TargetId: Eq + Hash,
{
    for (arm_index, arm) in arms.iter().copied().enumerate() {
        if !valid_arm(arm) {
            return Err(PlanError::InvalidArm { arm_index });
        }
    }

    let count = candidates.len();
    let indices: Vec<usize> = match selection {
        Selection::All => (0..count).collect(),
        Selection::Indices(indices) => {
            let mut seen = HashSet::with_capacity(indices.len());
            for &candidate_index in indices {
                if candidate_index >= count {
                    return Err(PlanError::InvalidSelection { candidate_index });
                }
                if !seen.insert(candidate_index) {
                    return Err(PlanError::RepeatedSelection { candidate_index });
                }
            }
            indices.to_vec()
        }
    };

    let mut candidates: Vec<_> = candidates.into_iter().map(Some).collect();
    let mut actions = Vec::new();
    for candidate_index in indices {
        let evaluated = candidates[candidate_index]
            .take()
            .expect("selection indices were checked for duplicates");
        if evaluated.arms.len() != arms.len() {
            return Err(PlanError::WrongArmCount { candidate_index });
        }
        let relation = evaluated.candidate.relation();
        let selected = evaluated
            .arms
            .into_iter()
            .enumerate()
            .find(|(_, evaluation)| matches!(evaluation, ArmEvaluation::Apply { .. }));
        let Some((arm_index, ArmEvaluation::Apply { new_target })) = selected else {
            continue;
        };
        let arm = arms[arm_index];
        if arm.relation != relation {
            return Err(PlanError::WrongRelation {
                candidate_index,
                arm_index,
            });
        }
        if new_target.is_some() != (arm.action != ActionKind::Delete) {
            return Err(PlanError::WrongNewImage {
                candidate_index,
                arm_index,
            });
        }
        let (target_id, source_id, old_target, source_image) = match evaluated.candidate {
            Candidate::Matched { target, source } => (
                Some(target.id),
                Some(source.id),
                Some(target.image),
                Some(source.image),
            ),
            Candidate::SourceOnly { source } => (None, Some(source.id), None, Some(source.image)),
            Candidate::TargetOnly { target } => (Some(target.id), None, Some(target.image), None),
        };
        actions.push(PlannedAction {
            candidate_index,
            arm_index,
            kind: arm.action,
            target_id,
            source_id,
            old_target,
            new_target,
            source_image,
        });
    }

    let mut written = HashSet::new();
    let mut duplicate_index = None;
    for (index, action) in actions.iter().enumerate() {
        if matches!(action.kind, ActionKind::Update | ActionKind::Delete) {
            let target_id = action
                .target_id
                .as_ref()
                .expect("validated update/delete arms have target identities");
            if !written.insert(target_id) {
                duplicate_index = Some(index);
                break;
            }
        }
    }
    drop(written);
    if let Some(index) = duplicate_index {
        let target_id = actions
            .into_iter()
            .nth(index)
            .and_then(|candidate| candidate.target_id)
            .expect("duplicate action has a target identity");
        return Err(PlanError::DuplicateTarget { target_id });
    }
    Ok(Plan { actions })
}
