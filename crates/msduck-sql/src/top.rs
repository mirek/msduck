//! Give each set-operation branch its own query scope for TOP lowering.
use sqlparser::ast::*;

pub fn wrap_set_branches(body: &mut SetExpr) {
    if let SetExpr::SetOperation { left, right, .. } = body {
        wrap_branch(left);
        wrap_branch(right);
    }
}

fn wrap_branch(body: &mut SetExpr) {
    if matches!(body, SetExpr::Select(select) if select.top.is_some()) {
        *body = SetExpr::Query(Box::new(Query {
            with: None,
            body: Box::new(body.clone()),
            order_by: None,
            limit_clause: None,
            fetch: None,
            locks: vec![],
            for_clause: None,
            settings: None,
            format_clause: None,
            pipe_operators: vec![],
        }));
    } else {
        // Existing query scopes are visited separately by Translator.
        wrap_set_branches(body);
    }
}

pub fn paging(query: &mut Query) -> Result<(), String> {
    let offset = match &mut query.limit_clause {
        Some(LimitClause::LimitOffset { offset, .. }) => offset.as_mut(),
        _ => None,
    };
    if offset.is_none() && query.fetch.is_none() {
        return Ok(());
    }
    if query.order_by.is_none() {
        return Err("OFFSET/FETCH requires ORDER BY".into());
    }
    if query.fetch.is_some() && offset.is_none() {
        return Err("FETCH requires OFFSET".into());
    }
    if let Some(offset) = offset {
        offset.value = checked_count(offset.value.clone(), "__msduck_offset_count");
    }
    if let Some(fetch) = &mut query.fetch {
        if fetch.percent || fetch.with_ties {
            return Err("unsupported FETCH PERCENT/WITH TIES".into());
        }
        let value = fetch.quantity.take().ok_or("FETCH requires a count")?;
        fetch.quantity = Some(checked_count(value, "__msduck_fetch_count"));
    }
    Ok(())
}

fn checked_count(value: Expr, function: &str) -> Expr {
    use crate::expr::{binary_function, unary_function};
    let invalid = unary_function(
        "error",
        Expr::Value(
            Value::SingleQuotedString("A TOP or FETCH clause contains an invalid value.".into())
                .into(),
        ),
    );
    unary_function(function, binary_function("coalesce", value, invalid))
}

/// sqlparser's FETCH parser only accepts Value nodes. Parse the count expression
/// ourselves, then restore its AST after the surrounding statement is parsed.
pub fn fetch_tokens(
    tokens: Vec<sqlparser::tokenizer::TokenWithSpan>,
) -> Result<(Vec<sqlparser::tokenizer::TokenWithSpan>, Vec<Expr>), sqlparser::parser::ParserError> {
    use sqlparser::{
        keywords::Keyword,
        parser::Parser,
        tokenizer::{Token, TokenWithSpan},
    };
    let mut tokens = tokens;
    let mut expressions = Vec::new();
    // Work inside out: parsing an outer count must see normalized FETCH
    // clauses inside its scalar subqueries. Replacements never move earlier
    // token positions, so this reverse walk keeps its indexes valid.
    for index in (0..tokens.len()).rev() {
        if matches!(&tokens[index].token, Token::Word(w) if w.keyword == Keyword::FETCH) {
            let mut parser = Parser::new(&crate::dialect::ServerDialect)
                .with_tokens_with_locations(tokens[index + 1..].to_vec());
            parser.expect_one_of_keywords(&[Keyword::FIRST, Keyword::NEXT])?;
            let expression = parser.parse_expr()?;
            parser.expect_one_of_keywords(&[Keyword::ROW, Keyword::ROWS])?;
            parser.expect_keyword(Keyword::ONLY)?;
            let consumed = parser.index();
            let normalized = "FETCH NEXT ? ROWS ONLY";
            let mut replacement =
                sqlparser::tokenizer::Tokenizer::new(&crate::dialect::ServerDialect, normalized)
                    .tokenize_with_location()?;
            for token in &mut replacement {
                if matches!(token.token, Token::Placeholder(_)) {
                    token.token = Token::Placeholder(format!("msduck:fetch:{}", expressions.len()));
                }
            }
            expressions.push(expression);
            let span = tokens[index].span;
            let replacement = replacement.into_iter().map(|token| TokenWithSpan {
                token: token.token,
                span,
            });
            tokens.splice(index..index + consumed + 1, replacement);
        }
    }
    Ok((tokens, expressions))
}

pub fn restore_fetch(statement: &mut Statement, expressions: &[Expr]) {
    struct Restore<'a>(&'a [Expr]);
    impl VisitorMut for Restore<'_> {
        type Break = ();
        fn pre_visit_query(&mut self, query: &mut Query) -> std::ops::ControlFlow<()> {
            if let Some(fetch) = &mut query.fetch
                && let Some(Expr::Value(value)) = &fetch.quantity
                && let Value::Placeholder(marker) = &value.value
                && let Some(index) = marker
                    .strip_prefix("msduck:fetch:")
                    .and_then(|n| n.parse::<usize>().ok())
                && let Some(expression) = self.0.get(index)
            {
                fetch.quantity = Some(expression.clone());
            }
            std::ops::ControlFlow::Continue(())
        }
    }
    let _ = VisitMut::visit(statement, &mut Restore(expressions));
}
