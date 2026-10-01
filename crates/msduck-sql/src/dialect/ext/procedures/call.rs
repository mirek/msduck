//! `EXEC`/`EXECUTE` and `DROP PROCEDURE` syntax, and the call encoding.
//!
//! A call stays a [`Statement::Execute`]:
//!
//! - `name` is the module name, or `None` for `EXEC (string)` (with
//!   `has_parentheses`) and for `EXEC @module_variable`;
//! - `parameters` holds the arguments, `@name = value` as an `Eq` binary
//!   expression, `OUTPUT` as `__msduck_output(value)` and `DEFAULT` as
//!   `__msduck_default()`. A bare identifier argument is a string literal.
//! - `using` carries tagged markers: `@status =` (alias `__msduck_return`),
//!   the module variable (`__msduck_module`) and calls inside a `BEGIN TRY`
//!   body (`__msduck_try`).
//!
//! The tags are identifiers the T-SQL tokenizer cannot place in those
//! positions, and every variable stays visible to preflight's checks.
use sqlparser::{
    ast::*,
    keywords::Keyword,
    parser::{Parser, ParserError},
    tokenizer::Token,
};

const OUTPUT: &str = "__msduck_output";
const DEFAULT: &str = "__msduck_default";
const RETURN: &str = "__msduck_return";
const MODULE: &str = "__msduck_module";
const TRY: &str = "__msduck_try";

/// What an `EXEC` runs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Target<'a> {
    /// A (possibly qualified) module name.
    Name(&'a ObjectName),
    /// `EXEC @variable`: the module name is the variable's value.
    Variable(&'a str),
    /// `EXEC (string)`: dynamic SQL.
    Dynamic(&'a Expr),
}

/// One argument of a call.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Argument<'a> {
    /// `@name` of `@name = value`, as written.
    pub name: Option<&'a str>,
    /// The value; `None` for `DEFAULT`.
    pub value: Option<&'a Expr>,
    /// `OUTPUT` or `OUT` follows the value.
    pub output: bool,
}

/// A decoded procedure call.
#[derive(Clone, Debug, PartialEq)]
pub struct Call<'a> {
    pub target: Target<'a>,
    pub arguments: Vec<Argument<'a>>,
    /// The `@status` variable of `EXEC @status = ...`.
    pub status: Option<&'a str>,
    /// The call is inside a `BEGIN TRY` body, so its caller catches errors.
    pub in_try: bool,
}

fn function(name: &str, args: Vec<Expr>) -> Expr {
    Expr::Function(Function {
        name: ObjectName::from(vec![Ident::new(name)]),
        uses_odbc_syntax: false,
        parameters: FunctionArguments::None,
        args: FunctionArguments::List(FunctionArgumentList {
            duplicate_treatment: None,
            args: args
                .into_iter()
                .map(|e| FunctionArg::Unnamed(FunctionArgExpr::Expr(e)))
                .collect(),
            clauses: vec![],
        }),
        filter: None,
        null_treatment: None,
        over: None,
        within_group: vec![],
    })
}

fn marker(expr: &Expr) -> Option<(&str, Option<&Expr>)> {
    let Expr::Function(f) = expr else {
        return None;
    };
    let name = match f.name.0.as_slice() {
        [ObjectNamePart::Identifier(ident)] if ident.quote_style.is_none() => ident.value.as_str(),
        _ => return None,
    };
    if name != OUTPUT && name != DEFAULT {
        return None;
    }
    let FunctionArguments::List(list) = &f.args else {
        return None;
    };
    match list.args.as_slice() {
        [] => Some((name, None)),
        [FunctionArg::Unnamed(FunctionArgExpr::Expr(e))] => Some((name, Some(e))),
        _ => None,
    }
}

fn tagged(tag: &str, expr: Expr) -> ExprWithAlias {
    ExprWithAlias {
        expr,
        alias: Some(Ident::new(tag)),
    }
}

/// Decode a call. Plain sqlparser `EXEC` statements decode too.
pub fn call(statement: &Statement) -> Option<Call<'_>> {
    let Statement::Execute {
        name,
        parameters,
        has_parentheses,
        immediate: false,
        into,
        using,
        ..
    } = statement
    else {
        return None;
    };
    if !into.is_empty() {
        return None;
    }
    let tag = |tag: &str| {
        using
            .iter()
            .find(|item| item.alias.as_ref().is_some_and(|alias| alias.value == tag))
            .map(|item| &item.expr)
    };
    fn variable(expr: Option<&Expr>) -> Option<&str> {
        match expr {
            Some(Expr::Identifier(ident)) => Some(ident.value.as_str()),
            _ => None,
        }
    }
    let target = match (name, has_parentheses) {
        (Some(name), false) => Target::Name(name),
        (None, true) => match parameters.as_slice() {
            [expr] => {
                return Some(Call {
                    target: Target::Dynamic(expr),
                    arguments: vec![],
                    status: variable(tag(RETURN)),
                    in_try: tag(TRY).is_some(),
                });
            }
            _ => return None,
        },
        (None, false) => Target::Variable(variable(tag(MODULE))?),
        (Some(_), true) => return None,
    };
    let arguments = parameters
        .iter()
        .map(|parameter| {
            let (name, value) = match parameter {
                Expr::BinaryOp {
                    left,
                    op: BinaryOperator::Eq,
                    right,
                } if matches!(left.as_ref(), Expr::Identifier(id) if id.value.starts_with('@')) => {
                    let Expr::Identifier(id) = left.as_ref() else {
                        unreachable!()
                    };
                    (Some(id.value.as_str()), right.as_ref())
                }
                value => (None, value),
            };
            match marker(value) {
                Some((DEFAULT, None)) => Argument {
                    name,
                    value: None,
                    output: false,
                },
                Some((OUTPUT, Some(value))) => Argument {
                    name,
                    value: Some(value),
                    output: true,
                },
                _ => Argument {
                    name,
                    value: Some(value),
                    output: false,
                },
            }
        })
        .collect();
    Some(Call {
        target,
        arguments,
        status: variable(tag(RETURN)),
        in_try: tag(TRY).is_some(),
    })
}

fn word(token: &Token) -> Option<&sqlparser::tokenizer::Word> {
    match token {
        Token::Word(word) => Some(word),
        _ => None,
    }
}

fn is_word(token: &Token, value: &str) -> bool {
    word(token).is_some_and(|w| w.quote_style.is_none() && w.value.eq_ignore_ascii_case(value))
}

fn is_variable(token: &Token) -> bool {
    word(token).is_some_and(|w| w.quote_style.is_none() && w.value.starts_with('@'))
}

fn is_procedure(token: &Token) -> bool {
    is_word(token, "PROC") || is_word(token, "PROCEDURE")
}

/// Words that begin a T-SQL statement. After a call they start the next
/// statement rather than a bare-identifier argument, and a batch that
/// begins with one is not a bare procedure call.
const STATEMENTS: &[&str] = &[
    "ALTER",
    "BACKUP",
    "BEGIN",
    "BREAK",
    "BULK",
    "CHECKPOINT",
    "CLOSE",
    "COMMIT",
    "CONTINUE",
    "CREATE",
    "DBCC",
    "DEALLOCATE",
    "DECLARE",
    "DELETE",
    "DENY",
    "DISABLE",
    "DROP",
    "ELSE",
    "ENABLE",
    "END",
    "EXEC",
    "EXECUTE",
    "FETCH",
    "GET",
    "GOTO",
    "GRANT",
    "IF",
    "INSERT",
    "KILL",
    "MERGE",
    "MOVE",
    "OPEN",
    "PRINT",
    "RAISERROR",
    "READTEXT",
    "RECEIVE",
    "RECONFIGURE",
    "RESTORE",
    "RETURN",
    "REVERT",
    "REVOKE",
    "ROLLBACK",
    "SAVE",
    "SELECT",
    "SEND",
    "SET",
    "SETUSER",
    "SHUTDOWN",
    "THROW",
    "TRUNCATE",
    "UPDATE",
    "UPDATETEXT",
    "USE",
    "WAITFOR",
    "WHILE",
    "WITH",
    "WRITETEXT",
];

/// Whether an unquoted word begins a T-SQL statement.
pub fn starts_statement(word: &str) -> bool {
    STATEMENTS.iter().any(|s| word.eq_ignore_ascii_case(s))
}

fn starts_argument(token: &Token) -> bool {
    match token {
        Token::Number(..)
        | Token::SingleQuotedString(_)
        | Token::NationalStringLiteral(_)
        | Token::HexStringLiteral(_)
        | Token::Minus
        | Token::Plus => true,
        Token::Word(word) => word.quote_style.is_some() || !starts_statement(&word.value),
        _ => false,
    }
}

fn syntax(token: &Token) -> ParserError {
    let near = match token {
        Token::EOF => {
            return ParserError::ParserError("Incorrect syntax near the end of the batch.".into());
        }
        Token::Word(word) if word.quote_style.is_none() && word.keyword != Keyword::NoKeyword => {
            return ParserError::ParserError(format!(
                "Incorrect syntax near the keyword '{}'.",
                word.value
            ));
        }
        token => token.to_string(),
    };
    ParserError::ParserError(format!("Incorrect syntax near '{near}'."))
}

pub(super) fn parse(parser: &mut Parser) -> Option<Result<Statement, ParserError>> {
    let first = parser.peek_token().token;
    let second = parser.peek_nth_token(1).token;
    if (is_word(&first, "EXEC") || is_word(&first, "EXECUTE")) && !is_word(&second, "AS") {
        return Some(parse_execute(parser));
    }
    if is_word(&first, "BEGIN") && is_word(&second, "TRY") {
        return Some(
            crate::dialect::parse_try_catch(parser).map(|mut statement| {
                if let Statement::StartTransaction { statements, .. } = &mut statement {
                    mark_try(statements);
                }
                statement
            }),
        );
    }
    if is_word(&first, "DROP") && is_procedure(&second) {
        return Some(parse_drop(parser));
    }
    let third = parser.peek_nth_token(2).token;
    let fourth = parser.peek_nth_token(3).token;
    if (is_word(&first, "CREATE") || is_word(&first, "ALTER")) && is_procedure(&second)
        || is_word(&first, "CREATE")
            && is_word(&second, "OR")
            && is_word(&third, "ALTER")
            && is_procedure(&fourth)
    {
        // A batch that begins with one is handled before parsing.
        return Some(Err(ParserError::ParserError(super::NOT_FIRST.into())));
    }
    None
}

/// Mark every call in a `BEGIN TRY` body (including nested blocks) as one
/// whose errors a CATCH handler receives. `sp_set_session_context` calls are
/// left unchanged: their binder accepts only the plain form.
pub fn mark_try(statements: &mut [Statement]) {
    struct Mark;
    impl VisitorMut for Mark {
        type Break = ();
        fn pre_visit_statement(&mut self, statement: &mut Statement) -> std::ops::ControlFlow<()> {
            if crate::session_function::set_call(statement).is_none()
                && let Statement::Execute { using, .. } = statement
                && !using
                    .iter()
                    .any(|item| item.alias.as_ref().is_some_and(|alias| alias.value == TRY))
            {
                using.push(tagged(TRY, Expr::Value(Value::Boolean(true).into())));
            }
            std::ops::ControlFlow::Continue(())
        }
    }
    for statement in statements {
        let _ = statement.visit(&mut Mark);
    }
}

fn parse_execute(parser: &mut Parser) -> Result<Statement, ParserError> {
    parser.next_token();
    let mut using = Vec::new();
    if parser.peek_token().token == Token::LParen {
        parser.next_token();
        let expr = parser.parse_expr()?;
        parser.expect_token(&Token::RParen)?;
        // EXEC (string) AS { LOGIN | USER } = 'name' runs as the caller.
        if is_word(&parser.peek_token().token, "AS")
            && (is_word(&parser.peek_nth_token(1).token, "LOGIN")
                || is_word(&parser.peek_nth_token(1).token, "USER"))
        {
            parser.next_token();
            parser.next_token();
            parser.expect_token(&Token::Eq)?;
            let next = parser.next_token();
            if !matches!(
                next.token,
                Token::SingleQuotedString(_) | Token::NationalStringLiteral(_)
            ) {
                return Err(syntax(&next.token));
            }
        }
        if is_word(&parser.peek_token().token, "AT") {
            return Err(ParserError::ParserError(
                "unsupported EXEC AT linked server".into(),
            ));
        }
        return Ok(execute(None, vec![expr], true, using));
    }
    if is_variable(&parser.peek_token().token) && parser.peek_nth_token(1).token == Token::Eq {
        let status = parser.next_token();
        parser.next_token();
        let Token::Word(status) = status.token else {
            unreachable!()
        };
        using.push(tagged(RETURN, Expr::Identifier(Ident::new(status.value))));
    }
    let next = parser.peek_token().token;
    let name = if is_variable(&next) {
        let Token::Word(variable) = parser.next_token().token else {
            unreachable!()
        };
        using.push(tagged(MODULE, Expr::Identifier(Ident::new(variable.value))));
        None
    } else if matches!(next, Token::Word(_)) {
        Some(parser.parse_object_name(false)?)
    } else {
        return Err(syntax(&next));
    };
    let mut parameters = Vec::new();
    let mut named = false;
    if starts_argument(&parser.peek_token().token) {
        loop {
            let argument = parse_argument(parser)?;
            let is_named = matches!(&argument, Expr::BinaryOp { left, op: BinaryOperator::Eq, .. }
                if matches!(left.as_ref(), Expr::Identifier(id) if id.value.starts_with('@')));
            if named && !is_named {
                return Err(ParserError::ParserError(super::named_then_positional(
                    parameters.len() + 1,
                )));
            }
            named |= is_named;
            parameters.push(argument);
            if !parser.consume_token(&Token::Comma) {
                break;
            }
        }
    }
    // Arguments are constants and variables: an expression is a syntax
    // error while compiling the batch. sp_set_session_context's binder
    // reports its own.
    let set_context = name.as_ref().is_some_and(|name| {
        name.0.last().is_some_and(|part| {
            part.as_ident()
                .is_some_and(|ident| ident.value.eq_ignore_ascii_case("sp_set_session_context"))
        })
    });
    if !set_context {
        for argument in &parameters {
            let value = match argument {
                Expr::BinaryOp {
                    left,
                    op: BinaryOperator::Eq,
                    right,
                } if matches!(left.as_ref(), Expr::Identifier(id) if id.value.starts_with('@')) => {
                    right.as_ref()
                }
                value => value,
            };
            if let Expr::BinaryOp { op, .. } = value {
                return Err(ParserError::ParserError(format!(
                    "Incorrect syntax near '{op}'."
                )));
            }
        }
    }
    // WITH RECOMPILE (and RESULT SETS UNDEFINED) do not change results here.
    if is_word(&parser.peek_token().token, "WITH")
        && (is_word(&parser.peek_nth_token(1).token, "RECOMPILE")
            || is_word(&parser.peek_nth_token(1).token, "RESULT"))
    {
        parser.next_token();
        loop {
            let option = parser.next_token();
            if is_word(&option.token, "RESULT") {
                let sets = parser.next_token();
                let undefined = parser.next_token();
                if !is_word(&sets.token, "SETS")
                    || !(is_word(&undefined.token, "UNDEFINED")
                        || is_word(&undefined.token, "NONE"))
                {
                    return Err(ParserError::ParserError(
                        "unsupported EXEC WITH RESULT SETS definition".into(),
                    ));
                }
            } else if !is_word(&option.token, "RECOMPILE") {
                return Err(syntax(&option.token));
            }
            if !parser.consume_token(&Token::Comma) {
                break;
            }
        }
    }
    Ok(execute(name, parameters, false, using))
}

fn execute(
    name: Option<ObjectName>,
    parameters: Vec<Expr>,
    has_parentheses: bool,
    using: Vec<ExprWithAlias>,
) -> Statement {
    Statement::Execute {
        name,
        parameters,
        has_parentheses,
        immediate: false,
        into: vec![],
        using,
        output: false,
        default: false,
    }
}

fn parse_argument(parser: &mut Parser) -> Result<Expr, ParserError> {
    let name =
        if is_variable(&parser.peek_token().token) && parser.peek_nth_token(1).token == Token::Eq {
            let Token::Word(name) = parser.next_token().token else {
                unreachable!()
            };
            parser.next_token();
            Some(Ident::new(name.value))
        } else {
            None
        };
    let token = parser.next_token();
    let mut variable = false;
    let primary = match &token.token {
        Token::Word(word) if word.quote_style.is_none() && word.value.starts_with('@') => {
            variable = true;
            Expr::Identifier(Ident::new(word.value.clone()))
        }
        Token::Word(word) if word.quote_style.is_none() && word.keyword == Keyword::DEFAULT => {
            function(DEFAULT, vec![])
        }
        Token::Word(word) if word.quote_style.is_none() && word.keyword == Keyword::NULL => {
            Expr::Value(Value::Null.into())
        }
        // A bare identifier is a character string argument.
        Token::Word(word) => Expr::Value(Value::SingleQuotedString(word.value.clone()).into()),
        Token::Number(number, long) => Expr::Value(Value::Number(number.clone(), *long).into()),
        Token::SingleQuotedString(text) => {
            Expr::Value(Value::SingleQuotedString(text.clone()).into())
        }
        Token::NationalStringLiteral(text) => {
            Expr::Value(Value::NationalStringLiteral(text.clone()).into())
        }
        Token::HexStringLiteral(text) => Expr::Value(Value::HexStringLiteral(text.clone()).into()),
        Token::Minus | Token::Plus => {
            let number = parser.next_token();
            let Token::Number(number, long) = number.token else {
                return Err(syntax(&number.token));
            };
            Expr::UnaryOp {
                op: if token.token == Token::Minus {
                    UnaryOperator::Minus
                } else {
                    UnaryOperator::Plus
                },
                expr: Box::new(Expr::Value(Value::Number(number, long).into())),
            }
        }
        other => return Err(syntax(other)),
    };
    // SQL Server accepts only constants and variables. An operator after one
    // still parses, so the callee reports it (102) like
    // sp_set_session_context's binder does.
    let mut value = primary;
    while matches!(
        parser.peek_token().token,
        Token::Plus
            | Token::Minus
            | Token::Mul
            | Token::Div
            | Token::Mod
            | Token::Ampersand
            | Token::Pipe
            | Token::Caret
            | Token::StringConcat
    ) {
        variable = false;
        let precedence = parser.get_next_precedence()?;
        value = parser.parse_infix(value, precedence)?;
    }
    let next = parser.peek_token().token;
    let value = if is_word(&next, "OUTPUT") || is_word(&next, "OUT") {
        parser.next_token();
        if !variable {
            return Err(ParserError::ParserError(super::OUTPUT_CONSTANT.into()));
        }
        function(OUTPUT, vec![value])
    } else {
        value
    };
    Ok(match name {
        Some(name) => Expr::BinaryOp {
            left: Box::new(Expr::Identifier(name)),
            op: BinaryOperator::Eq,
            right: Box::new(value),
        },
        None => value,
    })
}

fn parse_drop(parser: &mut Parser) -> Result<Statement, ParserError> {
    parser.next_token();
    parser.next_token();
    let if_exists = parser.parse_keywords(&[Keyword::IF, Keyword::EXISTS]);
    let mut proc_desc = Vec::new();
    loop {
        proc_desc.push(FunctionDesc {
            name: parser.parse_object_name(false)?,
            args: None,
        });
        if !parser.consume_token(&Token::Comma) {
            break;
        }
    }
    Ok(Statement::DropProcedure {
        if_exists,
        proc_desc,
        drop_behavior: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(sql: &str) -> Statement {
        let mut statements = crate::batch::parse(sql).unwrap();
        assert_eq!(statements.len(), 1, "{sql}");
        statements.remove(0)
    }

    #[test]
    fn calls_decode_status_names_outputs_and_defaults() {
        let statement = one("EXEC @rc = dbo.p 1, N'x', abc, -2, DEFAULT, @v OUTPUT, @a = @w OUT");
        let call = call(&statement).unwrap();
        assert_eq!(call.status, Some("@rc"));
        assert!(matches!(call.target, Target::Name(name) if name.to_string() == "dbo.p"));
        assert_eq!(call.arguments.len(), 7);
        assert_eq!(
            call.arguments[2].value.unwrap().to_string(),
            "'abc'",
            "bare identifiers are strings"
        );
        assert_eq!(call.arguments[3].value.unwrap().to_string(), "-2");
        assert_eq!(call.arguments[4].value, None);
        assert!(call.arguments[5].output && call.arguments[5].name.is_none());
        assert_eq!(call.arguments[6].name, Some("@a"));
        assert!(call.arguments[6].output);
        assert!(!call.in_try);
    }

    #[test]
    fn plain_calls_keep_sqlparser_shape() {
        let statement = one("EXEC p @key = N'k', @value = 1");
        let Statement::Execute {
            using, parameters, ..
        } = &statement
        else {
            panic!()
        };
        assert!(using.is_empty());
        assert_eq!(parameters.len(), 2);
        let statements = crate::batch::parse("EXECUTE p; EXEC q 1 SELECT 2").unwrap();
        assert_eq!(statements.len(), 3);
    }

    #[test]
    fn dynamic_and_variable_targets() {
        let statement = one("EXEC ('SELECT ' + @x) AS USER = 'dbo'");
        assert!(matches!(
            call(&statement).unwrap().target,
            Target::Dynamic(_)
        ));
        let statement = one("EXEC @r = @name 1 WITH RECOMPILE");
        let call = call(&statement).unwrap();
        assert_eq!(call.target, Target::Variable("@name"));
        assert_eq!(call.status, Some("@r"));
    }

    #[test]
    fn try_bodies_mark_calls_but_not_handlers() {
        let statement = one(
            "BEGIN TRY IF 1=1 EXEC a; EXEC b END TRY BEGIN CATCH EXEC c; EXEC sp_set_session_context N'k', 1 END CATCH",
        );
        let (body, handler) = crate::preflight::try_catch_parts(&statement).unwrap();
        let mut marked = Vec::new();
        for statement in body.iter().chain(handler) {
            struct Find<'a>(&'a mut Vec<(String, bool)>);
            impl Visitor for Find<'_> {
                type Break = ();
                fn pre_visit_statement(&mut self, s: &Statement) -> std::ops::ControlFlow<()> {
                    if let Some(call) = call(s)
                        && let Target::Name(name) = call.target
                    {
                        self.0.push((name.to_string(), call.in_try));
                    }
                    std::ops::ControlFlow::Continue(())
                }
            }
            let _ = statement.visit(&mut Find(&mut marked));
        }
        assert_eq!(
            marked,
            [
                ("a".to_string(), true),
                ("b".to_string(), true),
                ("c".to_string(), false),
                ("sp_set_session_context".to_string(), false)
            ]
        );
    }

    #[test]
    fn compilation_errors_have_sql_server_numbers() {
        for (sql, number) in [
            ("EXEC p @a = 1, 2", 119),
            ("EXEC p 1 OUTPUT", 179),
            ("SELECT 1; CREATE PROCEDURE p AS SELECT 1", 111),
            ("SELECT 1; CREATE OR ALTER PROC p AS SELECT 1", 111),
            ("EXEC p 1 + 1, 2", 102),
            ("EXEC p @a = 1 + 1", 102),
        ] {
            let error = crate::batch::parse(sql).unwrap_err();
            let error = error
                .downcast_ref::<msduck_core::diagnostic::SqlError>()
                .unwrap_or_else(|| panic!("{sql}: {error}"));
            assert_eq!(error.number, number, "{sql}");
        }
    }

    #[test]
    fn drop_procedure_lists_names() {
        let statement = one("DROP PROC IF EXISTS a, dbo.b");
        let Statement::DropProcedure {
            if_exists,
            proc_desc,
            ..
        } = statement
        else {
            panic!()
        };
        assert!(if_exists);
        assert_eq!(proc_desc.len(), 2);
    }
}
