//! Constraint, module and file catalog views, OBJECT_DEFINITION, sp_pkeys, sp_fkeys and sp_rename.
//!
//! Stub until its gap task lands; see docs/extension-hooks.md.
use super::Feature;

#[derive(Default)]
pub(crate) struct State;

pub(super) struct Hooks;

impl Feature for Hooks {
    fn name(&self) -> &'static str {
        "catalog"
    }
}
