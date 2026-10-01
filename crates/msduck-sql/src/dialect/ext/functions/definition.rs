//! CREATE / ALTER FUNCTION definitions: header, options and body.
//!
//! A definition must be alone in its batch, so it is parsed from the whole
//! batch text rather than through the statement parser. The scalar and
//! multi-statement bodies between `BEGIN` and the final `END` are parsed as an
//! ordinary batch; an inline body is the query after `RETURN`.
use anyhow::{Result, anyhow, bail};
use msduck_core::diagnostic::SqlError;
use sqlparser::{
    ast::*,
    keywords::Keyword,
    parser::Parser,
    tokenizer::{Location, Token, TokenWithSpan, Whitespace},
};

/// Which statement introduced the definition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Create,
    Alter,
    CreateOrAlter,
}

impl Action {
    /// Recognize a batch that starts with a function definition.
    pub fn of(sql: &str) -> Option<Self> {
        let words = crate::dialect::ext::leading_words(sql, 4);
        let words: Vec<&str> = words.iter().map(String::as_str).collect();
        match words.as_slice() {
            ["CREATE", "FUNCTION", ..] => Some(Self::Create),
            ["ALTER", "FUNCTION", ..] => Some(Self::Alter),
            ["CREATE", "OR", "ALTER", "FUNCTION"] => Some(Self::CreateOrAlter),
            _ => None,
        }
    }
}

/// A declared parameter.
#[derive(Clone, Debug, PartialEq)]
pub struct Parameter {
    /// The name as written, including `@`.
    pub name: String,
    pub data_type: DataType,
    pub default: Option<Expr>,
    pub readonly: bool,
}

/// What the function returns.
#[derive(Clone, Debug, PartialEq)]
pub enum Returns {
    /// A scalar function.
    Scalar(DataType),
    /// An inline table-valued function (`RETURNS TABLE`).
    Inline,
    /// A multi-statement table-valued function (`RETURNS @t TABLE (...)`).
    Table {
        variable: String,
        columns: Vec<ColumnDef>,
    },
}

/// `WITH` options. They are recorded; only `RETURNS NULL ON NULL INPUT`
/// changes evaluation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Options {
    pub schemabinding: bool,
    pub encryption: bool,
    pub native_compilation: bool,
    pub returns_null_on_null_input: bool,
    pub execute_as: Option<String>,
    pub inline: Option<bool>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Body {
    /// The statements between `BEGIN` and `END`.
    Statements(Vec<Statement>),
    /// The query an inline function returns.
    Query(Box<Query>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Definition {
    pub action: Action,
    pub schema: Option<String>,
    pub name: String,
    pub parameters: Vec<Parameter>,
    pub returns: Returns,
    pub options: Options,
    pub body: Body,
}

impl Definition {
    /// The SQL Server object type code: FN, IF or TF.
    pub fn type_code(&self) -> &'static str {
        match self.returns {
            Returns::Scalar(_) => "FN",
            Returns::Inline => "IF",
            Returns::Table { .. } => "TF",
        }
    }
}

fn syntax(near: &str) -> anyhow::Error {
    SqlError::syntax(102, 31, format!("Incorrect syntax near '{near}'.")).into()
}

fn token_text(token: &Token) -> String {
    match token {
        Token::EOF => "end of file".into(),
        Token::Word(word) => word.value.clone(),
        other => other.to_string(),
    }
}

fn near(parser: &Parser) -> anyhow::Error {
    syntax(&token_text(&parser.peek_token_ref().token))
}

fn is_word(token: &Token, text: &str) -> bool {
    matches!(token, Token::Word(word) if word.quote_style.is_none() && word.value.eq_ignore_ascii_case(text))
}

fn peek_word(parser: &Parser, text: &str) -> bool {
    is_word(&parser.peek_token_ref().token, text)
}

fn parse_word(parser: &mut Parser, text: &str) -> bool {
    if peek_word(parser, text) {
        parser.next_token();
        true
    } else {
        false
    }
}

fn expect_word(parser: &mut Parser, text: &str) -> Result<()> {
    if parse_word(parser, text) {
        Ok(())
    } else {
        Err(near(parser))
    }
}

/// Byte offset of a tokenizer location (1-based line and character column).
fn offset(sql: &str, location: Location) -> usize {
    let (mut line, mut column) = (1u64, 1u64);
    for (index, ch) in sql.char_indices() {
        if line == location.line && column == location.column {
            return index;
        }
        if ch == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    sql.len()
}

/// Declarations default an omitted character length to one, unlike CAST.
pub fn declared_type(mut data_type: DataType) -> DataType {
    let one = || {
        Some(CharacterLength::IntegerLength {
            length: 1,
            unit: None,
        })
    };
    match &mut data_type {
        DataType::Varchar(length @ None)
        | DataType::Char(length @ None)
        | DataType::Character(length @ None)
        | DataType::CharacterVarying(length @ None)
        | DataType::CharVarying(length @ None)
        | DataType::Nvarchar(length @ None) => *length = one(),
        DataType::Custom(name, arguments)
            if arguments.is_empty()
                && name.0.len() == 1
                && name.to_string().eq_ignore_ascii_case("nchar") =>
        {
            *arguments = vec!["1".into()];
        }
        _ => {}
    }
    data_type
}

/// Parse a CREATE / ALTER / CREATE OR ALTER FUNCTION batch.
pub fn parse(sql: &str) -> Result<Definition> {
    let action = Action::of(sql).ok_or_else(|| anyhow!("not a function definition"))?;
    let tokens = crate::dialect::tokenize(sql).map_err(|error| anyhow!(error.to_string()))?;
    let dialect = crate::dialect::ServerDialect;
    let mut parser = Parser::new(&dialect).with_tokens_with_locations(tokens.clone());
    match action {
        Action::Create => expect_word(&mut parser, "CREATE")?,
        Action::Alter => expect_word(&mut parser, "ALTER")?,
        Action::CreateOrAlter => {
            for word in ["CREATE", "OR", "ALTER"] {
                expect_word(&mut parser, word)?;
            }
        }
    }
    expect_word(&mut parser, "FUNCTION")?;
    let name = parser.parse_object_name(false).map_err(|_| near(&parser))?;
    let parts: Vec<String> = name
        .0
        .iter()
        .map(|part| {
            part.as_ident()
                .map(|ident| ident.value.clone())
                .ok_or_else(|| near(&parser))
        })
        .collect::<Result<_>>()?;
    let (schema, name) = match parts.as_slice() {
        [name] => (None, name.clone()),
        [schema, name] => (Some(schema.clone()), name.clone()),
        [_, _, _] => bail!(SqlError::new(
            166,
            1,
            format!(
                "'{}' does not allow specifying the database name as a prefix to the object name.",
                match action {
                    Action::Create => "CREATE FUNCTION",
                    _ => "CREATE/ALTER FUNCTION",
                }
            )
        )),
        _ => return Err(near(&parser)),
    };
    if name.is_empty() || name.chars().count() > 128 {
        return Err(near(&parser));
    }
    if !parser.consume_token(&Token::LParen) {
        return Err(near(&parser));
    }
    let mut parameters: Vec<Parameter> = Vec::new();
    if !parser.consume_token(&Token::RParen) {
        loop {
            let token = parser.next_token();
            let Token::Word(word) = &token.token else {
                return Err(syntax(&token_text(&token.token)));
            };
            if !word.value.starts_with('@') || word.value.len() < 2 {
                return Err(syntax(&word.value));
            }
            let parameter = word.value.clone();
            parse_word(&mut parser, "AS");
            let data_type = parser.parse_data_type().map_err(|_| near(&parser))?;
            parse_word(&mut parser, "NULL");
            let default = if parser.consume_token(&Token::Eq) {
                Some(parser.parse_expr().map_err(|_| near(&parser))?)
            } else {
                None
            };
            let readonly = parse_word(&mut parser, "READONLY");
            if peek_word(&parser, "OUTPUT") || peek_word(&parser, "OUT") {
                return Err(near(&parser));
            }
            if parameters
                .iter()
                .any(|p| p.name.eq_ignore_ascii_case(&parameter))
            {
                bail!(SqlError::new(
                    134,
                    1,
                    format!(
                        "The variable name '{parameter}' has already been declared. Variable names must be unique within a query batch or stored procedure."
                    )
                ));
            }
            if parameters.len() >= 1024 {
                bail!(SqlError::new(
                    180,
                    1,
                    "There are too many parameters in this CREATE FUNCTION statement. The maximum number is 1024."
                ));
            }
            parameters.push(Parameter {
                name: parameter,
                data_type: declared_type(data_type),
                default,
                readonly,
            });
            if parser.consume_token(&Token::Comma) {
                continue;
            }
            if parser.consume_token(&Token::RParen) {
                break;
            }
            return Err(near(&parser));
        }
    }
    expect_word(&mut parser, "RETURNS")?;
    let returns = match &parser.peek_token_ref().token {
        Token::Word(word) if word.value.starts_with('@') => {
            let variable = word.value.clone();
            parser.next_token();
            expect_word(&mut parser, "TABLE")?;
            let (columns, _constraints) = parser.parse_columns().map_err(|_| near(&parser))?;
            if columns.is_empty() {
                return Err(near(&parser));
            }
            Returns::Table {
                variable,
                columns: columns
                    .into_iter()
                    .map(|mut column| {
                        column.data_type = declared_type(column.data_type);
                        column
                    })
                    .collect(),
            }
        }
        token if is_word(token, "TABLE") => {
            parser.next_token();
            Returns::Inline
        }
        _ => Returns::Scalar(declared_type(
            parser.parse_data_type().map_err(|_| near(&parser))?,
        )),
    };
    let mut options = Options::default();
    if parse_word(&mut parser, "WITH") {
        loop {
            if parse_word(&mut parser, "SCHEMABINDING") {
                options.schemabinding = true;
            } else if parse_word(&mut parser, "ENCRYPTION") {
                options.encryption = true;
            } else if parse_word(&mut parser, "NATIVE_COMPILATION") {
                options.native_compilation = true;
            } else if parse_word(&mut parser, "RETURNS") {
                for word in ["NULL", "ON", "NULL", "INPUT"] {
                    expect_word(&mut parser, word)?;
                }
                options.returns_null_on_null_input = true;
            } else if parse_word(&mut parser, "CALLED") {
                for word in ["ON", "NULL", "INPUT"] {
                    expect_word(&mut parser, word)?;
                }
                options.returns_null_on_null_input = false;
            } else if parse_word(&mut parser, "EXECUTE") || parse_word(&mut parser, "EXEC") {
                expect_word(&mut parser, "AS")?;
                let token = parser.next_token();
                options.execute_as = Some(match &token.token {
                    Token::Word(word)
                        if ["CALLER", "SELF", "OWNER"]
                            .iter()
                            .any(|w| word.value.eq_ignore_ascii_case(w)) =>
                    {
                        word.value.to_uppercase()
                    }
                    Token::SingleQuotedString(user) => user.clone(),
                    other => return Err(syntax(&token_text(other))),
                });
            } else if parse_word(&mut parser, "INLINE") {
                if !parser.consume_token(&Token::Eq) {
                    return Err(near(&parser));
                }
                options.inline = Some(if parse_word(&mut parser, "ON") {
                    true
                } else if parse_word(&mut parser, "OFF") {
                    false
                } else {
                    return Err(near(&parser));
                });
            } else {
                return Err(near(&parser));
            }
            if !parser.consume_token(&Token::Comma) {
                break;
            }
        }
    }
    parse_word(&mut parser, "AS");
    let body = match returns {
        Returns::Inline => {
            if !peek_word(&parser, "RETURN") {
                return Err(near(&parser));
            }
            let start = offset(sql, parser.next_token().span.end);
            let statements = crate::batch::parse(&sql[start..])?;
            match <[Statement; 1]>::try_from(statements) {
                Ok([Statement::Query(query)]) => Body::Query(query),
                _ => return Err(syntax("RETURN")),
            }
        }
        _ => {
            if !peek_word(&parser, "BEGIN") {
                return Err(near(&parser));
            }
            let start = offset(sql, parser.next_token().span.end);
            let end = final_end(&tokens).ok_or_else(|| syntax("end of file"))?;
            let end = offset(sql, end);
            if end < start {
                return Err(syntax("end of file"));
            }
            Body::Statements(crate::batch::parse(&sql[start..end])?)
        }
    };
    Ok(Definition {
        action,
        schema,
        name,
        parameters,
        returns,
        options,
        body,
    })
}

/// The start of the batch's final `END`, ignoring trailing semicolons and
/// comments.
fn final_end(tokens: &[TokenWithSpan]) -> Option<Location> {
    let last = tokens.iter().rev().find(|token| {
        !matches!(
            token.token,
            Token::Whitespace(
                Whitespace::Space
                    | Whitespace::Newline
                    | Whitespace::Tab
                    | Whitespace::SingleLineComment { .. }
                    | Whitespace::MultiLineComment(_)
            ) | Token::SemiColon
                | Token::EOF
        )
    })?;
    match &last.token {
        Token::Word(word) if word.keyword == Keyword::END && word.quote_style.is_none() => {
            Some(last.span.start)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_definition_with_parameters_options_and_body() {
        let definition = parse(
            "CREATE FUNCTION dbo.foo(@x int, @y varchar = 'a') RETURNS int \
             WITH SCHEMABINDING, RETURNS NULL ON NULL INPUT, EXECUTE AS CALLER \
             AS BEGIN DECLARE @z int = @x; RETURN @z + 1; END; -- trailing",
        )
        .unwrap();
        assert_eq!(definition.action, Action::Create);
        assert_eq!(
            (definition.schema.as_deref(), definition.name.as_str()),
            (Some("dbo"), "foo")
        );
        assert_eq!(definition.parameters.len(), 2);
        assert_eq!(definition.parameters[1].data_type.to_string(), "VARCHAR(1)");
        assert!(definition.parameters[1].default.is_some());
        assert_eq!(definition.returns, Returns::Scalar(DataType::Int(None)));
        assert!(definition.options.schemabinding);
        assert!(definition.options.returns_null_on_null_input);
        assert_eq!(definition.options.execute_as.as_deref(), Some("CALLER"));
        let Body::Statements(body) = &definition.body else {
            panic!()
        };
        assert_eq!(body.len(), 2);
        assert_eq!(definition.type_code(), "FN");
    }

    #[test]
    fn table_definitions() {
        let inline =
            parse("create or alter function it(@n int) returns table as return (select @n as n)")
                .unwrap();
        assert_eq!(inline.action, Action::CreateOrAlter);
        assert_eq!(inline.type_code(), "IF");
        assert!(matches!(inline.body, Body::Query(_)));
        let table = parse(
            "ALTER FUNCTION dbo.mt() RETURNS @t TABLE (i int NOT NULL, s varchar(5)) \
             BEGIN INSERT @t VALUES (1, 'a') RETURN END",
        )
        .unwrap();
        assert_eq!(table.type_code(), "TF");
        let Returns::Table { variable, columns } = &table.returns else {
            panic!()
        };
        assert_eq!((variable.as_str(), columns.len()), ("@t", 2));
        let Body::Statements(body) = &table.body else {
            panic!()
        };
        assert_eq!(body.len(), 2);
    }

    #[test]
    fn malformed_definitions_are_syntax_errors() {
        for sql in [
            "CREATE FUNCTION dbo.f(@x int) RETURNS int AS RETURN @x",
            "CREATE FUNCTION dbo.f(@x int) RETURNS TABLE AS BEGIN RETURN SELECT 1 a END",
            "CREATE FUNCTION dbo.f(@x int) RETURNS int AS BEGIN RETURN @x",
        ] {
            let error = parse(sql).unwrap_err();
            assert_eq!(
                error.downcast_ref::<SqlError>().map(|e| e.number),
                Some(102),
                "{sql}"
            );
        }
        let error =
            parse("CREATE FUNCTION db.dbo.f() RETURNS int AS BEGIN RETURN 1 END").unwrap_err();
        assert_eq!(error.downcast_ref::<SqlError>().unwrap().number, 166);
    }
}
