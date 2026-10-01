//! Syntax for: FOR JSON AUTO, JSON_MODIFY, ordered STRING_AGG, STRING_SPLIT and HASHBYTES.
//!
//! The statements themselves are ordinary sqlparser statements, so `parse`
//! and `owns` decline. This module keeps the deterministic rules: FOR JSON
//! AUTO source classification, nesting plans and grouping
//! ([`auto`]), and JSON_MODIFY document editing ([`modify`]). Runtime
//! lowering lives in `src/engine/ext/json_string.rs`; see
//! docs/gaps-json_string.md.
use sqlparser::{
    ast::{ForClause, Query, Statement},
    parser::{Parser, ParserError},
};

pub mod auto;
pub mod modify;

/// Parse a statement this feature owns, or decline without consuming tokens.
pub fn parse(_parser: &mut Parser) -> Option<Result<Statement, ParserError>> {
    None
}

/// Whether this feature validates `statement` itself, so the generic batch
/// checks (target-shape canonicalization and variable preflight) skip it.
pub fn owns(_statement: &Statement) -> bool {
    false
}

/// Prefix of a native or preflight error carrying an exact SQL Server
/// identity through DuckDB's stringified errors. The root adapter
/// (`json_extract::diagnostic`) recovers the number, state and class.
pub const MARKER: &str = "__msduck_sql_error_v1:";

/// Encode a SQL Server error as a marker message.
pub fn error(number: i32, state: u8, class: u8, text: &str) -> String {
    format!("{MARKER}{number}:{state}:{class}:{text}")
}

/// Batch preflight for a query whose FOR clause is FOR JSON AUTO: the
/// compile-time errors 13620 (ROOT with WITHOUT_ARRAY_WRAPPER), 13600 (no
/// table) and 13605 (unnamed column), in SQL Server's order.
pub fn validate_for_json_auto(query: &Query) -> Result<(), String> {
    let Some(ForClause::Json {
        root,
        without_array_wrapper,
        ..
    }) = &query.for_clause
    else {
        return Ok(());
    };
    if root.is_some() && *without_array_wrapper {
        return Err(error(13620, 1, 16, auto::ROOT_WITHOUT_ARRAY_WRAPPER));
    }
    auto::validate(&query.body)
}
