//! Token adaptation for variable OPENJSON path arguments.
// sqlparser stores OPENJSON paths as Value, although T-SQL permits variables.
// Adapt only an unquoted variable token in that argument, preserving its span.
pub fn path_tokens(tokens: &mut [sqlparser::tokenizer::TokenWithSpan]) {
    use sqlparser::{keywords::Keyword, tokenizer::Token};
    let mut stack: Vec<(bool, usize)> = Vec::new();
    let mut previous_openjson = false;
    for token in tokens {
        if matches!(token.token, Token::Whitespace(_)) {
            continue;
        }
        match &token.token {
            Token::LParen => stack.push((previous_openjson, 0)),
            Token::RParen => {
                stack.pop();
            }
            Token::Comma => {
                if let Some((_, argument)) = stack.last_mut() {
                    *argument += 1;
                }
            }
            Token::Word(word)
                if stack.last() == Some(&(true, 1))
                    && word.quote_style.is_none()
                    && word.value.starts_with('@') =>
            {
                token.token = Token::Placeholder(word.value.clone());
            }
            _ => {}
        }
        previous_openjson = matches!(&token.token, Token::Word(word) if word.quote_style.is_none() && word.keyword == Keyword::OPENJSON);
    }
}

pub fn path_variable(factor: &sqlparser::ast::TableFactor) -> Option<&str> {
    if let sqlparser::ast::TableFactor::OpenJsonTable {
        json_path: Some(path),
        ..
    } = factor
        && let sqlparser::ast::Value::Placeholder(name) = &path.value
        && name.starts_with('@')
    {
        Some(name)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use sqlparser::parser::Parser;
    #[test]
    fn path_token_adapter_respects_nested_arguments_and_quoted_text() {
        use sqlparser::tokenizer::Token;
        let sql = "DECLARE @path NVARCHAR(20); SELECT '@path, OPENJSON(@x,@y)',@path FROM OPENJSON(COALESCE(@doc,N'[]'), /* comment */ @path) j CROSS APPLY OPENJSON(j.value,@other) k";
        let tokens = crate::dialect::tokenize(sql).unwrap();
        let names = tokens
            .iter()
            .filter_map(|t| {
                if let Token::Placeholder(name) = &t.token {
                    Some(name.as_str())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["@path", "@other"]);
        for token in tokens
            .iter()
            .filter(|t| matches!(t.token, Token::Placeholder(_)))
        {
            assert_eq!(token.span.start.line, 1);
            assert!(token.span.end.column > token.span.start.column);
        }
        let statements = Parser::new(&crate::dialect::ServerDialect)
            .with_tokens_with_locations(tokens)
            .parse_statements()
            .unwrap();
        assert_eq!(statements.len(), 2);
    }
}
