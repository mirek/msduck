//! Application locks: sp_getapplock, sp_releaseapplock, APPLOCK_MODE and APPLOCK_TEST.
//!
//! Stub until its gap task lands; see docs/extension-hooks.md.
use super::Feature;

#[derive(Default)]
pub(crate) struct State;

pub(super) struct Hooks;

impl Feature for Hooks {
    fn name(&self) -> &'static str {
        "applock"
    }
}
