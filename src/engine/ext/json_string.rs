//! FOR JSON AUTO, JSON_MODIFY, ordered STRING_AGG, STRING_SPLIT and HASHBYTES.
//!
//! Stub until its gap task lands; see docs/extension-hooks.md.
use super::Feature;

#[derive(Default)]
pub(crate) struct State;

pub(super) struct Hooks;

impl Feature for Hooks {
    fn name(&self) -> &'static str {
        "json_string"
    }
}
