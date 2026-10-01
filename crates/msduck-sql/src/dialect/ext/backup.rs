//! Syntax for BACKUP and RESTORE.
//!
//! SQL Server's `BACKUP DATABASE|LOG ... TO DISK = ...` and
//! `RESTORE DATABASE|LOG|HEADERONLY|FILELISTONLY|VERIFYONLY|LABELONLY ...
//! FROM DISK = ...` have no sqlparser equivalent, so they travel through the
//! batch as an extension carrier (see docs/extension-hooks.md). Every value
//! that may be a variable or an expression (database name, devices, option
//! values, MOVE pairs) stays in the carrier's argument list, so variable
//! checks and binding see it; the payload records where each one belongs.
use super::{carrier, custom};
use sqlparser::{
    ast::{Expr, Ident, Statement},
    parser::{Parser, ParserError},
    tokenizer::Token,
};

/// The carrier kind of BACKUP and RESTORE statements.
pub const KIND: &str = "backup";

/// What the statement does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    BackupDatabase,
    BackupLog,
    RestoreDatabase,
    RestoreLog,
    RestoreHeaderOnly,
    RestoreFileListOnly,
    RestoreVerifyOnly,
    RestoreLabelOnly,
}

impl Operation {
    const ALL: [(Self, &'static str); 8] = [
        (Self::BackupDatabase, "BACKUP DATABASE"),
        (Self::BackupLog, "BACKUP LOG"),
        (Self::RestoreDatabase, "RESTORE DATABASE"),
        (Self::RestoreLog, "RESTORE LOG"),
        (Self::RestoreHeaderOnly, "RESTORE HEADERONLY"),
        (Self::RestoreFileListOnly, "RESTORE FILELISTONLY"),
        (Self::RestoreVerifyOnly, "RESTORE VERIFYONLY"),
        (Self::RestoreLabelOnly, "RESTORE LABELONLY"),
    ];

    /// The statement as SQL Server names it in messages such as 3013.
    pub fn statement(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(operation, _)| *operation == self)
            .map(|(_, text)| *text)
            .unwrap_or_default()
    }

    fn parse(text: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .find(|(_, name)| *name == text)
            .map(|(operation, _)| *operation)
    }

    pub fn is_backup(self) -> bool {
        matches!(self, Self::BackupDatabase | Self::BackupLog)
    }
}

/// A value in the statement: a name written as an identifier, or an
/// expression (literal or variable) at an index of the carrier's arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Operand {
    Name(String),
    Arg(usize),
}

/// A backup device: `DISK = value`, `URL = value` or `TAPE = value`, or a
/// logical backup device written as a bare name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    /// `DISK`, `URL`, `TAPE`, or empty for a logical device name.
    pub kind: String,
    pub value: Operand,
}

/// One WITH option: its upper-case name and an optional `= value`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opt {
    pub name: String,
    pub value: Option<Operand>,
}

/// A decoded BACKUP or RESTORE statement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub operation: Operation,
    pub database: Option<Operand>,
    /// FILE or FILEGROUP clauses before TO/FROM (partial backups and
    /// piecemeal restores), kept so the runtime can refuse them.
    pub file_clauses: Vec<String>,
    pub devices: Vec<Device>,
    /// `MIRROR TO` was present.
    pub mirror: bool,
    pub options: Vec<Opt>,
    /// `MOVE logical TO physical` pairs, in order.
    pub moves: Vec<(Operand, Operand)>,
}

impl Request {
    /// The value of the last option with this name, if any; `Some(None)` when
    /// it was written without a value.
    pub fn option(&self, name: &str) -> Option<Option<&Operand>> {
        self.options
            .iter()
            .rev()
            .find(|option| option.name == name)
            .map(|option| option.value.as_ref())
    }

    pub fn has(&self, name: &str) -> bool {
        self.option(name).is_some()
    }

    fn encode(&self) -> String {
        let mut lines = vec![format!("op={}", self.operation.statement())];
        if let Some(database) = &self.database {
            lines.push(format!("db={}", operand(database)));
        }
        for clause in &self.file_clauses {
            lines.push(format!("file={}", escape(clause)));
        }
        for device in &self.devices {
            lines.push(format!("device={}:{}", device.kind, operand(&device.value)));
        }
        if self.mirror {
            lines.push("mirror".into());
        }
        for option in &self.options {
            match &option.value {
                Some(value) => lines.push(format!("option={}:{}", option.name, operand(value))),
                None => lines.push(format!("option={}", option.name)),
            }
        }
        for (logical, physical) in &self.moves {
            lines.push(format!("move={}:{}", operand(logical), operand(physical)));
        }
        lines.join("\n")
    }

    fn decode(payload: &str) -> Option<Self> {
        let mut request = Request {
            operation: Operation::BackupDatabase,
            database: None,
            file_clauses: vec![],
            devices: vec![],
            mirror: false,
            options: vec![],
            moves: vec![],
        };
        let mut operation = None;
        for line in payload.lines() {
            let (key, value) = line.split_once('=').unwrap_or((line, ""));
            match key {
                "op" => operation = Operation::parse(value),
                "db" => request.database = Some(parse_operand(value)?),
                "file" => request.file_clauses.push(unescape(value)),
                "device" => {
                    let (kind, value) = value.split_once(':')?;
                    request.devices.push(Device {
                        kind: kind.into(),
                        value: parse_operand(value)?,
                    });
                }
                "mirror" => request.mirror = true,
                "option" => {
                    let (name, value) = match value.split_once(':') {
                        Some((name, value)) => (name, Some(parse_operand(value)?)),
                        None => (value, None),
                    };
                    request.options.push(Opt {
                        name: name.into(),
                        value,
                    });
                }
                "move" => {
                    // Operands never contain an unescaped ':' after their tag,
                    // so split after the first operand's tag and value.
                    let (logical, physical) = split_operands(value)?;
                    request
                        .moves
                        .push((parse_operand(logical)?, parse_operand(physical)?));
                }
                _ => return None,
            }
        }
        request.operation = operation?;
        Some(request)
    }
}

fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace(':', "\\c")
}

fn unescape(value: &str) -> String {
    let mut out = String::new();
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('c') => out.push(':'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

fn operand(value: &Operand) -> String {
    match value {
        Operand::Name(name) => format!("n{}", escape(name)),
        Operand::Arg(index) => format!("a{index}"),
    }
}

fn parse_operand(value: &str) -> Option<Operand> {
    match value.split_at_checked(1)? {
        ("n", name) => Some(Operand::Name(unescape(name))),
        ("a", index) => Some(Operand::Arg(index.parse().ok()?)),
        _ => None,
    }
}

fn split_operands(value: &str) -> Option<(&str, &str)> {
    value.split_once(':')
}

/// Decode a BACKUP or RESTORE carrier and its argument expressions.
pub fn request(statement: &Statement) -> Option<(Request, Vec<&Expr>)> {
    let custom = custom(statement)?;
    if custom.kind != KIND {
        return None;
    }
    Some((Request::decode(custom.payload)?, custom.args))
}

/// Parse a statement this feature owns, or decline without consuming tokens.
pub fn parse(parser: &mut Parser) -> Option<Result<Statement, ParserError>> {
    let first = word(&parser.peek_token().token)?;
    if first != "BACKUP" && first != "RESTORE" {
        return None;
    }
    // Only claim the statement when the next word names a BACKUP or
    // RESTORE form, so an identifier such as a column named `backup` in
    // another context is never consumed.
    let second = word(&parser.peek_nth_token(1).token)?;
    let operation = match (first.as_str(), second.as_str()) {
        ("BACKUP", "DATABASE") => Operation::BackupDatabase,
        ("BACKUP", "LOG") => Operation::BackupLog,
        ("RESTORE", "DATABASE") => Operation::RestoreDatabase,
        ("RESTORE", "LOG") => Operation::RestoreLog,
        ("RESTORE", "HEADERONLY") => Operation::RestoreHeaderOnly,
        ("RESTORE", "FILELISTONLY") => Operation::RestoreFileListOnly,
        ("RESTORE", "VERIFYONLY") => Operation::RestoreVerifyOnly,
        ("RESTORE", "LABELONLY") => Operation::RestoreLabelOnly,
        _ => return None,
    };
    parser.next_token();
    parser.next_token();
    Some(parse_rest(parser, operation))
}

/// Whether this feature validates `statement` itself. Carriers are owned by
/// the dispatcher already.
pub fn owns(_statement: &Statement) -> bool {
    false
}

fn word(token: &Token) -> Option<String> {
    match token {
        Token::Word(word) if word.quote_style.is_none() => Some(word.value.to_ascii_uppercase()),
        _ => None,
    }
}

fn next_word_is(parser: &Parser, expected: &str) -> bool {
    word(&parser.peek_token().token).is_some_and(|word| word == expected)
}

fn expect_word(parser: &mut Parser, expected: &str) -> Result<(), ParserError> {
    if next_word_is(parser, expected) {
        parser.next_token();
        Ok(())
    } else {
        parser.expected(expected, parser.peek_token())
    }
}

/// A value: a variable or literal expression becomes an argument.
fn value(parser: &mut Parser, args: &mut Vec<Expr>) -> Result<Operand, ParserError> {
    let expr = parser.parse_expr()?;
    args.push(expr);
    Ok(Operand::Arg(args.len() - 1))
}

/// A database name: an identifier, or a variable holding the name.
fn database(parser: &mut Parser, args: &mut Vec<Expr>) -> Result<Operand, ParserError> {
    if let Token::Word(word) = &parser.peek_token().token
        && word.quote_style.is_none()
        && word.value.starts_with('@')
    {
        return value(parser, args);
    }
    let ident: Ident = parser.parse_identifier()?;
    Ok(Operand::Name(ident.value))
}

fn parse_rest(parser: &mut Parser, operation: Operation) -> Result<Statement, ParserError> {
    let mut args = Vec::new();
    let mut request = Request {
        operation,
        database: None,
        file_clauses: vec![],
        devices: vec![],
        mirror: false,
        options: vec![],
        moves: vec![],
    };
    if matches!(
        operation,
        Operation::BackupDatabase
            | Operation::BackupLog
            | Operation::RestoreDatabase
            | Operation::RestoreLog
    ) {
        request.database = Some(database(parser, &mut args)?);
        // FILE = ... / FILEGROUP = ... / PAGE = ... / READ_WRITE_FILEGROUPS
        // before TO or FROM.
        while let Some(next) = word(&parser.peek_token().token).filter(|next| {
            matches!(
                next.as_str(),
                "FILE" | "FILEGROUP" | "PAGE" | "READ_WRITE_FILEGROUPS"
            )
        }) {
            parser.next_token();
            if parser.consume_token(&Token::Eq) {
                parser.parse_expr()?;
            }
            request.file_clauses.push(next);
            if !parser.consume_token(&Token::Comma) {
                break;
            }
        }
    }
    let direction = if operation.is_backup() { "TO" } else { "FROM" };
    // RESTORE DATABASE name WITH RECOVERY has no FROM clause.
    let has_devices = if operation.is_backup() {
        expect_word(parser, direction)?;
        true
    } else if next_word_is(parser, direction) {
        parser.next_token();
        true
    } else {
        false
    };
    if has_devices {
        request.devices = devices(parser, &mut args)?;
        while next_word_is(parser, "MIRROR") {
            parser.next_token();
            expect_word(parser, "TO")?;
            devices(parser, &mut args)?;
            request.mirror = true;
        }
    }
    if parser.parse_keyword(sqlparser::keywords::Keyword::WITH) {
        loop {
            let token = parser.next_token();
            let Some(name) = word(&token.token) else {
                return parser.expected("a BACKUP or RESTORE option", token);
            };
            if name == "MOVE" {
                let logical = value(parser, &mut args)?;
                expect_word(parser, "TO")?;
                let physical = value(parser, &mut args)?;
                request.moves.push((logical, physical));
            } else if parser.consume_token(&Token::Eq) {
                let value = value(parser, &mut args)?;
                request.options.push(Opt {
                    name,
                    value: Some(value),
                });
            } else {
                // COMPRESSION (ALGORITHM = ...), ENCRYPTION (...) and the
                // like: skip the parenthesized settings.
                if parser.peek_token().token == Token::LParen {
                    let mut depth = 0usize;
                    loop {
                        let token = parser.next_token();
                        match token.token {
                            Token::LParen => depth += 1,
                            Token::RParen => {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                            }
                            Token::EOF => return parser.expected(")", token),
                            _ => {}
                        }
                    }
                }
                request.options.push(Opt { name, value: None });
            }
            if !parser.consume_token(&Token::Comma) {
                break;
            }
        }
    }
    Ok(carrier(KIND, &request.encode(), args))
}

fn devices(parser: &mut Parser, args: &mut Vec<Expr>) -> Result<Vec<Device>, ParserError> {
    let mut devices = Vec::new();
    loop {
        let kind = word(&parser.peek_token().token);
        let named = matches!(kind.as_deref(), Some("DISK" | "URL" | "TAPE"))
            && parser.peek_nth_token(1).token == Token::Eq;
        if named {
            parser.next_token();
            parser.next_token();
            devices.push(Device {
                kind: kind.unwrap_or_default(),
                value: value(parser, args)?,
            });
        } else {
            // A logical backup device, by name or in a variable.
            devices.push(Device {
                kind: String::new(),
                value: database(parser, args)?,
            });
        }
        if !parser.consume_token(&Token::Comma) {
            break;
        }
    }
    Ok(devices)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::ext::owns;

    fn parse_one(sql: &str) -> Statement {
        let mut statements = crate::batch::parse(sql).unwrap();
        assert_eq!(statements.len(), 1, "{sql}");
        statements.remove(0)
    }

    #[test]
    fn backup_with_options_round_trips() {
        let statement = parse_one(
            "BACKUP DATABASE foo TO DISK=N'/tmp/foo.bak' WITH FORMAT, INIT, NAME=N'foo', DESCRIPTION = N'd', COMPRESSION, COPY_ONLY, STATS = 5, CHECKSUM",
        );
        assert!(owns(&statement));
        let (request, args) = request(&statement).unwrap();
        assert_eq!(request.operation, Operation::BackupDatabase);
        assert_eq!(request.database, Some(Operand::Name("foo".into())));
        assert_eq!(request.devices.len(), 1);
        assert_eq!(request.devices[0].kind, "DISK");
        let names: Vec<_> = request.options.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "FORMAT",
                "INIT",
                "NAME",
                "DESCRIPTION",
                "COMPRESSION",
                "COPY_ONLY",
                "STATS",
                "CHECKSUM"
            ]
        );
        assert_eq!(args.len(), 4);
        assert!(request.has("COPY_ONLY"));
        assert_eq!(request.option("STATS"), Some(Some(&Operand::Arg(3))));
    }

    #[test]
    fn restore_forms_parse() {
        let statement = parse_one(
            "RESTORE DATABASE [foo copy] FROM DISK=N'/tmp/foo.bak' WITH REPLACE, MOVE N'foo' TO N'/x/a.mdf', MOVE N'foo_log' TO N'/x/a.ldf', RECOVERY, STATS",
        );
        let (restore, args) = request(&statement).unwrap();
        assert_eq!(restore.operation, Operation::RestoreDatabase);
        assert_eq!(restore.database, Some(Operand::Name("foo copy".into())));
        assert_eq!(restore.moves.len(), 2);
        assert_eq!(args.len(), 5);
        assert_eq!(restore.option("STATS"), Some(None));
        for (sql, operation) in [
            (
                "RESTORE HEADERONLY FROM DISK=N'/tmp/foo.bak'",
                Operation::RestoreHeaderOnly,
            ),
            (
                "restore filelistonly from disk = @path with file = 2",
                Operation::RestoreFileListOnly,
            ),
            ("BACKUP LOG foo TO DISK='x'", Operation::BackupLog),
        ] {
            let (parsed, _) = request(&parse_one(sql)).unwrap();
            assert_eq!(parsed.operation, operation, "{sql}");
        }
    }

    #[test]
    fn variables_stay_arguments_and_statements_separate() {
        let statements = crate::batch::parse(
            "DECLARE @d sysname = N'foo', @p nvarchar(100) = N'/tmp/f.bak'; BACKUP DATABASE @d TO DISK = @p SELECT 1",
        )
        .unwrap();
        assert_eq!(statements.len(), 3);
        let (request, args) = request(&statements[1]).unwrap();
        assert_eq!(request.database, Some(Operand::Arg(0)));
        assert_eq!(args[0].to_string(), "@d");
        assert_eq!(args[1].to_string(), "@p");
    }

    #[test]
    fn other_statements_are_declined() {
        let statement = parse_one("SELECT backup FROM t");
        assert!(request(&statement).is_none());
    }

    #[test]
    fn payload_escapes_names() {
        let request = Request {
            operation: Operation::RestoreDatabase,
            database: Some(Operand::Name("a:b\\c\nd".into())),
            file_clauses: vec![],
            devices: vec![],
            mirror: false,
            options: vec![],
            moves: vec![(Operand::Name("x:y".into()), Operand::Arg(2))],
        };
        assert_eq!(Request::decode(&request.encode()), Some(request));
    }
}
