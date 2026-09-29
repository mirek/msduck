//! Pure binding and session-state rules for `SET IDENTITY_INSERT`.
//!
//! This module is intentionally path-imported until the SQL crate export is free.
//! The caller resolves `Operation::parts` against a catalog snapshot and passes
//! a stable table identity to `SessionState::apply`.

use sqlparser::ast::{Ident, SessionParamValue, Set, SetSessionParamKind, Statement};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Operation {
    /// One to three parsed identifier parts, in source order. Quoting and case
    /// remain available to the catalog resolver; no textual normalization is
    /// used to decide whether two names identify the same table.
    pub parts: Vec<Ident>,
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BindError {
    UnsupportedTarget,
}

/// Extract only the typed parser variant. Other SET statements are left to
/// their own binder, while unsupported target shapes fail closed.
pub fn operation(statement: &Statement) -> Result<Option<Operation>, BindError> {
    let Statement::Set(Set::SetSessionParam(SetSessionParamKind::IdentityInsert(setting))) =
        statement
    else {
        return Ok(None);
    };
    if !(1..=3).contains(&setting.obj.0.len()) {
        return Err(BindError::UnsupportedTarget);
    }
    let parts = setting
        .obj
        .0
        .iter()
        .map(|part| part.as_ident().cloned().ok_or(BindError::UnsupportedTarget))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(Operation {
        parts,
        enabled: matches!(setting.value, SessionParamValue::On),
    }))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionState<K> {
    active: Option<K>,
}

impl<K> Default for SessionState<K> {
    fn default() -> Self {
        Self { active: None }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transition {
    Enabled,
    AlreadyEnabled,
    Disabled,
    AlreadyDisabled,
    OtherTableUnaffected,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict<K> {
    pub active: K,
    pub requested: K,
}

impl<K: Clone + Eq> SessionState<K> {
    pub fn active(&self) -> Option<&K> {
        self.active.as_ref()
    }

    /// Nested RPC execution inherits caller settings but must not write back
    /// its local SET changes. Shared allocation is a separate root concern.
    pub fn fork_rpc(&self) -> Self {
        self.clone()
    }

    /// Apply a setting only after the root has resolved a real identity table.
    /// A failed resolution must never call this method.
    pub fn apply(&mut self, setting: &Operation, resolved: K) -> Result<Transition, Conflict<K>> {
        match (setting.enabled, self.active.as_ref()) {
            (true, None) => {
                self.active = Some(resolved);
                Ok(Transition::Enabled)
            }
            (true, Some(active)) if *active == resolved => Ok(Transition::AlreadyEnabled),
            (true, Some(active)) => Err(Conflict {
                active: active.clone(),
                requested: resolved,
            }),
            (false, None) => Ok(Transition::AlreadyDisabled),
            (false, Some(active)) if *active == resolved => {
                self.active = None;
                Ok(Transition::Disabled)
            }
            (false, Some(_)) => Ok(Transition::OtherTableUnaffected),
        }
    }
}
