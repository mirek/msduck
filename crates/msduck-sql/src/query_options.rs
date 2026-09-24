//! Statement-local query hints carried through sqlparser's extensible settings slot.
use sqlparser::{
    ast::*,
    keywords::Keyword,
    parser::{Parser, ParserError},
    tokenizer::Token,
};
const KEY: &str = "msduck:maxrecursion";

pub fn parse(parser: &mut Parser, query: &mut Query) -> Result<(), ParserError> {
    if !parser.parse_keyword(Keyword::OPTION) {
        return Ok(());
    }
    parser.expect_token(&Token::LParen)?;
    let token = parser.next_token();
    if !matches!(&token.token,Token::Word(word) if word.quote_style.is_none() && word.value.eq_ignore_ascii_case("MAXRECURSION"))
    {
        return Err(ParserError::ParserError(
            "unsupported query hint; expected MAXRECURSION".into(),
        ));
    }
    let token = parser.next_token();
    let Token::Number(value, false) = token.token else {
        return Err(ParserError::ParserError(
            "MAXRECURSION requires an unsigned integer literal".into(),
        ));
    };
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ParserError::ParserError(
            "MAXRECURSION requires an unsigned integer literal".into(),
        ));
    }
    let limit=value.parse::<u16>().ok().filter(|n|*n<=32767).ok_or_else(||ParserError::ParserError(format!("The value {value} specified for the MAXRECURSION option exceeds the allowed maximum of 32767.")))?;
    parser.expect_token(&Token::RParen)?;
    // The colon-bearing unquoted key cannot originate from a T-SQL identifier.
    // It is consumed before backend SQL serialization, like dialect loop markers.
    set(query, limit);
    Ok(())
}
pub fn set(query: &mut Query, limit: u16) {
    assert!(limit <= 32767, "validated recursion limit");
    query.settings.get_or_insert_default().push(Setting {
        key: Ident::new(KEY),
        value: crate::expr::number(limit),
    });
}
pub fn has(query: &Query) -> bool {
    query.settings.as_ref().is_some_and(|settings| {
        settings
            .iter()
            .any(|s| s.key.value == KEY && s.key.quote_style.is_none())
    })
}
pub fn take(query: &mut Query) -> Option<u16> {
    let settings = query.settings.as_mut()?;
    let index = settings
        .iter()
        .position(|s| s.key.value == KEY && s.key.quote_style.is_none())?;
    let setting = settings.remove(index);
    if settings.is_empty() {
        query.settings = None;
    }
    let Expr::Value(value) = setting.value else {
        unreachable!("validated query hint")
    };
    let Value::Number(value, false) = value.value else {
        unreachable!("validated query hint")
    };
    Some(value.parse().expect("validated recursion limit"))
}
pub fn error_number(message: &str) -> Option<i32> {
    let message = message
        .strip_prefix("sql parser error: ")
        .unwrap_or(message);
    (message.starts_with("The value ")
        && message.ends_with(
            " specified for the MAXRECURSION option exceeds the allowed maximum of 32767.",
        ))
    .then_some(310)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hints_are_literal_bounded_and_removable_ast_inputs() {
        for value in [0, 1, 100, 32767] {
            let sql = format!(
                "WITH r(n) AS (SELECT 1) SELECT n FROM r OPTION (MAXRECURSION {value}); SELECT 2"
            );
            let mut statements = Parser::parse_sql(&crate::dialect::ServerDialect, &sql).unwrap();
            assert_eq!(statements.len(), 2);
            let Statement::Query(query) = &mut statements[0] else {
                panic!()
            };
            assert!(has(query));
            assert_eq!(take(query), Some(value));
            assert!(!has(query));
            assert!(!query.to_string().contains("SETTINGS"));
        }
        for value in [
            "-1",
            "1.5",
            "@n",
            "'2'",
            "32768",
            "999999999999999999999999",
        ] {
            let error = Parser::parse_sql(
                &crate::dialect::ServerDialect,
                &format!("SELECT 1 OPTION (MAXRECURSION {value})"),
            )
            .unwrap_err();
            if value.bytes().all(|b| b.is_ascii_digit()) {
                assert_eq!(error_number(&error.to_string()), Some(310));
            }
        }
    }
}
