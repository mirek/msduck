//! Syntax for application locks: `EXEC @status = sp_getapplock ...` and
//! `EXEC @status = sp_releaseapplock ...`.
//!
//! sqlparser reads `EXEC sp_getapplock ...` itself; only the return-status
//! assignment form needs help. It becomes an ordinary `Statement::Execute`
//! whose `into` names the status variable, so argument checks and binding
//! still see every argument. Other procedures keep the built-in behavior.
use sqlparser::{
    ast::{Ident, Statement},
    keywords::Keyword,
    parser::{Parser, ParserError},
    tokenizer::Token,
};

/// The procedures this feature implements, by unqualified lowercase name.
pub const PROCEDURES: [&str; 2] = ["sp_getapplock", "sp_releaseapplock"];

/// The unqualified lowercase name of an application lock procedure. SQL
/// Server resolves `sp_` names in `master` from any database and schema, so
/// `dbo.sp_getapplock`, `sys.sp_getapplock` and `master..sp_getapplock` are
/// the same procedure.
pub fn procedure(name: &str) -> Option<&'static str> {
    let last = name.rsplit('.').next()?.trim_matches(['[', ']', '"']);
    PROCEDURES
        .into_iter()
        .find(|procedure| procedure.eq_ignore_ascii_case(last))
}

/// Parse `EXEC[UTE] @status = <applock procedure> arguments`.
pub fn parse(parser: &mut Parser) -> Option<Result<Statement, ParserError>> {
    let Token::Word(exec) = &parser.peek_nth_token_ref(0).token else {
        return None;
    };
    if !matches!(exec.keyword, Keyword::EXEC | Keyword::EXECUTE) {
        return None;
    }
    let variable = match &parser.peek_nth_token_ref(1).token {
        Token::Word(word) if word.quote_style.is_none() && word.value.starts_with('@') => {
            word.value.clone()
        }
        _ => return None,
    };
    if parser.peek_nth_token_ref(2).token != Token::Eq {
        return None;
    }
    // The procedure name: up to four dot-separated parts, where `master..x`
    // leaves the schema empty.
    let mut index = 3;
    let mut last = None;
    loop {
        match &parser.peek_nth_token_ref(index).token {
            Token::Word(word) => {
                last = Some(word.value.clone());
                if parser.peek_nth_token_ref(index + 1).token != Token::Period {
                    break;
                }
            }
            Token::Period => {}
            _ => break,
        }
        index += 1;
        if index > 3 + 7 {
            return None;
        }
    }
    procedure(&last?)?;
    for _ in 0..3 {
        parser.next_token();
    }
    Some(parser.parse_execute().map(|mut statement| {
        if let Statement::Execute { into, .. } = &mut statement {
            *into = vec![Ident::new(variable)];
        }
        statement
    }))
}

/// Application lock calls use the built-in EXEC checks.
pub fn owns(_statement: &Statement) -> bool {
    false
}

/// The return-status variable of an application lock call, if any.
pub fn status_variable(statement: &Statement) -> Option<&Ident> {
    match statement {
        Statement::Execute { into, .. } => into.first(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlparser::ast::Expr;

    fn parse_one(sql: &str) -> Statement {
        let mut statements = crate::batch::parse(sql).expect("parses");
        assert_eq!(statements.len(), 1, "{sql}");
        statements.remove(0)
    }

    #[test]
    fn status_assignment_names_the_variable() {
        for sql in [
            "EXEC @rc = sp_getapplock N'foo', 'Exclusive', 'Session', 0",
            "EXECUTE @rc = dbo.sp_getapplock @Resource = N'foo', @LockMode = 'Shared'",
            "exec @rc = SYS.SP_GETAPPLOCK N'foo', 'Shared', 'Session'",
            "EXEC @rc = sp_releaseapplock N'foo', 'Session';",
        ] {
            let statement = parse_one(sql);
            let Statement::Execute {
                name, parameters, ..
            } = &statement
            else {
                panic!("{sql}: {statement:?}");
            };
            assert!(procedure(&name.as_ref().unwrap().to_string()).is_some());
            assert!(!parameters.is_empty());
            assert_eq!(status_variable(&statement).unwrap().value, "@rc");
        }
    }

    #[test]
    fn named_arguments_stay_binary_expressions() {
        let statement = parse_one("EXEC @rc = sp_getapplock @Resource = N'foo', @LockMode = 'X'");
        let Statement::Execute { parameters, .. } = statement else {
            unreachable!()
        };
        assert!(matches!(parameters[0], Expr::BinaryOp { .. }));
    }

    #[test]
    fn other_procedures_decline() {
        assert!(crate::batch::parse("EXEC @rc = dbo.other_proc 1").is_err());
        assert_eq!(procedure("master..sp_getapplock"), Some("sp_getapplock"));
        assert_eq!(
            procedure("[sys].[sp_releaseapplock]"),
            Some("sp_releaseapplock")
        );
        assert_eq!(procedure("sp_getapplocks"), None);
    }
}
