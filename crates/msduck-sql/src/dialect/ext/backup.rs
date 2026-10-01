//! Syntax for: BACKUP DATABASE, RESTORE HEADERONLY/FILELISTONLY/DATABASE and msdb backup history.
//!
//! Stub until its gap task lands; see docs/extension-hooks.md.
use sqlparser::{
    ast::Statement,
    parser::{Parser, ParserError},
};

/// Parse a statement this feature owns, or decline without consuming tokens.
pub fn parse(_parser: &mut Parser) -> Option<Result<Statement, ParserError>> {
    None
}

/// Whether this feature validates `statement` itself, so the generic batch
/// checks (target-shape canonicalization and variable preflight) skip it.
pub fn owns(_statement: &Statement) -> bool {
    false
}
