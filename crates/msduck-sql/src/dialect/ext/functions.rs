//! Syntax for user-defined scalar, inline and multi-statement table-valued
//! functions: definitions, compile-time checks and folding of bodies into
//! T-SQL expressions and queries. See docs/gaps-functions.md.
use sqlparser::{
    ast::Statement,
    parser::{Parser, ParserError},
};

pub mod definition;
pub mod fold;
pub mod interpret;
pub mod validate;

pub use definition::{Action, Body, Definition, Options, Parameter, Returns};

/// Parse a statement this feature owns, or decline without consuming tokens.
/// Definitions are batch-level (the runtime batch hook handles them), and
/// DROP FUNCTION parses as sqlparser's `DropFunction`.
pub fn parse(_parser: &mut Parser) -> Option<Result<Statement, ParserError>> {
    None
}

/// Whether this feature validates `statement` itself, so the generic batch
/// checks (target-shape canonicalization and variable preflight) skip it.
pub fn owns(_statement: &Statement) -> bool {
    false
}

/// Whether a batch contains a function definition anywhere after its first
/// statement (SQL Server error 111).
pub fn misplaced_definition(sql: &str) -> Option<&'static str> {
    use sqlparser::tokenizer::Token;
    if Action::of(sql).is_some() {
        return None;
    }
    let tokens = crate::dialect::tokenize(sql).ok()?;
    let words: Vec<String> = tokens
        .into_iter()
        .filter(|t| !matches!(t.token, Token::Whitespace(_)))
        .map(|t| match t.token {
            Token::Word(word) if word.quote_style.is_none() => word.value.to_uppercase(),
            _ => String::new(),
        })
        .collect();
    words.windows(4).find_map(|w| {
        match (w[0].as_str(), w[1].as_str(), w[2].as_str(), w[3].as_str()) {
            ("CREATE", "FUNCTION", _, _) | ("CREATE", "OR", "ALTER", "FUNCTION") => {
                Some("CREATE FUNCTION")
            }
            (first, "ALTER", "FUNCTION", _) if first != "OR" => Some("ALTER FUNCTION"),
            _ => None,
        }
    })
}
