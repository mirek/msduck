//! Syntax for stored procedures, dynamic SQL and sp_executesql.
//!
//! - `EXEC`/`EXECUTE` calls with `@status =`, positional, named, `DEFAULT`
//!   and `OUTPUT` arguments, `EXEC (string)`, and `WITH RECOMPILE`. They stay
//!   [`Statement::Execute`] so the engine's procedure-call path (and other
//!   features' `exec` hooks) still see them; [`call`] decodes them.
//! - `BEGIN TRY` bodies mark the calls they contain, so a procedure can tell
//!   that its caller will catch its errors.
//! - `DROP PROC[EDURE] [IF EXISTS]` and the header of `CREATE`/`ALTER`
//!   `PROCEDURE` ([`definition`]).
//!
//! See docs/gaps-procedures.md for the encoding and its SQL Server evidence.
use msduck_core::diagnostic::SqlError;
use sqlparser::{
    ast::Statement,
    parser::{Parser, ParserError},
};

mod call;
mod definition;

pub use call::{Argument, Call, Target, call, mark_try, starts_statement};
pub use definition::{Definition, Parameter, definition, offset};

/// Parse a statement this feature owns, or decline without consuming tokens.
pub fn parse(parser: &mut Parser) -> Option<Result<Statement, ParserError>> {
    call::parse(parser)
}

/// Procedure calls and drops keep the generic batch checks.
pub fn owns(_statement: &Statement) -> bool {
    false
}

pub(crate) const NOT_FIRST: &str =
    "'CREATE/ALTER PROCEDURE' must be the first statement in a query batch.";
pub(crate) const OUTPUT_CONSTANT: &str =
    "Cannot use the OUTPUT option when passing a constant to a stored procedure.";
pub(crate) fn named_then_positional(position: usize) -> String {
    format!(
        "Must pass parameter number {position} and subsequent parameters as '@name = value'. After the form '@name = value' has been used, all subsequent parameters must be passed in the form '@name = value'."
    )
}

/// SQL Server compilation diagnostics for this feature's syntax errors, which
/// end the whole batch before it runs (captured: 111, 119 and 179).
pub fn diagnostic(error: &ParserError) -> Option<SqlError> {
    let ParserError::ParserError(message) = error else {
        return None;
    };
    let number = if message == NOT_FIRST {
        111
    } else if message == OUTPUT_CONSTANT {
        179
    } else if message.starts_with("Must pass parameter number ") {
        119
    } else if message.starts_with("Incorrect syntax near the keyword '") {
        156
    } else if message.starts_with("Incorrect syntax near ") {
        102
    } else {
        return None;
    };
    Some(SqlError::syntax(number, 1, message.clone()))
}
