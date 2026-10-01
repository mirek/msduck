//! Syntax for: Constraint, module and file catalog views, OBJECT_DEFINITION, sp_pkeys, sp_fkeys and sp_rename.
//!
//! Stub until its gap task lands; see docs/extension-hooks.md.
use sqlparser::{
    ast::Statement,
    parser::{Parser, ParserError},
};

pub mod definition;

/// Parse a statement this feature owns, or decline without consuming tokens.
pub fn parse(_parser: &mut Parser) -> Option<Result<Statement, ParserError>> {
    None
}

/// Whether this feature validates `statement` itself, so the generic batch
/// checks (target-shape canonicalization and variable preflight) skip it.
pub fn owns(_statement: &Statement) -> bool {
    false
}
