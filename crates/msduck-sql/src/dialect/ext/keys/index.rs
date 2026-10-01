//! `CREATE [UNIQUE] [CLUSTERED | NONCLUSTERED] INDEX` in T-SQL clause order.
//!
//! sqlparser's own CREATE INDEX follows PostgreSQL, which expects WITH before
//! WHERE and has no CLUSTERED keyword or ON filegroup clause. This parser
//! accepts:
//!
//! ```text
//! CREATE [UNIQUE] [CLUSTERED | NONCLUSTERED] INDEX name ON table
//!     (column [ASC | DESC] [, ...])
//!     [INCLUDE (column [, ...])]
//!     [WHERE filter_predicate]
//!     [WITH (option = value [, ...]) | WITH option [= value] [, ...]]
//!     [ON filegroup | ON partition_scheme (column) | ON "default"]
//!     [FILESTREAM_ON ...]
//! ```
//!
//! CLUSTERED is kept as `using = CLUSTERED`, and each WITH option as an
//! `option = value` expression, so the runtime can apply them.
use sqlparser::{
    ast::*,
    keywords::Keyword,
    parser::{Parser, ParserError},
    tokenizer::Token,
};

const CLUSTERED: &str = "CLUSTERED";

fn is_word(token: &Token, value: &str) -> bool {
    matches!(token, Token::Word(w) if w.quote_style.is_none() && w.value.eq_ignore_ascii_case(value))
}

/// Whether the parser is at `CREATE [UNIQUE] [CLUSTERED|NONCLUSTERED] INDEX`.
pub fn starts(parser: &Parser<'_>) -> bool {
    if !parser.peek_keyword(Keyword::CREATE) {
        return false;
    }
    let mut n = 1;
    if matches!(&parser.peek_nth_token(n).token, Token::Word(w) if w.keyword == Keyword::UNIQUE) {
        n += 1;
    }
    let token = parser.peek_nth_token(n).token;
    if is_word(&token, "CLUSTERED") || is_word(&token, "NONCLUSTERED") {
        n += 1;
    }
    matches!(&parser.peek_nth_token(n).token, Token::Word(w) if w.keyword == Keyword::INDEX)
}

/// Whether a parsed index was declared CLUSTERED.
pub fn clustered(index: &CreateIndex) -> bool {
    matches!(&index.using, Some(IndexType::Custom(name)) if name.value == CLUSTERED)
}

/// The WITH options as upper-cased `(name, value)` pairs.
pub fn options(index: &CreateIndex) -> Vec<(String, String)> {
    index
        .with
        .iter()
        .filter_map(|option| match option {
            Expr::BinaryOp {
                left,
                op: BinaryOperator::Eq,
                right,
            } => {
                let Expr::Identifier(name) = left.as_ref() else {
                    return None;
                };
                let value = match right.as_ref() {
                    Expr::Identifier(value) => value.value.to_ascii_uppercase(),
                    Expr::Value(value) => value.value.to_string(),
                    other => other.to_string().to_ascii_uppercase(),
                };
                Some((name.value.to_ascii_uppercase(), value))
            }
            _ => None,
        })
        .collect()
}

fn word_value(parser: &mut Parser<'_>) -> Result<Expr, ParserError> {
    let token = parser.next_token();
    Ok(match token.token {
        Token::Number(value, long) => Expr::Value(Value::Number(value, long).into()),
        Token::Word(w) => Expr::Identifier(Ident::new(w.value.to_ascii_uppercase())),
        other => {
            return Err(ParserError::ParserError(format!(
                "Incorrect syntax near '{other}'."
            )));
        }
    })
}

/// Skip a balanced parenthesized group if one follows.
fn skip_group(parser: &mut Parser<'_>) -> Result<(), ParserError> {
    if parser.peek_token().token != Token::LParen {
        return Ok(());
    }
    let mut depth = 0usize;
    loop {
        match parser.next_token().token {
            Token::LParen => depth += 1,
            Token::RParen => {
                depth -= 1;
                if depth == 0 {
                    return Ok(());
                }
            }
            Token::EOF => {
                return Err(ParserError::ParserError(
                    "Incorrect syntax near ')'.".into(),
                ));
            }
            _ => {}
        }
    }
}

fn option(parser: &mut Parser<'_>, parenthesized: bool) -> Result<Expr, ParserError> {
    let name = parser.parse_identifier()?;
    let value = if parser.consume_token(&Token::Eq) {
        let value = word_value(parser)?;
        // ONLINE = ON (WAIT_AT_LOW_PRIORITY (...)) and
        // DATA_COMPRESSION = PAGE ON PARTITIONS (...).
        if parenthesized && parser.parse_keyword(Keyword::ON) {
            parser.parse_identifier()?;
        }
        skip_group(parser)?;
        value
    } else if parenthesized {
        return parser.expected("=", parser.peek_token());
    } else {
        // Legacy `WITH PAD_INDEX, IGNORE_DUP_KEY`.
        Expr::Identifier(Ident::new("ON"))
    };
    Ok(Expr::BinaryOp {
        left: Box::new(Expr::Identifier(Ident::new(
            name.value.to_ascii_uppercase(),
        ))),
        op: BinaryOperator::Eq,
        right: Box::new(value),
    })
}

pub fn parse_index(parser: &mut Parser<'_>) -> Result<Statement, ParserError> {
    parser.expect_keyword(Keyword::CREATE)?;
    let unique = parser.parse_keyword(Keyword::UNIQUE);
    let token = parser.peek_token().token;
    let clustered = if is_word(&token, CLUSTERED) {
        parser.next_token();
        true
    } else {
        if is_word(&token, "NONCLUSTERED") {
            parser.next_token();
        }
        false
    };
    parser.expect_keyword(Keyword::INDEX)?;
    let name = parser.parse_identifier()?;
    parser.expect_keyword(Keyword::ON)?;
    let table_name = parser.parse_object_name(false)?;
    parser.expect_token(&Token::LParen)?;
    let columns = parser.parse_comma_separated(|parser| {
        let expr = Expr::Identifier(parser.parse_identifier()?);
        let sort = if parser.parse_keyword(Keyword::ASC) {
            Some(OrderBySort::Asc)
        } else if parser.parse_keyword(Keyword::DESC) {
            Some(OrderBySort::Desc)
        } else {
            None
        };
        Ok(IndexColumn {
            column: OrderByExpr {
                expr,
                options: OrderByOptions {
                    sort,
                    nulls_first: None,
                },
                with_fill: None,
            },
            operator_class: None,
        })
    })?;
    parser.expect_token(&Token::RParen)?;
    let include = if parser.parse_keyword(Keyword::INCLUDE) {
        parser.expect_token(&Token::LParen)?;
        let include = parser.parse_comma_separated(|parser| parser.parse_identifier())?;
        parser.expect_token(&Token::RParen)?;
        include
    } else {
        vec![]
    };
    let predicate = if parser.parse_keyword(Keyword::WHERE) {
        Some(parser.parse_expr()?)
    } else {
        None
    };
    let mut with = vec![];
    if parser.parse_keyword(Keyword::WITH) {
        if parser.consume_token(&Token::LParen) {
            with = parser.parse_comma_separated(|parser| option(parser, true))?;
            parser.expect_token(&Token::RParen)?;
        } else {
            with = parser.parse_comma_separated(|parser| option(parser, false))?;
        }
    }
    // Storage placement has no effect on a DuckDB table.
    if parser.parse_keyword(Keyword::ON) {
        parser.next_token();
        skip_group(parser)?;
    }
    if is_word(&parser.peek_token().token, "FILESTREAM_ON") {
        parser.next_token();
        parser.next_token();
    }
    Ok(Statement::CreateIndex(CreateIndex {
        name: Some(ObjectName::from(vec![name])),
        table_name,
        using: clustered.then(|| IndexType::Custom(Ident::new(CLUSTERED))),
        columns,
        unique,
        concurrently: false,
        r#async: false,
        if_not_exists: false,
        include,
        nulls_distinct: None,
        with,
        predicate,
        index_options: vec![],
        alter_options: vec![],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(sql: &str) -> CreateIndex {
        let mut statements = crate::batch::parse(sql).unwrap();
        assert_eq!(statements.len(), 1, "{sql}");
        let Statement::CreateIndex(index) = statements.remove(0) else {
            panic!("{sql}")
        };
        index
    }

    #[test]
    fn clustered_include_filter_and_options_parse_in_tsql_order() {
        let i = index("CREATE CLUSTERED INDEX ix_items ON items(id)");
        assert!(clustered(&i) && !i.unique);
        let i = index("create unique clustered index [ux] on dbo.[items] (id desc, code asc)");
        assert!(clustered(&i) && i.unique);
        assert_eq!(i.table_name.to_string(), "dbo.[items]");
        assert_eq!(
            i.columns[0].column.options.sort,
            Some(OrderBySort::Desc),
            "descending key"
        );
        let i = index("CREATE INDEX ix ON items(id) INCLUDE(value, label)");
        assert!(!clustered(&i));
        assert_eq!(i.include.len(), 2);
        let i = index("CREATE INDEX ix ON items(id) WHERE id>0");
        assert_eq!(i.predicate.unwrap().to_string(), "id > 0");
        let i = index(
            "CREATE NONCLUSTERED INDEX ix ON items(value DESC) INCLUDE(label) WHERE value IS NOT NULL WITH (FILLFACTOR = 80, PAD_INDEX = ON, ONLINE = ON (WAIT_AT_LOW_PRIORITY (MAX_DURATION = 1 MINUTES)), DATA_COMPRESSION = PAGE ON PARTITIONS (1 TO 2), IGNORE_DUP_KEY = OFF) ON [PRIMARY]",
        );
        assert!(!clustered(&i));
        assert_eq!(
            options(&i),
            [
                ("FILLFACTOR", "80"),
                ("PAD_INDEX", "ON"),
                ("ONLINE", "ON"),
                ("DATA_COMPRESSION", "PAGE"),
                ("IGNORE_DUP_KEY", "OFF"),
            ]
            .map(|(a, b)| (a.to_owned(), b.to_owned()))
        );
        let i = index("CREATE UNIQUE INDEX ux ON t(a) WITH FILLFACTOR = 70, IGNORE_DUP_KEY");
        assert_eq!(
            options(&i),
            [("FILLFACTOR", "70"), ("IGNORE_DUP_KEY", "ON")]
                .map(|(a, b)| (a.to_owned(), b.to_owned()))
        );
        // Following statements still split without semicolons.
        let statements = crate::batch::parse(
            "CREATE INDEX ix ON t(a) WHERE a > 0 SELECT 1 CREATE INDEX iy ON t(b)",
        )
        .unwrap();
        assert_eq!(statements.len(), 3);
    }
}
