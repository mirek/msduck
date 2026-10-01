//! Syntax for: isolation levels, SAVE TRANSACTION, named transactions,
//! WAITFOR and DBCC USEROPTIONS.
//!
//! Statements without an sqlparser equivalent travel as carriers (see
//! `docs/extension-hooks.md`) and are decoded with [`request`]. Literal
//! diagnostics that SQL Server raises while compiling a batch (an invalid
//! WAITFOR time string, 148; a transaction or savepoint name longer than 32
//! characters, 103) become an error carrier, which [`compile_error`] finds
//! before any statement of the batch runs. See `docs/gaps-transactions.md`.
use msduck_core::diagnostic::SqlError;
use sqlparser::{
    ast::{Expr, Ident, Statement, Value, ValueWithSpan, Visit, Visitor},
    parser::{Parser, ParserError},
    tokenizer::{Token, Word},
};
use std::ops::ControlFlow;

const PREFIX: &str = "transactions.";

/// The longest transaction or savepoint name SQL Server accepts, in
/// characters. Longer variable values are truncated to it.
pub const NAME_LIMIT: usize = 32;

/// A transaction or savepoint name: a literal identifier, or a variable
/// whose value is read when the statement runs.
#[derive(Clone, Debug, PartialEq)]
pub enum Name<'a> {
    Literal(&'a str),
    Variable(&'a Expr),
}

/// WAITFOR DELAY waits for an interval; WAITFOR TIME until a time of day.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wait {
    Delay,
    Time,
}

/// The argument of WAITFOR: a validated literal (milliseconds) or a variable.
#[derive(Clone, Debug, PartialEq)]
pub enum WaitValue<'a> {
    Milliseconds(u32),
    Variable(&'a Expr),
}

/// A statement this feature owns, decoded from its carrier.
#[derive(Clone, Debug, PartialEq)]
pub enum Request<'a> {
    /// `BEGIN TRAN[SACTION] name [WITH MARK ['description']]`.
    Begin(Name<'a>),
    /// `COMMIT TRAN[SACTION] name`; SQL Server ignores the name.
    Commit(Name<'a>),
    /// `ROLLBACK TRAN[SACTION] @variable`. A literal name is parsed as
    /// `Statement::Rollback` with a savepoint.
    Rollback(Name<'a>),
    /// `SAVE TRAN[SACTION] name`.
    Save(Name<'a>),
    WaitFor(Wait, WaitValue<'a>),
    /// `DBCC USEROPTIONS [WITH NO_INFOMSGS]`; `true` suppresses the
    /// completion message.
    UserOptions {
        no_infomsgs: bool,
    },
    /// A diagnostic SQL Server raises while compiling the batch.
    Error(SqlError),
}

/// Parse a statement this feature owns, or decline without consuming tokens.
pub fn parse(parser: &mut Parser) -> Option<Result<Statement, ParserError>> {
    let first = keyword(&parser.peek_token_ref().token)?;
    match first.as_str() {
        "SAVE" if transaction_keyword(parser, 1) => Some(parse_save(parser)),
        "BEGIN" | "COMMIT" | "ROLLBACK"
            if transaction_keyword(parser, 1) && name_follows(parser, 2) =>
        {
            Some(parse_named(parser, &first))
        }
        "WAITFOR" => Some(parse_waitfor(parser)),
        "DBCC" if matches!(keyword(&parser.peek_nth_token_ref(1).token), Some(word) if word == "USEROPTIONS") => {
            Some(parse_dbcc(parser))
        }
        _ => None,
    }
}

/// Whether this feature validates `statement` itself. Carriers are owned by
/// the extension dispatcher already; named ROLLBACK needs no exemption.
pub fn owns(_statement: &Statement) -> bool {
    false
}

/// Decode a statement of this feature.
pub fn request(statement: &Statement) -> Option<Request<'_>> {
    let custom = super::custom(statement)?;
    let kind = custom.kind.strip_prefix(PREFIX)?;
    let name = || -> Option<Name<'_>> {
        match custom.args.first()? {
            Expr::Value(ValueWithSpan {
                value: Value::SingleQuotedString(name),
                ..
            }) => Some(Name::Literal(name)),
            variable => Some(Name::Variable(variable)),
        }
    };
    Some(match kind {
        "begin" => Request::Begin(name()?),
        "commit" => Request::Commit(name()?),
        "rollback" => Request::Rollback(name()?),
        "save" => Request::Save(name()?),
        "waitfor" => {
            let (wait, literal) = match custom.payload.split_once(':') {
                Some((wait, literal)) => (wait, Some(literal.parse().ok()?)),
                None => (custom.payload, None),
            };
            let wait = match wait {
                "delay" => Wait::Delay,
                "time" => Wait::Time,
                _ => return None,
            };
            let value = match literal {
                Some(milliseconds) => WaitValue::Milliseconds(milliseconds),
                None => WaitValue::Variable(custom.args.first()?),
            };
            Request::WaitFor(wait, value)
        }
        "useroptions" => Request::UserOptions {
            no_infomsgs: custom.payload == "no_infomsgs",
        },
        "error" => {
            let mut parts = custom.payload.splitn(4, '|');
            let number = parts.next()?.parse().ok()?;
            let state = parts.next()?.parse().ok()?;
            let severity = parts.next()?.parse().ok()?;
            let mut error = SqlError::new(number, state, parts.next()?);
            error.severity = severity;
            Request::Error(error)
        }
        _ => return None,
    })
}

/// The first compile-time diagnostic of this feature anywhere in a batch,
/// including statements nested in IF, WHILE, BEGIN...END and TRY blocks.
pub fn compile_error(statements: &[Statement]) -> Option<SqlError> {
    struct Find(Option<SqlError>);
    impl Visitor for Find {
        type Break = ();
        fn pre_visit_statement(&mut self, statement: &Statement) -> ControlFlow<()> {
            if let Some(Request::Error(error)) = request(statement) {
                self.0 = Some(error);
                return ControlFlow::Break(());
            }
            ControlFlow::Continue(())
        }
    }
    let mut find = Find(None);
    for statement in statements {
        if statement.visit(&mut find).is_break() {
            break;
        }
    }
    find.0
}

/// The interval or time of day a WAITFOR time string denotes, in
/// milliseconds, or `None` when SQL Server rejects it. Accepted forms are
/// `h[h]:m[m][:s[s][.f[f[f]]|:m[m[m]]]]` with an optional `AM`/`PM` suffix,
/// surrounding blanks, and the empty string (zero). Hours run 0-23, so the
/// interval is shorter than a day; no date part is allowed.
pub fn time_string(text: &str) -> Option<u32> {
    let text = text.trim_matches(' ');
    if text.is_empty() {
        return Some(0);
    }
    let upper = text.to_ascii_uppercase();
    let (clock, meridiem) = if let Some(clock) = upper.strip_suffix("AM") {
        (clock.trim_end_matches(' '), Some(false))
    } else if let Some(clock) = upper.strip_suffix("PM") {
        (clock.trim_end_matches(' '), Some(true))
    } else {
        (upper.as_str(), None)
    };
    let number = |digits: &str, max_len: usize| -> Option<u32> {
        (!digits.is_empty()
            && digits.len() <= max_len
            && digits.bytes().all(|b| b.is_ascii_digit()))
        .then(|| digits.parse().ok())
        .flatten()
    };
    // Split off a fraction introduced by '.' after the seconds.
    let (clock, fraction) = match clock.split_once('.') {
        Some((clock, fraction)) => (clock, Some(fraction)),
        None => (clock, None),
    };
    let fields: Vec<&str> = clock.split(':').collect();
    let (hours, minutes, seconds, milliseconds) = match (fields.as_slice(), fraction) {
        ([h, m], None) => (number(h, 2)?, number(m, 2)?, 0, 0),
        ([h, m, s], None) => (number(h, 2)?, number(m, 2)?, number(s, 2)?, 0),
        ([h, m, s], Some(f)) => {
            let digits = number(f, 3)?;
            let scaled = digits * 10u32.pow(3 - f.len() as u32);
            (number(h, 2)?, number(m, 2)?, number(s, 2)?, scaled)
        }
        ([h, m, s, ms], None) => (number(h, 2)?, number(m, 2)?, number(s, 2)?, number(ms, 3)?),
        _ => return None,
    };
    let hours = match meridiem {
        None if hours <= 23 => hours,
        Some(false) if hours <= 12 => hours % 12,
        Some(true) if (1..=12).contains(&hours) => hours % 12 + 12,
        _ => return None,
    };
    if minutes > 59 || seconds > 59 {
        return None;
    }
    Some(((hours * 60 + minutes) * 60 + seconds) * 1000 + milliseconds)
}

/// An unquoted word, upper-cased.
fn keyword(token: &Token) -> Option<String> {
    match token {
        Token::Word(Word {
            value,
            quote_style: None,
            ..
        }) => Some(value.to_ascii_uppercase()),
        _ => None,
    }
}

fn transaction_keyword(parser: &Parser, n: usize) -> bool {
    matches!(
        keyword(&parser.peek_nth_token_ref(n).token).as_deref(),
        Some("TRAN" | "TRANSACTION")
    )
}

/// Whether token `n` can be a transaction or savepoint name: a variable, a
/// quoted identifier or an unquoted word that is not a reserved keyword. A
/// reserved word after `BEGIN TRAN` starts the next statement.
fn name_follows(parser: &Parser, n: usize) -> bool {
    match &parser.peek_nth_token_ref(n).token {
        Token::Word(Word {
            quote_style: Some(_),
            ..
        }) => true,
        Token::Word(Word { value, .. }) => {
            value.starts_with('@') || !reserved(&value.to_ascii_uppercase())
        }
        _ => false,
    }
}

/// Transact-SQL reserved keywords, which cannot be unquoted names.
fn reserved(word: &str) -> bool {
    const RESERVED: &[&str] = &[
        "ADD",
        "ALL",
        "ALTER",
        "AND",
        "ANY",
        "AS",
        "ASC",
        "AUTHORIZATION",
        "BACKUP",
        "BEGIN",
        "BETWEEN",
        "BREAK",
        "BROWSE",
        "BULK",
        "BY",
        "CASCADE",
        "CASE",
        "CHECK",
        "CHECKPOINT",
        "CLOSE",
        "CLUSTERED",
        "COALESCE",
        "COLLATE",
        "COLUMN",
        "COMMIT",
        "COMPUTE",
        "CONSTRAINT",
        "CONTAINS",
        "CONTAINSTABLE",
        "CONTINUE",
        "CONVERT",
        "CREATE",
        "CROSS",
        "CURRENT",
        "CURRENT_DATE",
        "CURRENT_TIME",
        "CURRENT_TIMESTAMP",
        "CURRENT_USER",
        "CURSOR",
        "DATABASE",
        "DBCC",
        "DEALLOCATE",
        "DECLARE",
        "DEFAULT",
        "DELETE",
        "DENY",
        "DESC",
        "DISK",
        "DISTINCT",
        "DISTRIBUTED",
        "DOUBLE",
        "DROP",
        "DUMP",
        "ELSE",
        "END",
        "ERRLVL",
        "ESCAPE",
        "EXCEPT",
        "EXEC",
        "EXECUTE",
        "EXISTS",
        "EXIT",
        "EXTERNAL",
        "FETCH",
        "FILE",
        "FILLFACTOR",
        "FOR",
        "FOREIGN",
        "FREETEXT",
        "FREETEXTTABLE",
        "FROM",
        "FULL",
        "FUNCTION",
        "GOTO",
        "GRANT",
        "GROUP",
        "HAVING",
        "HOLDLOCK",
        "IDENTITY",
        "IDENTITY_INSERT",
        "IDENTITYCOL",
        "IF",
        "IN",
        "INDEX",
        "INNER",
        "INSERT",
        "INTERSECT",
        "INTO",
        "IS",
        "JOIN",
        "KEY",
        "KILL",
        "LEFT",
        "LIKE",
        "LINENO",
        "LOAD",
        "MERGE",
        "NATIONAL",
        "NOCHECK",
        "NONCLUSTERED",
        "NOT",
        "NULL",
        "NULLIF",
        "OF",
        "OFF",
        "OFFSETS",
        "ON",
        "OPEN",
        "OPENDATASOURCE",
        "OPENQUERY",
        "OPENROWSET",
        "OPENXML",
        "OPTION",
        "OR",
        "ORDER",
        "OUTER",
        "OVER",
        "PERCENT",
        "PIVOT",
        "PLAN",
        "PRECISION",
        "PRIMARY",
        "PRINT",
        "PROC",
        "PROCEDURE",
        "PUBLIC",
        "RAISERROR",
        "READ",
        "READTEXT",
        "RECONFIGURE",
        "REFERENCES",
        "REPLICATION",
        "RESTORE",
        "RESTRICT",
        "RETURN",
        "REVERT",
        "REVOKE",
        "RIGHT",
        "ROLLBACK",
        "ROWCOUNT",
        "ROWGUIDCOL",
        "RULE",
        "SAVE",
        "SCHEMA",
        "SECURITYAUDIT",
        "SELECT",
        "SEMANTICKEYPHRASETABLE",
        "SEMANTICSIMILARITYDETAILSTABLE",
        "SEMANTICSIMILARITYTABLE",
        "SESSION_USER",
        "SET",
        "SETUSER",
        "SHUTDOWN",
        "SOME",
        "STATISTICS",
        "SYSTEM_USER",
        "TABLE",
        "TABLESAMPLE",
        "TEXTSIZE",
        "THEN",
        "TO",
        "TOP",
        "TRAN",
        "TRANSACTION",
        "TRIGGER",
        "TRUNCATE",
        "TRY_CONVERT",
        "TSEQUAL",
        "UNION",
        "UNIQUE",
        "UNPIVOT",
        "UPDATE",
        "UPDATETEXT",
        "USE",
        "USER",
        "VALUES",
        "VARYING",
        "VIEW",
        "WAITFOR",
        "WHEN",
        "WHERE",
        "WHILE",
        "WITH",
        "WITHIN",
        "WRITETEXT",
    ];
    RESERVED.contains(&word)
}

fn text(value: &str) -> Expr {
    Expr::Value(Value::SingleQuotedString(value.into()).into())
}

fn carrier(kind: &str, payload: &str, args: Vec<Expr>) -> Statement {
    super::carrier(&format!("{PREFIX}{kind}"), payload, args)
}

fn error_carrier(error: &SqlError) -> Statement {
    carrier(
        "error",
        &format!(
            "{}|{}|{}|{}",
            error.number, error.state, error.severity, error.message
        ),
        vec![],
    )
}

/// SQL Server's diagnostic for a literal name longer than 32 characters.
pub fn name_too_long(name: &str, state: u8) -> SqlError {
    let prefix: String = name.chars().take(NAME_LIMIT + 1).collect();
    SqlError::syntax(
        103,
        state,
        format!("The identifier that starts with '{prefix}' is too long. Maximum length is 32."),
    )
}

/// Consume a name; `Err` holds the compile-time diagnostic of a literal
/// that is too long.
fn parse_name(parser: &mut Parser) -> Result<Result<Expr, SqlError>, ParserError> {
    let token = parser.next_token();
    match &token.token {
        Token::Word(Word {
            value, quote_style, ..
        }) => {
            if quote_style.is_none() && value.starts_with('@') {
                return Ok(Ok(Expr::Identifier(Ident::new(value.clone()))));
            }
            if value.chars().count() > NAME_LIMIT {
                return Ok(Err(name_too_long(value, 2)));
            }
            Ok(Ok(text(value)))
        }
        _ => parser.expected("a transaction or savepoint name", token),
    }
}

/// SQL Server's 102 for the token `near`, reported while compiling the
/// batch. The rest of the statement is skipped so the batch still parses
/// and the diagnostic keeps SQL Server's wording.
fn syntax_error(parser: &mut Parser, near: &Token) -> Statement {
    loop {
        match &parser.peek_token_ref().token {
            Token::SemiColon | Token::EOF => break,
            token if matches!(keyword(token).as_deref(), Some("END" | "ELSE")) => break,
            _ => parser.advance_token(),
        }
    }
    error_carrier(&SqlError::syntax(
        102,
        1,
        format!("Incorrect syntax near '{near}'."),
    ))
}

fn parse_save(parser: &mut Parser) -> Result<Statement, ParserError> {
    parser.next_token(); // SAVE
    let keyword = parser.next_token(); // TRAN | TRANSACTION
    if !name_follows(parser, 0) {
        return Ok(syntax_error(parser, &keyword.token));
    }
    Ok(match parse_name(parser)? {
        Ok(name) => carrier("save", "", vec![name]),
        Err(error) => error_carrier(&error),
    })
}

fn parse_named(parser: &mut Parser, first: &str) -> Result<Statement, ParserError> {
    parser.next_token(); // BEGIN | COMMIT | ROLLBACK
    parser.next_token(); // TRAN | TRANSACTION
    let name = parse_name(parser)?;
    if first == "BEGIN" && parser.parse_keyword(sqlparser::keywords::Keyword::WITH) {
        // A log mark only matters to log restores, which msduck has none of.
        let mark = parser.next_token();
        if !matches!(keyword(&mark.token).as_deref(), Some("MARK")) {
            return parser.expected("MARK", mark);
        }
        if matches!(
            parser.peek_token_ref().token,
            Token::SingleQuotedString(_) | Token::NationalStringLiteral(_)
        ) {
            parser.next_token();
        }
    }
    let name = match name {
        Ok(name) => name,
        Err(error) => return Ok(error_carrier(&error)),
    };
    Ok(match (first, &name) {
        (
            "ROLLBACK",
            Expr::Value(ValueWithSpan {
                value: Value::SingleQuotedString(literal),
                ..
            }),
        ) => Statement::Rollback {
            chain: false,
            savepoint: Some(Ident::new(literal.clone())),
        },
        ("ROLLBACK", _) => carrier("rollback", "", vec![name]),
        ("COMMIT", _) => carrier("commit", "", vec![name]),
        _ => carrier("begin", "", vec![name]),
    })
}

fn parse_waitfor(parser: &mut Parser) -> Result<Statement, ParserError> {
    parser.next_token(); // WAITFOR
    let mode = parser.next_token();
    let wait = match keyword(&mode.token).as_deref() {
        Some("DELAY") => Wait::Delay,
        Some("TIME") => Wait::Time,
        _ if mode.token == Token::LParen => {
            return Err(ParserError::ParserError(
                "unsupported WAITFOR with RECEIVE or GET CONVERSATION GROUP: msduck has no Service Broker queues".into(),
            ));
        }
        _ => return Ok(syntax_error(parser, &mode.token)),
    };
    let name = if wait == Wait::Delay { "delay" } else { "time" };
    let argument = parser.next_token();
    let statement = match &argument.token {
        Token::SingleQuotedString(literal) | Token::NationalStringLiteral(literal) => {
            match time_string(literal) {
                Some(milliseconds) => carrier("waitfor", &format!("{name}:{milliseconds}"), vec![]),
                None => error_carrier(&SqlError::syntax(
                    148,
                    1,
                    format!("Incorrect time syntax in time string '{literal}' used with WAITFOR."),
                )),
            }
        }
        Token::Word(Word {
            value,
            quote_style: None,
            ..
        }) if value.starts_with('@') => carrier(
            "waitfor",
            name,
            vec![Expr::Identifier(Ident::new(value.clone()))],
        ),
        // At the end of the batch SQL Server names the last token.
        Token::EOF => return Ok(syntax_error(parser, &mode.token)),
        other => {
            let other = other.clone();
            return Ok(syntax_error(parser, &other));
        }
    };
    // Only RECEIVE and GET CONVERSATION GROUP take a TIMEOUT; anything else
    // that continues the statement is a syntax error, as in SQL Server.
    match &parser.peek_token_ref().token {
        Token::SemiColon | Token::EOF | Token::Word(_) => Ok(statement),
        other => {
            let other = other.clone();
            parser.advance_token();
            Ok(syntax_error(parser, &other))
        }
    }
}

fn parse_dbcc(parser: &mut Parser) -> Result<Statement, ParserError> {
    parser.next_token(); // DBCC
    parser.next_token(); // USEROPTIONS
    let mut no_infomsgs = false;
    if parser.parse_keyword(sqlparser::keywords::Keyword::WITH) {
        let option = parser.next_token();
        match keyword(&option.token).as_deref() {
            Some("NO_INFOMSGS") => no_infomsgs = true,
            _ => return parser.expected("NO_INFOMSGS", option),
        }
    }
    Ok(carrier(
        "useroptions",
        if no_infomsgs { "no_infomsgs" } else { "" },
        vec![],
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn statements(sql: &str) -> Vec<Statement> {
        crate::batch::parse(sql).unwrap()
    }

    fn one(sql: &str) -> Statement {
        let mut parsed = statements(sql);
        assert_eq!(parsed.len(), 1, "{sql}");
        parsed.remove(0)
    }

    #[test]
    fn time_strings_follow_the_captured_waitfor_rules() {
        for (text, expected) in [
            ("00:00:00.001", Some(1)),
            ("00:00:00.300", Some(300)),
            ("00:00:01", Some(1000)),
            ("00:00:00:200", Some(200)),
            ("0:0:0.2", Some(200)),
            ("00:00", Some(0)),
            ("", Some(0)),
            (" 00:00:00.100 ", Some(100)),
            ("00:00:00.100 AM", Some(100)),
            ("12:00:00.100 AM", Some(100)),
            ("1:2", Some(3_720_000)),
            ("23:59:59.999", Some(86_399_999)),
            ("12:30 PM", Some(45_000_000)),
            ("abc", None),
            ("25:00", None),
            ("24:00:00", None),
            ("00:60:00", None),
            ("2020-01-01 00:00:00.100", None),
            ("00:00:00.0005", None),
            ("00:00:00.1234567", None),
            ("00:00:00.100PM", None),
            ("00:00:00.100 PM", None),
            ("00:00:00:1000", None),
            ("5", None),
            (":05", None),
        ] {
            assert_eq!(time_string(text), expected, "{text:?}");
        }
    }

    #[test]
    fn waitfor_literals_validate_at_parse_and_variables_stay_visible() {
        let statement = one("WAITFOR DELAY '00:00:00.250'");
        assert_eq!(
            request(&statement),
            Some(Request::WaitFor(Wait::Delay, WaitValue::Milliseconds(250)))
        );
        let statement = one("WAITFOR TIME N'01:00'");
        assert_eq!(
            request(&statement),
            Some(Request::WaitFor(
                Wait::Time,
                WaitValue::Milliseconds(3_600_000)
            ))
        );
        let statement = one("waitfor delay @d");
        let Some(Request::WaitFor(Wait::Delay, WaitValue::Variable(Expr::Identifier(id)))) =
            request(&statement)
        else {
            panic!("{statement:?}");
        };
        assert_eq!(id.value, "@d");
        let parsed = statements("SELECT 1; IF 1 = 1 WAITFOR DELAY 'abc'; SELECT 2");
        let error = compile_error(&parsed).unwrap();
        assert_eq!((error.number, error.state, error.severity), (148, 1, 15));
        assert_eq!(
            error.message,
            "Incorrect time syntax in time string 'abc' used with WAITFOR."
        );
        for (sql, near) in [
            ("WAITFOR DELAY 5", "5"),
            ("WAITFOR DELAY '00:00:00.1' + ''", "+"),
            ("WAITFOR TIME", "TIME"),
            ("WAITFOR DELAY '00:00:00.100', TIMEOUT 5", ","),
            ("IF 1 = 1 BEGIN WAITFOR DELAY 5 END", "5"),
        ] {
            let error = compile_error(&statements(sql)).unwrap();
            assert_eq!(
                (error.number, error.state, error.severity),
                (102, 1, 15),
                "{sql}"
            );
            assert_eq!(
                error.message,
                format!("Incorrect syntax near '{near}'."),
                "{sql}"
            );
        }
    }

    #[test]
    fn named_transactions_and_savepoints() {
        assert_eq!(
            request(&one("SAVE TRANSACTION s1")),
            Some(Request::Save(Name::Literal("s1")))
        );
        assert_eq!(
            request(&one("save tran [my point]")),
            Some(Request::Save(Name::Literal("my point")))
        );
        assert!(matches!(
            request(&one("SAVE TRAN @sp")),
            Some(Request::Save(Name::Variable(_)))
        ));
        assert_eq!(
            request(&one("BEGIN TRAN t1 WITH MARK 'nightly'")),
            Some(Request::Begin(Name::Literal("t1")))
        );
        assert_eq!(
            request(&one("COMMIT TRANSACTION t1")),
            Some(Request::Commit(Name::Literal("t1")))
        );
        assert_eq!(
            one("ROLLBACK TRAN s1"),
            Statement::Rollback {
                chain: false,
                savepoint: Some(Ident::new("s1"))
            }
        );
        assert!(matches!(
            request(&one("ROLLBACK TRANSACTION @name")),
            Some(Request::Rollback(Name::Variable(_)))
        ));
        // A reserved word after BEGIN TRAN starts the next statement.
        let parsed = statements("BEGIN TRAN INSERT INTO t VALUES (1) COMMIT");
        assert_eq!(parsed.len(), 3);
        assert!(matches!(parsed[0], Statement::StartTransaction { .. }));
        // Exactly 32 characters is accepted; 33 fails while compiling.
        let name = "n".repeat(32);
        assert_eq!(
            request(&one(&format!("SAVE TRAN {name}"))),
            Some(Request::Save(Name::Literal(&name)))
        );
        let parsed = statements(&format!("SELECT 1; ROLLBACK TRAN {name}n"));
        let error = compile_error(&parsed).unwrap();
        assert_eq!((error.number, error.state, error.severity), (103, 2, 15));
        assert_eq!(
            error.message,
            "The identifier that starts with 'nnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnn' is too long. Maximum length is 32."
        );
        assert_eq!(
            compile_error(&statements("SAVE TRANSACTION"))
                .unwrap()
                .message,
            "Incorrect syntax near 'TRANSACTION'."
        );
    }

    #[test]
    fn dbcc_useroptions() {
        assert_eq!(
            request(&one("DBCC USEROPTIONS")),
            Some(Request::UserOptions { no_infomsgs: false })
        );
        assert_eq!(
            request(&one("dbcc useroptions with no_infomsgs")),
            Some(Request::UserOptions { no_infomsgs: true })
        );
    }
}
