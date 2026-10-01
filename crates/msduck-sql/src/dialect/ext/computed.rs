//! Syntax for computed columns over Unicode and JSON expressions, and for
//! session-dependent column defaults. Computed columns themselves are parsed
//! by `dialect::computed_column`; this feature claims no statements. See
//! docs/gaps-computed.md.
use sqlparser::{
    ast::Statement,
    parser::{Parser, ParserError},
};

pub mod session;

/// Parse a statement this feature owns, or decline without consuming tokens.
pub fn parse(_parser: &mut Parser) -> Option<Result<Statement, ParserError>> {
    None
}

/// Whether this feature validates `statement` itself, so the generic batch
/// checks (target-shape canonicalization and variable preflight) skip it.
pub fn owns(_statement: &Statement) -> bool {
    false
}
