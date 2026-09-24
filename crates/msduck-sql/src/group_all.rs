//! Token marker for SQL Server's explicit GROUP BY ALL column list.
use sqlparser::{
    ast::*,
    keywords::Keyword,
    tokenizer::{Token, TokenWithSpan},
};
pub const MARKER: &str = "msduck:group-all";
pub fn tokens(tokens: Vec<TokenWithSpan>) -> Vec<TokenWithSpan> {
    let mut out = vec![];
    let mut previous = [Keyword::NoKeyword; 2];
    for mut token in tokens {
        if matches!(token.token, Token::Whitespace(_)) {
            out.push(token);
            continue;
        }
        let keyword = match &token.token {
            Token::Word(w) if w.quote_style.is_none() => w.keyword,
            _ => Keyword::NoKeyword,
        };
        if previous == [Keyword::GROUP, Keyword::BY] && keyword == Keyword::ALL {
            token.token = Token::Placeholder(MARKER.into());
            out.push(token.clone());
            token.token = Token::Comma;
        }
        out.push(token);
        previous = [previous[1], keyword];
    }
    out
}
pub fn marked(select: &Select) -> bool {
    matches!(&select.group_by, GroupByExpr::Expressions(values, _) if matches!(values.first(),Some(Expr::Value(v)) if matches!(&v.value, Value::Placeholder(p) if p==MARKER)))
}
