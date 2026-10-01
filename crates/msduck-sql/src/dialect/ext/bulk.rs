//! Syntax for INSERT BULK, the statement a bulk-load client (SqlBulkCopy,
//! tedious `newBulkLoad`, mssql `request.bulk`, bcp) sends before its
//! BulkLoadBCP message:
//!
//! ```text
//! INSERT BULK table ( column type [COLLATE name] [NULL | NOT NULL] [, ...] )
//!     [ WITH ( option [, ...] ) ]
//! ```
//!
//! The options are `CHECK_CONSTRAINTS`, `FIRE_TRIGGERS`, `KEEP_NULLS`,
//! `TABLOCK`, `ROWS_PER_BATCH = n`, `KILOBYTES_PER_BATCH = n` and
//! `ORDER ( column [ASC | DESC] [, ...] )`. SQL Server rejects anything else,
//! including `KEEP_IDENTITY`, with 102; a client keeps identity values by
//! listing the identity column. The statement has no sqlparser equivalent,
//! so it travels through the batch as an extension carrier whose payload is
//! the canonical statement text (see docs/extension-hooks.md).
use super::{carrier, custom};
use sqlparser::{
    ast::{DataType, Ident, ObjectName, Statement},
    keywords::Keyword,
    parser::{Parser, ParserError},
    tokenizer::Token,
};
use std::fmt;

/// The carrier kind of INSERT BULK.
pub const KIND: &str = "insert_bulk";

/// One column of the INSERT BULK column list.
#[derive(Clone, Debug, PartialEq)]
pub struct Column {
    pub name: Ident,
    pub data_type: DataType,
    pub collation: Option<ObjectName>,
    /// An explicit `NULL` (`Some(true)`) or `NOT NULL` (`Some(false)`).
    pub nullable: Option<bool>,
}

/// The WITH options.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Options {
    pub check_constraints: bool,
    pub fire_triggers: bool,
    pub keep_nulls: bool,
    pub tablock: bool,
    pub rows_per_batch: Option<u64>,
    pub kilobytes_per_batch: Option<u64>,
    /// `ORDER` hint columns, `true` for DESC.
    pub order: Vec<(Ident, bool)>,
}

/// A decoded INSERT BULK statement.
#[derive(Clone, Debug, PartialEq)]
pub struct InsertBulk {
    pub table: ObjectName,
    pub columns: Vec<Column>,
    pub options: Options,
}

/// An identifier in brackets, with `]` doubled. (sqlparser prints a
/// bracketed identifier without escaping.)
pub fn bracket(value: &str) -> String {
    format!("[{}]", value.replace(']', "]]"))
}

/// A multipart name with every part in brackets.
pub fn bracket_name(name: &ObjectName) -> String {
    name.0
        .iter()
        .map(|part| match part {
            sqlparser::ast::ObjectNamePart::Identifier(ident) => bracket(&ident.value),
            other => other.to_string(),
        })
        .collect::<Vec<_>>()
        .join(".")
}

impl fmt::Display for InsertBulk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "INSERT BULK {} (", bracket_name(&self.table))?;
        for (index, column) in self.columns.iter().enumerate() {
            if index > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{} {}", bracket(&column.name.value), column.data_type)?;
            if let Some(collation) = &column.collation {
                write!(f, " COLLATE {collation}")?;
            }
            match column.nullable {
                Some(true) => f.write_str(" NULL")?,
                Some(false) => f.write_str(" NOT NULL")?,
                None => {}
            }
        }
        f.write_str(")")?;
        let options = &self.options;
        let mut items = Vec::new();
        if options.tablock {
            items.push("TABLOCK".to_owned());
        }
        if options.check_constraints {
            items.push("CHECK_CONSTRAINTS".to_owned());
        }
        if options.fire_triggers {
            items.push("FIRE_TRIGGERS".to_owned());
        }
        if options.keep_nulls {
            items.push("KEEP_NULLS".to_owned());
        }
        if let Some(rows) = options.rows_per_batch {
            items.push(format!("ROWS_PER_BATCH = {rows}"));
        }
        if let Some(kilobytes) = options.kilobytes_per_batch {
            items.push(format!("KILOBYTES_PER_BATCH = {kilobytes}"));
        }
        if !options.order.is_empty() {
            let columns = options
                .order
                .iter()
                .map(|(name, descending)| {
                    format!(
                        "{} {}",
                        bracket(&name.value),
                        if *descending { "DESC" } else { "ASC" }
                    )
                })
                .collect::<Vec<_>>();
            items.push(format!("ORDER ({})", columns.join(", ")));
        }
        if !items.is_empty() {
            write!(f, " WITH ({})", items.join(", "))?;
        }
        Ok(())
    }
}

/// Whether the next two tokens are `INSERT BULK` (BULK unquoted).
fn starts_insert_bulk(parser: &Parser) -> bool {
    let insert = matches!(&parser.peek_nth_token_ref(0).token,
        Token::Word(word) if word.keyword == Keyword::INSERT && word.quote_style.is_none());
    let bulk = matches!(&parser.peek_nth_token_ref(1).token,
        Token::Word(word) if word.quote_style.is_none() && word.value.eq_ignore_ascii_case("BULK"));
    insert && bulk
}

/// Parse INSERT BULK into a carrier, or decline without consuming tokens.
pub fn parse(parser: &mut Parser) -> Option<Result<Statement, ParserError>> {
    if !starts_insert_bulk(parser) {
        return None;
    }
    Some(parse_insert_bulk(parser).map(|statement| carrier(KIND, &statement.to_string(), vec![])))
}

/// Carriers are owned by the extension mechanism; nothing else is.
pub fn owns(_statement: &Statement) -> bool {
    false
}

/// Decode a carrier produced by [`parse`].
pub fn decode(statement: &Statement) -> Option<Result<InsertBulk, ParserError>> {
    let custom = custom(statement)?;
    if custom.kind != KIND {
        return None;
    }
    Some((|| {
        let mut parser =
            Parser::new(&crate::dialect::ServerDialect).try_with_sql(custom.payload)?;
        let statement = parse_insert_bulk(&mut parser)?;
        if parser.peek_token_ref().token != Token::EOF {
            return parser.expected("end of statement", parser.peek_token());
        }
        Ok(statement)
    })())
}

/// SQL Server's 102 text for the token that stopped parsing: the next token,
/// or the last one consumed at the end of the statement.
pub fn syntax_error(parser: &Parser) -> ParserError {
    let next = &parser.peek_token_ref().token;
    let near = if matches!(next, Token::EOF | Token::SemiColon) {
        let mut index = parser.get_current_index();
        loop {
            let token = &parser.token_at(index).token;
            match token {
                Token::Whitespace(_) | Token::EOF | Token::SemiColon if index > 0 => index -= 1,
                Token::EOF => break String::new(),
                _ => break token_text(token),
            }
        }
    } else {
        token_text(next)
    };
    ParserError::ParserError(format!("Incorrect syntax near '{near}'."))
}

fn token_text(token: &Token) -> String {
    match token {
        Token::Word(word) => word.value.clone(),
        other => other.to_string(),
    }
}

/// Whether a parser error is one of this module's SQL Server 102 messages.
pub fn is_syntax_error(error: &ParserError) -> bool {
    matches!(error, ParserError::ParserError(message)
        if message.starts_with("Incorrect syntax near '") && message.ends_with("'."))
}

fn expect_token(parser: &mut Parser, token: &Token) -> Result<(), ParserError> {
    if parser.consume_token(token) {
        Ok(())
    } else {
        Err(syntax_error(parser))
    }
}

fn parse_insert_bulk(parser: &mut Parser) -> Result<InsertBulk, ParserError> {
    parser.expect_keyword(Keyword::INSERT)?;
    let bulk = parser.next_token();
    if !matches!(&bulk.token, Token::Word(word) if word.value.eq_ignore_ascii_case("BULK")) {
        return Err(syntax_error(parser));
    }
    let table = parser
        .parse_object_name(false)
        .map_err(|_| syntax_error(parser))?;
    expect_token(parser, &Token::LParen)?;
    let mut columns = Vec::new();
    loop {
        let name = parser
            .parse_identifier()
            .map_err(|_| syntax_error(parser))?;
        let data_type = parser.parse_data_type().map_err(|_| syntax_error(parser))?;
        let collation = if parser.parse_keyword(Keyword::COLLATE) {
            Some(
                parser
                    .parse_object_name(false)
                    .map_err(|_| syntax_error(parser))?,
            )
        } else {
            None
        };
        let nullable = if parser.parse_keywords(&[Keyword::NOT, Keyword::NULL]) {
            Some(false)
        } else if parser.parse_keyword(Keyword::NULL) {
            Some(true)
        } else {
            None
        };
        columns.push(Column {
            name,
            data_type,
            collation,
            nullable,
        });
        if parser.consume_token(&Token::Comma) {
            continue;
        }
        expect_token(parser, &Token::RParen)?;
        break;
    }
    let options = if parser.parse_keyword(Keyword::WITH) {
        parse_options(parser)?
    } else {
        Options::default()
    };
    if !matches!(parser.peek_token_ref().token, Token::EOF | Token::SemiColon) {
        return Err(syntax_error(parser));
    }
    Ok(InsertBulk {
        table,
        columns,
        options,
    })
}

fn word(parser: &Parser) -> Option<String> {
    match &parser.peek_token_ref().token {
        Token::Word(word) if word.quote_style.is_none() => Some(word.value.to_ascii_uppercase()),
        _ => None,
    }
}

fn number(parser: &mut Parser) -> Result<u64, ParserError> {
    expect_token(parser, &Token::Eq)?;
    match &parser.peek_token_ref().token {
        Token::Number(text, _) => match text.parse() {
            Ok(value) => {
                parser.next_token();
                Ok(value)
            }
            Err(_) => Err(syntax_error(parser)),
        },
        _ => Err(syntax_error(parser)),
    }
}

fn parse_options(parser: &mut Parser) -> Result<Options, ParserError> {
    expect_token(parser, &Token::LParen)?;
    let mut options = Options::default();
    loop {
        let Some(option) = word(parser) else {
            return Err(syntax_error(parser));
        };
        match option.as_str() {
            "CHECK_CONSTRAINTS" | "FIRE_TRIGGERS" | "KEEP_NULLS" | "TABLOCK" => {
                parser.next_token();
                match option.as_str() {
                    "CHECK_CONSTRAINTS" => options.check_constraints = true,
                    "FIRE_TRIGGERS" => options.fire_triggers = true,
                    "KEEP_NULLS" => options.keep_nulls = true,
                    _ => options.tablock = true,
                }
            }
            "ROWS_PER_BATCH" => {
                parser.next_token();
                options.rows_per_batch = Some(number(parser)?);
            }
            "KILOBYTES_PER_BATCH" => {
                parser.next_token();
                options.kilobytes_per_batch = Some(number(parser)?);
            }
            "ORDER" => {
                parser.next_token();
                expect_token(parser, &Token::LParen)?;
                loop {
                    let name = parser
                        .parse_identifier()
                        .map_err(|_| syntax_error(parser))?;
                    let descending = if parser.parse_keyword(Keyword::DESC) {
                        true
                    } else {
                        let _ = parser.parse_keyword(Keyword::ASC);
                        false
                    };
                    options.order.push((name, descending));
                    if !parser.consume_token(&Token::Comma) {
                        break;
                    }
                }
                expect_token(parser, &Token::RParen)?;
            }
            _ => return Err(syntax_error(parser)),
        }
        if parser.consume_token(&Token::Comma) {
            continue;
        }
        expect_token(parser, &Token::RParen)?;
        return Ok(options);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn statement(sql: &str) -> Result<InsertBulk, ParserError> {
        let statements = crate::batch::parse(sql).map_err(|error| {
            ParserError::ParserError(
                error
                    .downcast_ref::<ParserError>()
                    .map(|error| match error {
                        ParserError::ParserError(message) => message.clone(),
                        other => other.to_string(),
                    })
                    .unwrap_or_else(|| error.to_string()),
            )
        })?;
        assert_eq!(statements.len(), 1);
        decode(&statements[0]).expect("an INSERT BULK carrier")
    }

    fn error(sql: &str) -> String {
        match statement(sql) {
            Err(ParserError::ParserError(message)) => message,
            other => panic!("expected a syntax error for {sql}: {other:?}"),
        }
    }

    #[test]
    fn client_statements_round_trip_through_the_carrier() {
        let parsed = statement(
            "insert bulk [dbo].[items]([id] int, [name] nvarchar(50), [amount] decimal(18, 4), \
             [at] datetimeoffset(7), [blob] varbinary(max), [flag] bit) \
             WITH (CHECK_CONSTRAINTS,FIRE_TRIGGERS,KEEP_NULLS,TABLOCK)",
        )
        .unwrap();
        assert_eq!(parsed.table.to_string(), "[dbo].[items]");
        let names: Vec<_> = parsed
            .columns
            .iter()
            .map(|c| c.name.value.as_str())
            .collect();
        assert_eq!(names, ["id", "name", "amount", "at", "blob", "flag"]);
        assert!(parsed.options.check_constraints);
        assert!(parsed.options.fire_triggers);
        assert!(parsed.options.keep_nulls);
        assert!(parsed.options.tablock);
        // The canonical text parses back to the same statement.
        assert_eq!(statement(&parsed.to_string()).unwrap(), parsed);
    }

    #[test]
    fn collations_nullability_hints_and_comments_are_accepted() {
        let parsed = statement(
            "/* c */ INSERT  BULK dbo . t ([s] varchar(5) COLLATE Latin1_General_CI_AS NOT NULL, \
             [n] sysname NULL, [u] int) with (keep_nulls, ROWS_PER_BATCH = 10, \
             KILOBYTES_PER_BATCH = 5, ORDER (s ASC, n DESC)) -- trailing",
        )
        .unwrap();
        // The canonical carrier text brackets every name.
        assert_eq!(bracket_name(&parsed.table), "[dbo].[t]");
        assert_eq!(
            parsed.columns[0].collation.as_ref().unwrap().to_string(),
            "Latin1_General_CI_AS"
        );
        assert_eq!(parsed.columns[0].nullable, Some(false));
        assert_eq!(parsed.columns[1].nullable, Some(true));
        assert_eq!(parsed.columns[2].nullable, None);
        assert!(parsed.options.keep_nulls);
        assert_eq!(parsed.options.rows_per_batch, Some(10));
        assert_eq!(parsed.options.kilobytes_per_batch, Some(5));
        assert_eq!(parsed.options.order.len(), 2);
        assert!(parsed.options.order[1].1);
        assert_eq!(statement(&parsed.to_string()).unwrap(), parsed);
    }

    #[test]
    fn bracketed_names_keep_closing_brackets() {
        let parsed =
            statement("INSERT BULK [dbo].[a]]b] ([c]]d] int) WITH (ORDER ([c]]d]))").unwrap();
        assert_eq!(bracket_name(&parsed.table), "[dbo].[a]]b]");
        assert_eq!(parsed.columns[0].name.value, "c]d");
        assert_eq!(statement(&parsed.to_string()).unwrap(), parsed);
    }

    #[test]
    fn unknown_options_and_missing_columns_fail_like_sql_server() {
        // Captured: reference/gaps-bulk.json (102, class 15).
        assert_eq!(
            error("insert bulk n([b] int) WITH (KEEP_IDENTITY)"),
            "Incorrect syntax near 'KEEP_IDENTITY'."
        );
        assert_eq!(
            error("insert bulk n([b] int) WITH (FOO)"),
            "Incorrect syntax near 'FOO'."
        );
        assert_eq!(error("insert bulk n"), "Incorrect syntax near 'n'.");
        assert!(is_syntax_error(&ParserError::ParserError(error(
            "insert bulk n ([a] int"
        ))));
    }

    #[test]
    fn other_statements_are_declined() {
        let statements = crate::batch::parse("INSERT INTO bulk VALUES (1)").unwrap();
        assert!(decode(&statements[0]).is_none());
        let statements = crate::batch::parse("INSERT bulk (a) VALUES (1)");
        // `bulk` followed by a column list is still INSERT BULK syntax, which
        // needs a table name first.
        assert!(statements.is_err());
    }
}
