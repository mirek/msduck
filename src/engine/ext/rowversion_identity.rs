//! rowversion/timestamp columns, decimal identity, SCOPE_IDENTITY and IDENTITY_INSERT.
//!
//! Stub until its gap task lands; see docs/extension-hooks.md.
use super::Feature;

#[derive(Default)]
pub(crate) struct State;

pub(super) struct Hooks;

impl Feature for Hooks {
    fn name(&self) -> &'static str {
        "rowversion_identity"
    }
}
