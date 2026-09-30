//! ALTER DATABASE {name | CURRENT} SET options, preserved in a
//! visitor-compatible node until root execution.
//!
//! Supported options are READ_COMMITTED_SNAPSHOT {ON | OFF} and SINGLE_USER,
//! RESTRICTED_USER or MULTI_USER, followed by an optional WITH ROLLBACK
//! IMMEDIATE, WITH ROLLBACK AFTER n [SECONDS] or WITH NO_WAIT. Other
//! ALTER DATABASE forms fail explicitly.
use sqlparser::{
    ast::*,
    keywords::Keyword,
    parser::{Parser, ParserError},
    tokenizer::Token,
};

const MARKER: &str = "__msduck_alter_database";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Setting {
    ReadCommittedSnapshot(bool),
    SingleUser,
    RestrictedUser,
    MultiUser,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Termination {
    /// No termination clause.
    Wait,
    NoWait,
    RollbackImmediate,
    RollbackAfterSeconds(u64),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// None for CURRENT.
    pub database: Option<Ident>,
    pub settings: Vec<Setting>,
    pub termination: Termination,
}

pub fn starts(parser: &Parser<'_>) -> bool {
    parser.peek_keyword(Keyword::ALTER)
        && matches!(parser.peek_nth_token(1).token, Token::Word(w) if w.keyword == Keyword::DATABASE)
}

/// The next unquoted word, upper-cased.
fn word(parser: &mut Parser<'_>) -> Option<String> {
    match parser.peek_token().token {
        Token::Word(w) if w.quote_style.is_none() => {
            parser.next_token();
            Some(w.value.to_ascii_uppercase())
        }
        _ => None,
    }
}

fn unsupported(message: impl Into<String>) -> ParserError {
    ParserError::ParserError(message.into())
}

pub fn parse_request(parser: &mut Parser<'_>) -> Result<Request, ParserError> {
    parser.expect_keyword(Keyword::ALTER)?;
    parser.expect_keyword(Keyword::DATABASE)?;
    let database = match parser.peek_token().token {
        Token::Word(w) if w.quote_style.is_none() && w.keyword == Keyword::CURRENT => {
            parser.next_token();
            None
        }
        _ => Some(parser.parse_identifier()?),
    };
    if !parser.parse_keyword(Keyword::SET) {
        return Err(unsupported(
            "unsupported ALTER DATABASE statement; only SET READ_COMMITTED_SNAPSHOT, SINGLE_USER, RESTRICTED_USER and MULTI_USER are supported",
        ));
    }
    let mut settings = Vec::new();
    loop {
        let option = word(parser).unwrap_or_default();
        settings.push(match option.as_str() {
            "READ_COMMITTED_SNAPSHOT" => match word(parser).as_deref() {
                Some("ON") => Setting::ReadCommittedSnapshot(true),
                Some("OFF") => Setting::ReadCommittedSnapshot(false),
                _ => return Err(unsupported("READ_COMMITTED_SNAPSHOT requires ON or OFF")),
            },
            "SINGLE_USER" => Setting::SingleUser,
            "RESTRICTED_USER" => Setting::RestrictedUser,
            "MULTI_USER" => Setting::MultiUser,
            _ => {
                return Err(unsupported(format!(
                    "unsupported ALTER DATABASE option {}",
                    if option.is_empty() {
                        parser.peek_token().token.to_string()
                    } else {
                        option
                    }
                )));
            }
        });
        if !parser.consume_token(&Token::Comma) {
            break;
        }
    }
    let termination = if parser.parse_keyword(Keyword::WITH) {
        match word(parser).as_deref() {
            Some("NO_WAIT") => Termination::NoWait,
            Some("ROLLBACK") => match word(parser).as_deref() {
                Some("IMMEDIATE") => Termination::RollbackImmediate,
                Some("AFTER") => {
                    let token = parser.next_token();
                    let Token::Number(seconds, false) = &token.token else {
                        return parser.expected("a number of seconds", token);
                    };
                    let seconds = seconds
                        .parse()
                        .map_err(|_| unsupported(format!("invalid ROLLBACK AFTER {seconds}")))?;
                    if matches!(parser.peek_token().token, Token::Word(ref w) if w.value.eq_ignore_ascii_case("SECONDS"))
                    {
                        parser.next_token();
                    }
                    Termination::RollbackAfterSeconds(seconds)
                }
                _ => return Err(unsupported("ROLLBACK requires IMMEDIATE or AFTER")),
            },
            _ => {
                return Err(unsupported(
                    "unsupported ALTER DATABASE termination; use ROLLBACK IMMEDIATE, ROLLBACK AFTER or NO_WAIT",
                ));
            }
        }
    } else {
        Termination::Wait
    };
    Ok(Request {
        database,
        settings,
        termination,
    })
}

fn text(value: impl Into<String>) -> Expr {
    Expr::Value(Value::SingleQuotedString(value.into()).into())
}

pub fn parse(parser: &mut Parser<'_>) -> Result<Statement, ParserError> {
    let request = parse_request(parser)?;
    let mut args = vec![
        match request.database {
            Some(name) => Expr::Identifier(name),
            None => Expr::Value(Value::Null.into()),
        },
        match request.termination {
            Termination::Wait => text("WAIT"),
            Termination::NoWait => text("NO_WAIT"),
            Termination::RollbackImmediate => text("ROLLBACK IMMEDIATE"),
            Termination::RollbackAfterSeconds(seconds) => crate::expr::number(seconds),
        },
    ];
    args.extend(request.settings.iter().map(|setting| {
        text(match setting {
            Setting::ReadCommittedSnapshot(true) => "READ_COMMITTED_SNAPSHOT ON",
            Setting::ReadCommittedSnapshot(false) => "READ_COMMITTED_SNAPSHOT OFF",
            Setting::SingleUser => "SINGLE_USER",
            Setting::RestrictedUser => "RESTRICTED_USER",
            Setting::MultiUser => "MULTI_USER",
        })
    }));
    let Expr::Function(mut function) = crate::expr::unary_function(MARKER, crate::expr::number(0))
    else {
        unreachable!()
    };
    if let FunctionArguments::List(list) = &mut function.args {
        list.args = args
            .into_iter()
            .map(|e| FunctionArg::Unnamed(FunctionArgExpr::Expr(e)))
            .collect();
    }
    Ok(Statement::Raise(RaiseStatement {
        value: Some(RaiseStatementValue::Expr(Expr::Function(function))),
    }))
}

/// The ALTER DATABASE request a parsed statement carries, if any.
pub fn request(statement: &Statement) -> Option<Request> {
    let Statement::Raise(RaiseStatement {
        value: Some(RaiseStatementValue::Expr(Expr::Function(f))),
    }) = statement
    else {
        return None;
    };
    if f.name.to_string() != MARKER {
        return None;
    }
    let FunctionArguments::List(args) = &f.args else {
        return None;
    };
    let mut expressions = args.args.iter().map(|a| match a {
        FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => Some(e),
        _ => None,
    });
    let database = match expressions.next()?? {
        Expr::Identifier(name) => Some(name.clone()),
        Expr::Value(value) if value.value == Value::Null => None,
        _ => return None,
    };
    let Expr::Value(termination) = expressions.next()?? else {
        return None;
    };
    let termination = match &termination.value {
        Value::SingleQuotedString(kind) => match kind.as_str() {
            "WAIT" => Termination::Wait,
            "NO_WAIT" => Termination::NoWait,
            "ROLLBACK IMMEDIATE" => Termination::RollbackImmediate,
            _ => return None,
        },
        Value::Number(seconds, false) => Termination::RollbackAfterSeconds(seconds.parse().ok()?),
        _ => return None,
    };
    let settings = expressions
        .map(|e| {
            let Expr::Value(value) = e? else { return None };
            let Value::SingleQuotedString(setting) = &value.value else {
                return None;
            };
            Some(match setting.as_str() {
                "READ_COMMITTED_SNAPSHOT ON" => Setting::ReadCommittedSnapshot(true),
                "READ_COMMITTED_SNAPSHOT OFF" => Setting::ReadCommittedSnapshot(false),
                "SINGLE_USER" => Setting::SingleUser,
                "RESTRICTED_USER" => Setting::RestrictedUser,
                "MULTI_USER" => Setting::MultiUser,
                _ => return None,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    if settings.is_empty() {
        return None;
    }
    Some(Request {
        database,
        settings,
        termination,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_one(sql: &str) -> Result<Request, String> {
        let statements = crate::batch::parse(sql).map_err(|e| e.to_string())?;
        assert_eq!(statements.len(), 1, "{sql}");
        request(&statements[0]).ok_or_else(|| format!("no request in {sql}"))
    }

    #[test]
    fn parses_owner_statements_and_variants() {
        let request =
            parse_one("alter database [x] set read_committed_snapshot on with rollback immediate;")
                .unwrap();
        assert_eq!(request.database.as_ref().unwrap().value, "x");
        assert_eq!(request.database.unwrap().quote_style, Some('['));
        assert_eq!(request.settings, [Setting::ReadCommittedSnapshot(true)]);
        assert_eq!(request.termination, Termination::RollbackImmediate);
        let request =
            parse_one("ALTER DATABASE [probe_db] SET SINGLE_USER WITH ROLLBACK IMMEDIATE").unwrap();
        assert_eq!(request.settings, [Setting::SingleUser]);
        let request = parse_one("ALTER DATABASE CURRENT SET MULTI_USER").unwrap();
        assert_eq!(request.database, None);
        assert_eq!(request.termination, Termination::Wait);
        let request = parse_one(
            "ALTER DATABASE probe SET SINGLE_USER, READ_COMMITTED_SNAPSHOT OFF WITH NO_WAIT",
        )
        .unwrap();
        assert_eq!(
            request.settings,
            [Setting::SingleUser, Setting::ReadCommittedSnapshot(false)]
        );
        assert_eq!(request.termination, Termination::NoWait);
        let request =
            parse_one("ALTER DATABASE probe SET RESTRICTED_USER WITH ROLLBACK AFTER 5 SECONDS")
                .unwrap();
        assert_eq!(request.termination, Termination::RollbackAfterSeconds(5));
        let request =
            parse_one("ALTER DATABASE probe SET MULTI_USER WITH ROLLBACK AFTER 2").unwrap();
        assert_eq!(request.termination, Termination::RollbackAfterSeconds(2));
    }

    #[test]
    fn statements_after_alter_database_still_parse() {
        let statements = crate::batch::parse(
            "ALTER DATABASE [p] SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE [p]; SELECT 1",
        )
        .unwrap();
        assert_eq!(statements.len(), 3);
        assert!(request(&statements[0]).is_some());
    }

    #[test]
    fn unsupported_forms_fail_explicitly() {
        for sql in [
            "ALTER DATABASE p SET ALLOW_SNAPSHOT_ISOLATION ON",
            "ALTER DATABASE p MODIFY NAME = q",
            "ALTER DATABASE p SET READ_COMMITTED_SNAPSHOT",
            "ALTER DATABASE p SET SINGLE_USER WITH ROLLBACK",
        ] {
            let error = parse_one(sql).unwrap_err();
            assert!(!error.is_empty(), "{sql}");
        }
    }
}
