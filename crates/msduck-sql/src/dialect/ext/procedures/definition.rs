//! The header of `CREATE [OR ALTER] PROC[EDURE]` and `ALTER PROC[EDURE]`.
use msduck_core::diagnostic::SqlError;
use sqlparser::{
    ast::{DataType, Expr, ObjectNamePart},
    parser::Parser,
    tokenizer::{Location, Token, TokenWithSpan},
};

/// One declared parameter.
#[derive(Clone, Debug, PartialEq)]
pub struct Parameter {
    /// The name as written, including `@`.
    pub name: String,
    pub data_type: DataType,
    pub output: bool,
    /// The default value and its source text.
    pub default: Option<(Expr, String)>,
}

/// A parsed procedure header; the body is `sql[body..]`.
#[derive(Clone, Debug, PartialEq)]
pub struct Definition {
    /// `CREATE` (possibly `CREATE OR ALTER`).
    pub create: bool,
    /// `ALTER` (possibly `CREATE OR ALTER`).
    pub alter: bool,
    pub schema: Option<String>,
    pub name: String,
    pub parameters: Vec<Parameter>,
    /// Byte offset of the body, just after `AS`.
    pub body: usize,
}

/// Byte offset of a tokenizer location (1-based line and character column).
pub fn offset(sql: &str, location: Location) -> usize {
    let mut line = 1;
    let mut column = 1;
    for (index, character) in sql.char_indices() {
        if line == location.line && column == location.column {
            return index;
        }
        if character == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    sql.len()
}

fn syntax(token: &TokenWithSpan) -> SqlError {
    let message = match &token.token {
        Token::EOF => "Incorrect syntax near the end of the batch.".to_string(),
        Token::Word(word)
            if word.quote_style.is_none()
                && word.keyword != sqlparser::keywords::Keyword::NoKeyword =>
        {
            format!("Incorrect syntax near the keyword '{}'.", word.value)
        }
        Token::Word(word) => format!("Incorrect syntax near '{}'.", word.value),
        token => format!("Incorrect syntax near '{token}'."),
    };
    let number = if message.contains("the keyword") {
        156
    } else {
        102
    };
    SqlError::syntax(number, 1, message)
}

fn is_word(token: &Token, value: &str) -> bool {
    matches!(token, Token::Word(w) if w.quote_style.is_none() && w.value.eq_ignore_ascii_case(value))
}

/// Parse the header of a batch that begins with `CREATE [OR ALTER] PROC`
/// or `ALTER PROC`. Returns `None` for any other batch.
pub fn definition(sql: &str) -> Option<Result<Definition, SqlError>> {
    let words = crate::dialect::ext::leading_words(sql, 4);
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    let (create, alter, skip) = match words.as_slice() {
        ["CREATE", "PROC" | "PROCEDURE", ..] => (true, false, 2),
        ["ALTER", "PROC" | "PROCEDURE", ..] => (false, true, 2),
        ["CREATE", "OR", "ALTER", "PROC" | "PROCEDURE"] => (true, true, 4),
        _ => return None,
    };
    Some(parse(sql, create, alter, skip))
}

fn parse(sql: &str, create: bool, alter: bool, skip: usize) -> Result<Definition, SqlError> {
    let tokens = crate::dialect::tokenize(sql)
        .map_err(|error| SqlError::syntax(102, 1, error.to_string()))?;
    let dialect = crate::dialect::ServerDialect;
    let mut parser = Parser::new(&dialect).with_tokens_with_locations(tokens);
    for _ in 0..skip {
        parser.next_token();
    }
    let error = |parser: &Parser| syntax(&parser.peek_token());
    let name = parser
        .parse_object_name(false)
        .map_err(|_| error(&parser))?;
    let parts: Vec<String> = name
        .0
        .iter()
        .map(|part| match part {
            ObjectNamePart::Identifier(ident) => Ok(ident.value.clone()),
            _ => Err(()),
        })
        .collect::<Result<_, _>>()
        .map_err(|()| error(&parser))?;
    let (schema, name) = match parts.as_slice() {
        [name] => (None, name.clone()),
        [schema, name] => (Some(schema.clone()), name.clone()),
        [..] if parts.len() > 2 => {
            return Err(SqlError::syntax(
                166,
                1,
                "'CREATE/ALTER PROCEDURE' does not allow specifying the database name as a prefix to the object name.",
            ));
        }
        _ => return Err(error(&parser)),
    };
    if parser.peek_token().token == Token::SemiColon {
        return Err(SqlError::new(
            40515,
            1,
            "unsupported numbered stored procedure",
        ));
    }
    let parenthesized = parser.consume_token(&Token::LParen);
    let mut parameters: Vec<Parameter> = Vec::new();
    let starts_parameter = |token: &Token| matches!(token, Token::Word(w) if w.quote_style.is_none() && w.value.starts_with('@'));
    if starts_parameter(&parser.peek_token().token) {
        loop {
            let Token::Word(word) = parser.next_token().token else {
                return Err(error(&parser));
            };
            if word.value.starts_with("@@") || word.value.len() < 2 {
                return Err(error(&parser));
            }
            if parameters
                .iter()
                .any(|p| p.name.eq_ignore_ascii_case(&word.value))
            {
                return Err(SqlError::syntax(
                    134,
                    1,
                    format!(
                        "The variable name '{}' has already been declared. Variable names must be unique within a query batch or stored procedure.",
                        word.value
                    ),
                ));
            }
            // `@p AS int` is allowed.
            if is_word(&parser.peek_token().token, "AS") {
                parser.next_token();
            }
            let data_type = parser.parse_data_type().map_err(|_| error(&parser))?;
            if is_word(&parser.peek_token().token, "VARYING") {
                return Err(SqlError::new(
                    40515,
                    1,
                    "unsupported CURSOR VARYING procedure parameter",
                ));
            }
            if is_word(&parser.peek_token().token, "NULL") {
                parser.next_token();
            }
            let default = if parser.consume_token(&Token::Eq) {
                let start = parser.peek_token().span.start;
                let expr = parser.parse_expr().map_err(|_| error(&parser))?;
                let end = parser.peek_token().span.start;
                let text = sql[offset(sql, start)..offset(sql, end)].trim().to_string();
                Some((expr, text))
            } else {
                None
            };
            let mut output = false;
            loop {
                let next = parser.peek_token().token;
                if is_word(&next, "OUTPUT") || is_word(&next, "OUT") {
                    output = true;
                } else if is_word(&next, "READONLY") {
                    return Err(SqlError::new(
                        40515,
                        1,
                        "unsupported READONLY table-valued procedure parameter",
                    ));
                } else {
                    break;
                }
                parser.next_token();
            }
            parameters.push(Parameter {
                name: word.value,
                data_type,
                output,
                default,
            });
            if !parser.consume_token(&Token::Comma) {
                break;
            }
            if !starts_parameter(&parser.peek_token().token) {
                return Err(error(&parser));
            }
        }
    }
    if parenthesized && !parser.consume_token(&Token::RParen) {
        return Err(error(&parser));
    }
    if is_word(&parser.peek_token().token, "WITH") {
        parser.next_token();
        loop {
            let option = parser.next_token();
            if is_word(&option.token, "EXECUTE") || is_word(&option.token, "EXEC") {
                if !is_word(&parser.next_token().token, "AS") {
                    return Err(error(&parser));
                }
                let principal = parser.next_token();
                if !(is_word(&principal.token, "CALLER")
                    || is_word(&principal.token, "SELF")
                    || is_word(&principal.token, "OWNER")
                    || matches!(principal.token, Token::SingleQuotedString(_)))
                {
                    return Err(syntax(&principal));
                }
            } else if ![
                "RECOMPILE",
                "ENCRYPTION",
                "SCHEMABINDING",
                "NATIVE_COMPILATION",
            ]
            .iter()
            .any(|name| is_word(&option.token, name))
            {
                return Err(syntax(&option));
            }
            if !parser.consume_token(&Token::Comma) {
                break;
            }
        }
    }
    if is_word(&parser.peek_token().token, "FOR")
        && is_word(&parser.peek_nth_token(1).token, "REPLICATION")
    {
        parser.next_token();
        parser.next_token();
    }
    let as_token = parser.next_token();
    if !is_word(&as_token.token, "AS") {
        return Err(syntax(&as_token));
    }
    Ok(Definition {
        create,
        alter,
        schema,
        name,
        parameters,
        body: offset(sql, as_token.span.end),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers_record_parameters_options_and_body() {
        let sql = "-- c\nCREATE OR ALTER PROC [dbo].p_out (@a int, @b nvarchar(10) = N'x y' OUTPUT,\n  @c AS decimal(5,2) = -1.5 OUT) WITH RECOMPILE, EXECUTE AS CALLER AS\nSELECT 1";
        let definition = definition(sql).unwrap().unwrap();
        assert!(definition.create && definition.alter);
        assert_eq!(definition.schema.as_deref(), Some("dbo"));
        assert_eq!(definition.name, "p_out");
        assert_eq!(&sql[definition.body..], "\nSELECT 1");
        let names: Vec<_> = definition
            .parameters
            .iter()
            .map(|p| {
                (
                    p.name.as_str(),
                    p.data_type.to_string(),
                    p.output,
                    p.default.as_ref().map(|d| d.1.as_str()),
                )
            })
            .collect();
        assert_eq!(
            names,
            [
                ("@a", "INT".to_string(), false, None),
                ("@b", "NVARCHAR(10)".to_string(), true, Some("N'x y'")),
                ("@c", "DECIMAL(5,2)".to_string(), true, Some("-1.5")),
            ]
        );
        let sql = "ALTER PROCEDURE foo AS SELECT 7 AS value;";
        let definition = super::definition(sql).unwrap().unwrap();
        assert!(!definition.create && definition.alter);
        assert_eq!(&sql[definition.body..], " SELECT 7 AS value;");
        assert!(super::definition("SELECT 1").is_none());
    }

    #[test]
    fn multibyte_text_keeps_byte_offsets() {
        let sql = "CREATE PROCEDURE p @s nvarchar(5) = N'żółw' AS SELECT N'ą' AS a";
        let definition = definition(sql).unwrap().unwrap();
        assert_eq!(
            definition.parameters[0].default.as_ref().unwrap().1,
            "N'żółw'"
        );
        assert_eq!(&sql[definition.body..], " SELECT N'ą' AS a");
    }

    #[test]
    fn header_errors_match_sql_server() {
        for (sql, number) in [
            ("CREATE PROCEDURE d.dbo.p AS SELECT 1", 166),
            ("CREATE PROCEDURE p @a int, @A int AS SELECT 1", 134),
            ("CREATE PROCEDURE p @a int SELECT 1", 156),
            ("CREATE PROCEDURE p", 102),
        ] {
            let error = definition(sql).unwrap().unwrap_err();
            assert_eq!(error.number, number, "{sql}: {}", error.message);
        }
    }
}
