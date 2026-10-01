//! Syntax for: Contextual identifiers.
//!
//! SQL Server takes many words as regular identifiers that sqlparser (for
//! other dialects) or DuckDB treat as keywords: `offset`, `limit`,
//! `qualify`, `at`, `using`, `window`, `interval`, `lateral` and so on.
//! `reference/gaps-identifiers.json` records SQL Server accepting each of
//! them as a table, column, alias and parameter name.
//!
//! Parsing keeps them identifiers in three ways:
//! - [`tokens`] drops the keyword meaning of words that are never T-SQL
//!   syntax (and of function-like words not followed by `(`), so sqlparser
//!   reads them as plain names everywhere;
//! - [`select_item_alias`] and [`table_factor_alias`] accept the remaining
//!   words that are T-SQL syntax only in particular places (`OFFSET` after
//!   ORDER BY, `AT TIME ZONE`, `MERGE ... USING`, the WINDOW clause) as bare
//!   aliases everywhere else;
//! - [`reserved_by_backend`] tells the engine which names must be delimited
//!   before the statement is rendered for DuckDB.
use sqlparser::{
    ast::{Expr, Statement},
    keywords::Keyword,
    parser::{Parser, ParserError},
    tokenizer::{Token, TokenWithSpan},
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

/// Words the DuckDB grammar does not accept as bare column names, but SQL
/// Server accepts as regular identifiers: DuckDB's `reserved` and
/// `type_func_name` keyword categories (the bundled DuckDB's
/// `third_party/libpg_query/include/parser/kwlist.hpp`, equivalently
/// `duckdb_keywords()`), minus SQL Server's reserved keywords. A T-SQL
/// reserved word cannot be a regular identifier (SQL Server raises 156).
/// DuckDB's `col_name` keywords (`time`, `int`, `position`, ...) are valid
/// DuckDB column, table and alias names, and several of them are type names
/// in msduck's lowered SQL, so they stay undelimited.
/// Sorted, lower case.
const BACKEND_RESERVED: &[&str] = &[
    "analyse",
    "analyze",
    "anti",
    "array",
    "asof",
    "asymmetric",
    "at",
    "binary",
    "both",
    "cast",
    "collation",
    "columns",
    "concurrently",
    "deferrable",
    "describe",
    "do",
    "false",
    "freeze",
    "generated",
    "glob",
    "ilike",
    "initially",
    "isnull",
    "lambda",
    "lateral",
    "leading",
    "limit",
    "map",
    "natural",
    "notnull",
    "offset",
    "only",
    "overlaps",
    "pivot_longer",
    "pivot_wider",
    "placing",
    "positional",
    "qualify",
    "returning",
    "semi",
    "show",
    "similar",
    "struct",
    "summarize",
    "symmetric",
    "trailing",
    "true",
    "try_cast",
    "unpack",
    "using",
    "variadic",
    "verbose",
    "window",
];

/// Whether DuckDB needs `word` delimited where SQL Server takes it as a
/// regular identifier.
pub fn reserved_by_backend(word: &str) -> bool {
    word.len() <= 16
        && BACKEND_RESERVED
            .binary_search(&word.to_ascii_lowercase().as_str())
            .is_ok()
}

/// Keywords sqlparser knows from other dialects that never start or continue
/// T-SQL syntax. Sorted, upper case.
const NEVER_SYNTAX: &[&str] = &[
    "ANALYZE",
    "ANTI",
    "ARRAY",
    "ASOF",
    "ASYMMETRIC",
    "CONCURRENTLY",
    "DEFERRABLE",
    "DESCRIBE",
    "DO",
    "FREEZE",
    "GLOB",
    "ILIKE",
    "INITIALLY",
    "INTERVAL",
    "LAMBDA",
    "LATERAL",
    "LIMIT",
    "MAP",
    "NATURAL",
    "OVERLAPS",
    "PLACING",
    "QUALIFY",
    "RETURNING",
    "SEMI",
    "SHOW",
    "SIMILAR",
    "STRUCT",
    "SYMMETRIC",
    "VARIADIC",
    "VERBOSE",
];

/// Function-like keywords that are syntax only directly before `(`.
const CALL_SYNTAX: &[&str] = &[
    "CAST",
    "EXTRACT",
    "OVERLAY",
    "SUBSTRING",
    "TRIM",
    "TRY_CAST",
];

/// Drop the keyword meaning of unquoted words that are identifiers in T-SQL.
pub fn tokens(tokens: &mut [TokenWithSpan]) {
    for index in 0..tokens.len() {
        let Token::Word(word) = &tokens[index].token else {
            continue;
        };
        if word.quote_style.is_some() || word.keyword == Keyword::NoKeyword {
            continue;
        }
        let upper = word.value.to_ascii_uppercase();
        let plain = NEVER_SYNTAX.binary_search(&upper.as_str()).is_ok()
            || (CALL_SYNTAX.binary_search(&upper.as_str()).is_ok()
                && !matches!(next(tokens, index), Some(Token::LParen)));
        if plain && let Token::Word(word) = &mut tokens[index].token {
            word.keyword = Keyword::NoKeyword;
        }
    }
}

/// A prefix expression that starts with a contextual word, read as a column
/// reference. The tokenizer already handles batches; this covers text that
/// sqlparser tokenizes itself, such as stored CHECK definitions parsed with
/// `ServerDialect`. `None` defers to the dialect.
pub fn parse_prefix(parser: &mut Parser) -> Option<Result<Expr, ParserError>> {
    let Token::Word(word) = &parser.peek_token_ref().token else {
        return None;
    };
    if word.quote_style.is_some()
        || word.keyword == Keyword::NoKeyword
        || matches!(parser.peek_nth_token_ref(1).token, Token::LParen)
    {
        return None;
    }
    let upper = word.value.to_ascii_uppercase();
    if NEVER_SYNTAX.binary_search(&upper.as_str()).is_err()
        && CALL_SYNTAX.binary_search(&upper.as_str()).is_err()
    {
        return None;
    }
    Some((|| {
        let mut parts = vec![parser.parse_identifier()?];
        while parser.peek_token_ref().token == Token::Period
            && matches!(parser.peek_nth_token_ref(1).token, Token::Word(_))
        {
            parser.next_token();
            parts.push(parser.parse_identifier()?);
        }
        Ok(match parts.len() {
            1 => Expr::Identifier(parts.remove(0)),
            _ => Expr::CompoundIdentifier(parts),
        })
    })())
}

/// The next significant token after `index`.
fn next(tokens: &[TokenWithSpan], index: usize) -> Option<&Token> {
    tokens[index + 1..]
        .iter()
        .map(|token| &token.token)
        .find(|token| !matches!(token, Token::Whitespace(_)))
}

/// Whether a bare (implicit) select-item alias candidate `keyword`, already
/// consumed, is an alias. `None` defers to the dialect's general rule.
pub fn select_item_alias(explicit: bool, keyword: &Keyword, parser: &mut Parser) -> Option<bool> {
    if explicit {
        return None;
    }
    match keyword {
        // ORDER BY ... OFFSET never follows a select item.
        Keyword::OFFSET | Keyword::USING => Some(true),
        Keyword::AT => Some(!parser.peek_keyword(Keyword::TIME)),
        Keyword::WINDOW => Some(!window_clause(parser)),
        _ => None,
    }
}

/// Whether a bare (implicit) table alias candidate `keyword`, already
/// consumed, is an alias. `None` defers to the dialect's general rule.
pub fn table_factor_alias(explicit: bool, keyword: &Keyword, parser: &mut Parser) -> Option<bool> {
    if explicit {
        return None;
    }
    match keyword {
        Keyword::OFFSET | Keyword::AT => Some(true),
        Keyword::WINDOW => Some(!window_clause(parser)),
        // `MERGE target USING source`: USING is followed by a table source.
        // As an alias it is followed by a clause keyword or punctuation.
        Keyword::USING => Some(match &parser.peek_token_ref().token {
            Token::Word(word) => {
                word.quote_style.is_none()
                    && matches!(
                        word.keyword,
                        Keyword::WHERE
                            | Keyword::GROUP
                            | Keyword::HAVING
                            | Keyword::ORDER
                            | Keyword::JOIN
                            | Keyword::INNER
                            | Keyword::LEFT
                            | Keyword::RIGHT
                            | Keyword::FULL
                            | Keyword::CROSS
                            | Keyword::OUTER
                            | Keyword::ON
                            | Keyword::UNION
                            | Keyword::EXCEPT
                            | Keyword::INTERSECT
                            | Keyword::OPTION
                            | Keyword::FOR
                            | Keyword::WINDOW
                            | Keyword::SET
                            | Keyword::OUTPUT
                            | Keyword::WITH
                    )
            }
            Token::LParen => false,
            _ => true,
        }),
        _ => None,
    }
}

/// `WINDOW name AS (...)` follows the consumed WINDOW keyword.
fn window_clause(parser: &Parser) -> bool {
    matches!(parser.peek_token_ref().token, Token::Word(_))
        && matches!(parser.peek_nth_token_ref(1).token, Token::Word(ref word) if word.keyword == Keyword::AS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_lists_are_sorted() {
        assert!(BACKEND_RESERVED.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(BACKEND_RESERVED.iter().all(|word| word.len() <= 16));
        assert!(NEVER_SYNTAX.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(CALL_SYNTAX.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn backend_reserved_words_are_case_insensitive() {
        for word in ["offset", "OFFSET", "Limit", "qualify", "at", "TRUE"] {
            assert!(reserved_by_backend(word), "{word}");
        }
        // Ordinary names and T-SQL reserved words are left alone.
        for word in [
            "id", "name", "pivot", "values", "select", "offsets", "@offset",
        ] {
            assert!(!reserved_by_backend(word), "{word}");
        }
    }

    fn parse(sql: &str) -> Result<String, String> {
        crate::batch::parse(sql)
            .map(|statements| {
                statements
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
            })
            .map_err(|error| error.to_string())
    }

    #[test]
    fn contextual_words_parse_as_names() {
        assert_eq!(
            parse("SELECT other offset, other at, other using, other window FROM t limit").unwrap(),
            "SELECT other AS offset, other AS at, other AS using, other AS window FROM t limit"
        );
        assert_eq!(
            parse("SELECT interval, lateral.trim FROM lateral WHERE qualify = 1").unwrap(),
            "SELECT interval, lateral.trim FROM lateral WHERE qualify = 1"
        );
        assert_eq!(
            parse(
                "SELECT a FROM t at WHERE at.a = 1; SELECT a FROM t offset; SELECT a FROM t window"
            )
            .unwrap(),
            "SELECT a FROM t at WHERE at.a = 1; SELECT a FROM t offset; SELECT a FROM t window"
        );
        assert_eq!(
            parse("UPDATE limit SET interval = interval + 1 WHERE trim = 2").unwrap(),
            "UPDATE limit SET interval = interval + 1 WHERE trim = 2"
        );
    }

    #[test]
    fn raw_sqlparser_tokens_read_contextual_words_as_names() {
        for (sql, expected) in [
            ("interval > 0", "interval > 0"),
            ("trim > 0 AND cast < 2", "trim > 0 AND cast < 2"),
            ("t.interval + lateral.limit", "t.interval + lateral.limit"),
            ("TRIM(x) + CAST(y AS INT)", "TRIM(x) + CAST(y AS INT)"),
        ] {
            let expr = Parser::new(&crate::dialect::ServerDialect)
                .try_with_sql(sql)
                .and_then(|mut parser| parser.parse_expr())
                .unwrap_or_else(|error| panic!("{sql}: {error}"));
            assert_eq!(expr.to_string(), expected);
        }
    }

    #[test]
    fn tsql_syntax_keeps_its_keywords() {
        for sql in [
            "SELECT a FROM t ORDER BY a OFFSET 1 ROWS FETCH NEXT 2 ROWS ONLY",
            "SELECT a AT TIME ZONE 'UTC' FROM t",
            "SELECT TRIM(a), CAST(a AS int), TRY_CAST(a AS int) FROM t",
            "SELECT TRIM('x' FROM a) FROM t",
            "SELECT a, SUM(a) OVER w FROM t WINDOW w AS (ORDER BY a)",
            "MERGE t USING s ON t.a = s.a WHEN MATCHED THEN DELETE;",
            "MERGE t AS x USING (SELECT 1 AS a) AS s ON x.a = s.a WHEN MATCHED THEN DELETE;",
        ] {
            let parsed = parse(sql).unwrap_or_else(|error| panic!("{sql}: {error}"));
            assert!(
                !parsed.contains(" t at")
                    && !parsed.contains(" AS at")
                    && !parsed.contains(" t using"),
                "{parsed}"
            );
        }
    }
}
