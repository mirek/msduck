//! Syntax for: Explicit COLLATE, styled CONVERT, FORMAT, SERVERPROPERTY, DATABASEPROPERTYEX and ROWCOUNT_BIG,
//! and delimited system type names such as `CONVERT([nvarchar](200), x)`.
//!
//! See docs/extension-hooks.md and docs/bracket-types.md.
use sqlparser::{
    ast::Statement,
    parser::{Parser, ParserError},
};

pub mod bracket_types;

/// Parse a statement this feature owns, or decline without consuming tokens.
pub fn parse(_parser: &mut Parser) -> Option<Result<Statement, ParserError>> {
    None
}

/// Whether this feature validates `statement` itself, so the generic batch
/// checks (target-shape canonicalization and variable preflight) skip it.
pub fn owns(_statement: &Statement) -> bool {
    false
}
