//! Syntax for: Key and index columns of every type, UNIQUE NULL semantics and index options.
//!
//! The feature parses `CREATE [UNIQUE] [CLUSTERED | NONCLUSTERED] INDEX` in
//! T-SQL clause order (see [`index`]) and provides the deterministic parts of
//! keys-managed indexes: key expressions and value display ([`value`]),
//! filter lowering ([`filter`]) and duplicate-key messages ([`message`]).
//! Column-level `CONSTRAINT name UNIQUE (col)` / `PRIMARY KEY (col)` forms
//! are rewritten at tokenization in `dialect::key_index_type`. See
//! docs/gaps-keys.md.
use sqlparser::{
    ast::Statement,
    parser::{Parser, ParserError},
};

pub mod collation;
pub mod filter;
pub mod index;
pub mod message;
pub mod table;
pub mod value;

/// Parse a statement this feature owns, or decline without consuming tokens.
pub fn parse(parser: &mut Parser) -> Option<Result<Statement, ParserError>> {
    index::starts(parser).then(|| index::parse_index(parser))
}

/// Whether this feature validates `statement` itself, so the generic batch
/// checks (target-shape canonicalization and variable preflight) skip it.
pub fn owns(_statement: &Statement) -> bool {
    false
}
