//! Column-level `[CONSTRAINT name] FOREIGN KEY REFERENCES t [(column)]`.
//!
//! SQL Server accepts the words `FOREIGN KEY` before a column's REFERENCES
//! clause, and `NOT FOR REPLICATION` after it. The built-in column option
//! parser reads neither, and the column option hook belongs to the shared
//! dialect. CREATE TABLE and ALTER TABLE statements that use them are parsed
//! again here from their tokens without those words, which yields the same
//! statement as the plain REFERENCES form. Table-level `FOREIGN KEY` always
//! has a column list, so `FOREIGN KEY REFERENCES` only occurs in a column
//! definition. Replication does not exist here and table-level `NOT FOR
//! REPLICATION` is ignored too.
use super::{peek_nth_word, peek_word};
use sqlparser::{
    ast::Statement,
    parser::{Parser, ParserError},
    tokenizer::{Token, TokenWithSpan},
};

/// Bound on the tokens scanned to recognize a statement.
const LIMIT: usize = 1_000_000;

/// Words that begin the statement after an ALTER TABLE statement. UPDATE and
/// DELETE also follow `ON` in referential actions.
const NEXT: &[&str] = &[
    "ALTER",
    "CREATE",
    "DROP",
    "INSERT",
    "UPDATE",
    "DELETE",
    "SELECT",
    "EXEC",
    "EXECUTE",
    "DECLARE",
    "IF",
    "BEGIN",
    "END",
    "WHILE",
    "PRINT",
    "MERGE",
    "TRUNCATE",
    "RETURN",
    "RAISERROR",
    "THROW",
    "USE",
];

fn is_word(token: &Token, word: &str) -> bool {
    matches!(token, Token::Word(w) if w.quote_style.is_none() && w.value.eq_ignore_ascii_case(word))
}

/// The non-whitespace tokens of the statement at the parser's position, with
/// their raw offsets. A CREATE TABLE statement is scanned to the end of its
/// column list, an ALTER TABLE statement to where another statement begins.
fn statement_tokens(parser: &Parser, create: bool) -> Vec<(usize, Token)> {
    let mut tokens: Vec<(usize, Token)> = Vec::new();
    let mut depth = 0usize;
    for offset in 0..LIMIT {
        let token = parser.peek_nth_token_no_skip(offset).token;
        match &token {
            Token::EOF | Token::SemiColon => break,
            Token::Whitespace(_) => continue,
            Token::LParen => depth += 1,
            Token::RParen => depth = depth.saturating_sub(1),
            _ => {}
        }
        if !create
            && depth == 0
            && !tokens.is_empty()
            && NEXT.iter().any(|word| is_word(&token, word))
            && !tokens.last().is_some_and(|(_, last)| is_word(last, "ON"))
        {
            break;
        }
        let closed = create && depth == 0 && matches!(token, Token::RParen);
        tokens.push((offset, token));
        if closed {
            break;
        }
    }
    tokens
}

/// The index after `REFERENCES name [(columns)] [ON {DELETE | UPDATE}
/// action]...` starting at `index`, the index of the name.
fn reference_end(tokens: &[(usize, Token)], mut index: usize) -> usize {
    let at = |index: usize| tokens.get(index).map(|(_, token)| token);
    let word = |index: usize, word: &str| at(index).is_some_and(|token| is_word(token, word));
    // A one to three part name.
    while matches!(at(index), Some(Token::Word(_))) {
        index += 1;
        if matches!(at(index), Some(Token::Period)) {
            index += 1;
        } else {
            break;
        }
    }
    if matches!(at(index), Some(Token::LParen)) {
        while at(index).is_some_and(|token| !matches!(token, Token::RParen)) {
            index += 1;
        }
        index += 1;
    }
    while word(index, "ON") && (word(index + 1, "DELETE") || word(index + 1, "UPDATE")) {
        index += 2;
        if word(index, "CASCADE") {
            index += 1;
        } else if word(index, "NO") || word(index, "SET") {
            index += 2;
        }
    }
    index
}

/// The raw offsets, relative to the parser's position, of the tokens to drop
/// from the statement that starts there, in ascending order. Empty when the
/// statement has no column-level form to rewrite.
fn dropped(parser: &Parser, create: bool) -> Vec<usize> {
    let tokens = statement_tokens(parser, create);
    let word = |index: usize, word: &str| {
        tokens
            .get(index)
            .is_some_and(|(_, token)| is_word(token, word))
    };
    let mut drop = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        if word(index, "REFERENCES") {
            // Any REFERENCES clause may end with NOT FOR REPLICATION.
            index = reference_end(&tokens, index + 1);
            if word(index, "NOT") && word(index + 1, "FOR") && word(index + 2, "REPLICATION") {
                drop.extend(tokens[index].0..=tokens[index + 2].0);
                index += 3;
            }
            continue;
        }
        if !(word(index, "FOREIGN") && word(index + 1, "KEY") && word(index + 2, "REFERENCES")) {
            index += 1;
            continue;
        }
        // A table-level constraint, which needs a column list, keeps its
        // error: there the words follow `(`, `,` or `ADD`, possibly with
        // `CONSTRAINT name` between.
        let mut before = index;
        if before >= 2 && word(before - 2, "CONSTRAINT") {
            before -= 2;
        }
        let table_level = before == 0
            || matches!(tokens[before - 1].1, Token::Comma | Token::LParen)
            || word(before - 1, "ADD");
        if !table_level {
            drop.extend(tokens[index].0..=tokens[index + 1].0);
        }
        index += 2;
    }
    drop
}

/// Parse a CREATE TABLE or ALTER TABLE statement with column-level
/// `FOREIGN KEY REFERENCES`, or decline without consuming tokens.
pub fn parse(parser: &mut Parser) -> Option<Result<Statement, ParserError>> {
    let create = peek_word(parser, "CREATE") && peek_nth_word(parser, 1, "TABLE");
    let alter = peek_word(parser, "ALTER") && peek_nth_word(parser, 1, "TABLE");
    if !(create || alter) {
        return None;
    }
    let drop = dropped(parser, create);
    if drop.is_empty() {
        return None;
    }
    // The rest of the batch without the dropped words; `origin` maps each
    // kept token to its raw offset.
    let mut tokens: Vec<TokenWithSpan> = Vec::new();
    let mut origin = Vec::new();
    let mut offset = 0;
    loop {
        let token = parser.peek_nth_token_no_skip(offset);
        if token.token == Token::EOF {
            break;
        }
        if drop.binary_search(&offset).is_err() {
            tokens.push(token);
            origin.push(offset);
        }
        offset += 1;
    }
    let total = offset;
    let mut rewritten =
        Parser::new(&crate::dialect::ServerDialect).with_tokens_with_locations(tokens);
    let statement = match rewritten.parse_statement() {
        Ok(statement) => statement,
        Err(error) => return Some(Err(error)),
    };
    // Dropped words at the end of the statement are consumed with it.
    while matches!(rewritten.peek_token_no_skip().token, Token::Whitespace(_)) {
        rewritten.next_token_no_skip();
    }
    let consumed = origin.get(rewritten.index()).copied().unwrap_or(total);
    if drop.first().is_none_or(|first| *first >= consumed) {
        // The words belong to a later statement.
        return None;
    }
    for _ in 0..consumed {
        parser.next_token_no_skip();
    }
    Some(Ok(statement))
}
