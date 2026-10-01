//! Syntax for: rowversion/timestamp columns, decimal identity, SCOPE_IDENTITY and IDENTITY_INSERT.
//!
//! T-SQL lets a CREATE TABLE column list name a bare `timestamp` column with
//! no data type: its name is `timestamp` and its type is timestamp
//! (rowversion). sqlparser requires a data type, so such a statement is
//! parsed again with the column spelled `timestamp rowversion`. Everything
//! else (types, IDENTITY_INSERT, SCOPE_IDENTITY) uses the built-in syntax and
//! is handled at runtime by `src/engine/ext/rowversion_identity.rs`.
use sqlparser::{
    ast::Statement,
    keywords::Keyword,
    parser::{Parser, ParserError},
    tokenizer::{Token, TokenWithSpan, Word},
};

/// Parse a statement this feature owns, or decline without consuming tokens.
pub fn parse(parser: &mut Parser) -> Option<Result<Statement, ParserError>> {
    let (tokens, inserted) = bare_timestamp(parser)?;
    let mut inner = Parser::new(&crate::dialect::ServerDialect).with_tokens_with_locations(tokens);
    let statement = inner.parse_statement();
    if statement.is_ok() {
        // The inserted type names lie inside the consumed column list.
        for _ in 0..inner.index().saturating_sub(inserted) {
            parser.next_token();
        }
    }
    Some(statement)
}

/// Whether this feature validates `statement` itself, so the generic batch
/// checks (target-shape canonicalization and variable preflight) skip it.
pub fn owns(_statement: &Statement) -> bool {
    false
}

fn is_word(token: &Token, keyword: Keyword) -> bool {
    matches!(token, Token::Word(word) if word.keyword == keyword)
}

/// The remaining tokens with `rowversion` inserted after each bare
/// `timestamp` column of a CREATE TABLE column list, or None.
fn bare_timestamp(parser: &Parser) -> Option<(Vec<TokenWithSpan>, usize)> {
    if !is_word(&parser.peek_nth_token_ref(0).token, Keyword::CREATE)
        || !is_word(&parser.peek_nth_token_ref(1).token, Keyword::TABLE)
    {
        return None;
    }
    let mut tokens = vec![];
    let mut depth = 0usize;
    let mut column_start = false;
    let mut found = 0;
    let mut in_list = true;
    for n in 0.. {
        let token = parser.peek_nth_token(n);
        match &token.token {
            Token::EOF => break,
            Token::SemiColon if depth == 0 => break,
            // After the column list: give up unless a column matched, and
            // stop at the next statement (batches need not use semicolons).
            _ if !in_list && found == 0 => return None,
            Token::Word(word) if !in_list && depth == 0 && STATEMENTS.contains(&word.keyword) => {
                break;
            }
            _ => {}
        }
        let starts_column = std::mem::take(&mut column_start);
        if in_list
            && depth == 1
            && starts_column
            && matches!(&token.token, Token::Word(Word { value, quote_style: None, .. }) if value.eq_ignore_ascii_case("timestamp"))
            && matches!(
                parser.peek_nth_token_ref(n + 1).token,
                Token::Comma | Token::RParen
            )
        {
            let span = token.span;
            tokens.push(token);
            tokens.push(TokenWithSpan::new(
                Token::make_word("rowversion", None),
                span,
            ));
            found += 1;
            continue;
        }
        match &token.token {
            Token::LParen => {
                depth += 1;
                column_start = in_list && depth == 1;
            }
            Token::RParen => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    in_list = false;
                }
            }
            Token::Comma if depth == 1 => column_start = true,
            _ => {}
        }
        tokens.push(token);
    }
    tokens.push(TokenWithSpan::wrap(Token::EOF));
    (found > 0).then_some((tokens, found))
}

/// Keywords that start the next statement of a batch.
const STATEMENTS: &[Keyword] = &[
    Keyword::ALTER,
    Keyword::BEGIN,
    Keyword::CREATE,
    Keyword::DECLARE,
    Keyword::DELETE,
    Keyword::DROP,
    Keyword::EXEC,
    Keyword::EXECUTE,
    Keyword::IF,
    Keyword::INSERT,
    Keyword::MERGE,
    Keyword::PRINT,
    Keyword::RETURN,
    Keyword::SELECT,
    Keyword::SET,
    Keyword::TRUNCATE,
    Keyword::UPDATE,
    Keyword::USE,
    Keyword::WHILE,
];

#[cfg(test)]
mod tests {
    use crate::batch;

    #[test]
    fn bare_timestamp_columns_get_the_rowversion_type() {
        let statements = batch::parse(
            "CREATE TABLE stamps(id int, timestamp)\n INSERT stamps(id) VALUES(1) SELECT 2",
        )
        .unwrap();
        assert_eq!(statements.len(), 3);
        assert_eq!(statements[1].to_string(), "INSERT stamps (id) VALUES (1)");
        let statements = batch::parse("CREATE TABLE stamps(id int, timestamp); SELECT 1").unwrap();
        assert_eq!(statements.len(), 2);
        assert_eq!(
            statements[0].to_string(),
            "CREATE TABLE stamps (id INT, timestamp rowversion)"
        );
        let statements = batch::parse("CREATE TABLE t(timestamp, id int)").unwrap();
        assert_eq!(
            statements[0].to_string(),
            "CREATE TABLE t (timestamp rowversion, id INT)"
        );
        // A typed column named timestamp and a quoted name are unchanged.
        let statements =
            batch::parse("CREATE TABLE t(timestamp int, [timestamp2] timestamp)").unwrap();
        assert_eq!(
            statements[0].to_string(),
            "CREATE TABLE t (timestamp INT, [timestamp2] TIMESTAMP)"
        );
    }
}
