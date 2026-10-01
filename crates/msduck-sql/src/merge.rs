//! Parse-time MERGE validation, before any batch statements can execute.
use sqlparser::{
    ast::*,
    parser::{Parser, ParserError},
    tokenizer::{Token, TokenWithSpan},
};

#[path = "merge_top.rs"]
pub mod top;

const TERMINATOR: &str = "A MERGE statement must be terminated by a semi-colon (;).";
const HINT_AFTER_ALIAS: &str = "Incorrect syntax near the keyword 'WITH'.";
const TARGET_NOLOCK: &str = "The NOLOCK and READUNCOMMITTED lock hints are not allowed for target tables of INSERT, UPDATE, DELETE or MERGE statements.";
/// Prefix of the optimizer hint that carries `MERGE TOP`, which the sqlparser
/// `Merge` AST cannot represent.
const TOP_HINT: &str = "msduck_merge_top";

/// Parse `MERGE [TOP (n) [PERCENT]] [INTO] target [WITH (hints)] [[AS] alias]
/// USING ...;`. sqlparser accepts target hints only after the alias, so a
/// hint list written in SQL Server's position is moved behind the alias
/// before the remaining tokens are handed to sqlparser.
pub fn parse(parser: &mut Parser) -> Result<Statement, ParserError> {
    let token = parser.next_token();
    let top = top::parse_after_merge(parser)?;
    // Every token up to the statement's terminating semicolon (or the end of
    // the batch). MERGE requires the semicolon, so it bounds the statement.
    let mut tokens = Vec::new();
    let mut depth = 0usize;
    loop {
        let next = parser.peek_token();
        match next.token {
            Token::EOF => break,
            Token::SemiColon if depth == 0 => break,
            Token::LParen => depth += 1,
            Token::RParen => depth = depth.saturating_sub(1),
            _ => {}
        }
        tokens.push(parser.next_token());
    }
    let moved = move_target_hints(&mut tokens);
    default_values(&mut tokens);
    let count = tokens.len();
    let mut inner = Parser::new(&crate::dialect::ServerDialect).with_tokens_with_locations(tokens);
    let mut merge = inner.parse_merge(token)?;
    if inner.index() < count {
        // Trailing tokens that are not part of the MERGE: SQL Server reports
        // the missing terminator, as the token-level check did before.
        return Err(ParserError::ParserError(TERMINATOR.into()));
    }
    // SQL Server accepts target hints only before the alias.
    if !moved
        && matches!(&merge.table, TableFactor::Table { alias: Some(_), with_hints, .. } if !with_hints.is_empty())
    {
        return Err(ParserError::ParserError(HINT_AFTER_ALIAS.into()));
    }
    if let Some(top) = top {
        merge.optimizer_hints.push(OptimizerHint {
            prefix: TOP_HINT.into(),
            text: top.to_string(),
            style: OptimizerHintStyle::MultiLine,
        });
    }
    finish(parser, &merge)?;
    Ok(Statement::Merge(merge))
}

/// The `TOP` clause of a parsed MERGE, if any.
pub fn top_clause(merge: &Merge) -> Result<Option<Top>, ParserError> {
    let Some(hint) = merge.optimizer_hints.iter().find(|h| h.prefix == TOP_HINT) else {
        return Ok(None);
    };
    let mut parser = Parser::new(&crate::dialect::ServerDialect).try_with_sql(&hint.text)?;
    top::parse_after_merge(&mut parser)
}

/// The marker that `INSERT DEFAULT VALUES` becomes, since sqlparser's MERGE
/// grammar has no such form: `INSERT VALUES (__msduck_default_values)`.
pub const DEFAULT_VALUES: &str = "__msduck_default_values";

/// Whether a MERGE INSERT row is the `DEFAULT VALUES` marker.
pub fn is_default_values(columns: &[ObjectName], row: &[Expr]) -> bool {
    columns.is_empty()
        && matches!(row, [Expr::Identifier(id)] if id.quote_style.is_none() && id.value == DEFAULT_VALUES)
}

fn default_values(tokens: &mut Vec<TokenWithSpan>) {
    let mut index = 0;
    while index + 3 < tokens.len() {
        if is_word(&tokens[index].token, "THEN")
            && is_word(&tokens[index + 1].token, "INSERT")
            && is_word(&tokens[index + 2].token, "DEFAULT")
            && is_word(&tokens[index + 3].token, "VALUES")
        {
            let (default, values) = (tokens[index + 2].span, tokens[index + 3].span);
            tokens.splice(
                index + 2..index + 4,
                [
                    TokenWithSpan::new(Token::make_keyword("VALUES"), default),
                    TokenWithSpan::new(Token::LParen, values),
                    TokenWithSpan::new(Token::make_word(DEFAULT_VALUES, None), values),
                    TokenWithSpan::new(Token::RParen, values),
                ],
            );
            index += 6;
        } else {
            index += 1;
        }
    }
}

fn is_word(token: &Token, word: &str) -> bool {
    matches!(token, Token::Word(w) if w.quote_style.is_none() && w.value.eq_ignore_ascii_case(word))
}

/// Rewrite `[INTO] name WITH ( ... ) [AS] alias USING` as
/// `[INTO] name [AS] alias WITH ( ... ) USING`; true when hints moved.
fn move_target_hints(tokens: &mut Vec<TokenWithSpan>) -> bool {
    let mut at = usize::from(tokens.first().is_some_and(|t| is_word(&t.token, "INTO")));
    // A one- to four-part object name.
    let mut parts = 0;
    while matches!(tokens.get(at).map(|t| &t.token), Some(Token::Word(_))) {
        parts += 1;
        at += 1;
        if tokens.get(at).map(|t| &t.token) != Some(&Token::Period) {
            break;
        }
        at += 1;
    }
    if parts == 0
        || !tokens.get(at).is_some_and(|t| is_word(&t.token, "WITH"))
        || tokens.get(at + 1).map(|t| &t.token) != Some(&Token::LParen)
    {
        return false;
    }
    let start = at;
    let mut depth = 0usize;
    let mut end = None;
    for (index, token) in tokens.iter().enumerate().skip(at + 1) {
        match token.token {
            Token::LParen => depth += 1,
            Token::RParen => {
                depth -= 1;
                if depth == 0 {
                    end = Some(index);
                    break;
                }
            }
            _ => {}
        }
    }
    let Some(end) = end else {
        return false;
    };
    let mut alias = end + 1;
    if tokens.get(alias).is_some_and(|t| is_word(&t.token, "AS")) {
        alias += 1;
    }
    match tokens.get(alias).map(|t| &t.token) {
        Some(Token::Word(w))
            if w.quote_style.is_some() || !w.value.eq_ignore_ascii_case("USING") =>
        {
            alias += 1;
        }
        // Hints without an alias are already where sqlparser expects them.
        _ => return true,
    }
    let hints: Vec<_> = tokens.drain(start..=end).collect();
    let insert_at = alias - hints.len();
    tokens.splice(insert_at..insert_at, hints);
    true
}

pub fn finish(parser: &Parser, merge: &Merge) -> Result<(), ParserError> {
    // Leave the terminator for the enclosing batch/block parser. Comments and
    // whitespace are skipped by peek_token; punctuation inside strings is not.
    if parser.peek_token().token != Token::SemiColon {
        return Err(ParserError::ParserError(TERMINATOR.into()));
    }
    validate(merge).map_err(ParserError::ParserError)
}

fn family(kind: &MergeClauseKind) -> usize {
    match kind {
        MergeClauseKind::Matched => 0,
        MergeClauseKind::NotMatched | MergeClauseKind::NotMatchedByTarget => 1,
        MergeClauseKind::NotMatchedBySource => 2,
    }
}

fn validate(merge: &Merge) -> Result<(), String> {
    if let TableFactor::Table { with_hints, .. } = &merge.table
        && with_hints.iter().any(|hint| {
            matches!(hint, Expr::Identifier(id) if id.value.eq_ignore_ascii_case("NOLOCK") || id.value.eq_ignore_ascii_case("READUNCOMMITTED"))
        })
    {
        return Err(TARGET_NOLOCK.into());
    }
    let labels = [
        "WHEN MATCHED",
        "WHEN NOT MATCHED",
        "WHEN NOT MATCHED BY SOURCE",
    ];
    let mut seen = [[false; 3]; 3];
    for clause in &merge.clauses {
        let (action, name) = match clause.action {
            MergeAction::Insert(_) => (0, "INSERT"),
            MergeAction::Update(_) => (1, "UPDATE"),
            MergeAction::Delete { .. } => (2, "DELETE"),
            MergeAction::DoNothing { .. } => {
                return Err("unsupported MERGE DO NOTHING action".into());
            }
        };
        let kind = family(&clause.clause_kind);
        if seen[kind][action] {
            // SQL Server's message names the clause first and the action second.
            return Err(format!(
                "An action of type '{}' cannot appear more than once in a '{name}' clause of a MERGE statement.",
                labels[kind]
            ));
        }
        seen[kind][action] = true;
    }
    let mut unconditional = [false; 3];
    for clause in &merge.clauses {
        let kind = family(&clause.clause_kind);
        if unconditional[kind] {
            return Err(format!(
                "In a MERGE statement, a '{}' clause with a search condition cannot appear after a '{}' clause with no search condition.",
                labels[kind], labels[kind]
            ));
        }
        unconditional[kind] = clause.predicate.is_none();
    }
    Ok(())
}

pub fn error_number(message: &str) -> Option<i32> {
    let message = message
        .strip_prefix("sql parser error: ")
        .unwrap_or(message);
    if message == TERMINATOR {
        Some(10713)
    } else if message == TARGET_NOLOCK {
        Some(1065)
    } else if message == HINT_AFTER_ALIAS {
        Some(156)
    } else if message.starts_with("An action of type '")
        && message.ends_with("clause of a MERGE statement.")
    {
        Some(10714)
    } else if message.starts_with("In a MERGE statement, a '")
        && message.ends_with("clause with no search condition.")
    {
        Some(5324)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn terminator_is_a_token_and_valid_arms_parse() {
        let head = "MERGE t USING s ON t.id=s.id ";
        for tail in [
            "WHEN MATCHED THEN DELETE;",
            "WHEN MATCHED AND s.id>0 THEN UPDATE SET n=1 WHEN MATCHED THEN DELETE;",
            "WHEN NOT MATCHED BY SOURCE AND t.id>0 THEN DELETE WHEN NOT MATCHED BY SOURCE THEN UPDATE SET n=1;",
            "WHEN NOT MATCHED THEN INSERT(n) VALUES (';') /* ; */ ;",
        ] {
            assert!(
                Parser::parse_sql(&crate::dialect::ServerDialect, &format!("{head}{tail}")).is_ok(),
                "{tail}"
            );
        }
        for tail in [
            "WHEN MATCHED THEN DELETE",
            "WHEN MATCHED THEN DELETE -- ;",
            "WHEN NOT MATCHED THEN INSERT(n) VALUES (';')",
        ] {
            let error = Parser::parse_sql(&crate::dialect::ServerDialect, &format!("{head}{tail}"))
                .unwrap_err();
            assert_eq!(error_number(&error.to_string()), Some(10713));
        }
    }
}
