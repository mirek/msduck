//! Syntax for DML triggers and DISABLE/ENABLE TRIGGER.
//!
//! - CREATE, ALTER and CREATE OR ALTER TRIGGER own their whole batch, as SQL
//!   Server requires. [`definition`] reads the header (name, target, timing,
//!   events and options) and the byte offset where the body starts; the body
//!   is ordinary batch text that the runtime parses and runs like any batch.
//! - DROP TRIGGER, DISABLE/ENABLE TRIGGER and ALTER TABLE ... DISABLE/ENABLE
//!   TRIGGER travel through the batch as carriers (see [`Command`]).
//!
//! See docs/gaps-triggers.md.
use super::{carrier, custom};
use msduck_core::diagnostic::SqlError;
use sqlparser::{
    ast::{Expr, Statement, Value},
    keywords::Keyword,
    parser::{Parser, ParserError},
    tokenizer::Token,
};
use std::ops::Range;

/// Carrier kind of the trigger management statements.
pub const KIND: &str = "triggers";

/// A DML trigger event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Event {
    Insert,
    Update,
    Delete,
}

impl Event {
    pub const ALL: [Event; 3] = [Event::Insert, Event::Update, Event::Delete];

    pub fn name(self) -> &'static str {
        match self {
            Event::Insert => "INSERT",
            Event::Update => "UPDATE",
            Event::Delete => "DELETE",
        }
    }

    pub fn parse(name: &str) -> Option<Event> {
        Event::ALL
            .into_iter()
            .find(|event| event.name().eq_ignore_ascii_case(name))
    }
}

/// Which statement introduced a trigger definition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Create,
    Alter,
    CreateOrAlter,
}

impl Action {
    /// The statement name SQL Server uses in messages such as error 111.
    pub fn statement(self) -> &'static str {
        match self {
            Action::Alter => "ALTER TRIGGER",
            Action::Create | Action::CreateOrAlter => "CREATE TRIGGER",
        }
    }
}

/// What a trigger is defined on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// A table or view, as written (one to three name parts).
    Object(Vec<String>),
    /// `ON DATABASE` or `ON ALL SERVER`: a DDL trigger.
    Ddl,
}

/// A parsed trigger header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Definition {
    pub action: Action,
    pub schema: Option<String>,
    pub name: String,
    pub target: Target,
    pub instead_of: bool,
    /// Events in the order written; duplicates are rejected with error 1034.
    pub events: Vec<Event>,
    /// Byte range that the stored definition rewrites: `ALTER` for ALTER
    /// TRIGGER, `OR ALTER` for CREATE OR ALTER, and `CREATE` otherwise.
    pub keyword: Range<usize>,
    /// Byte offset of the body, just after `AS`.
    pub body: usize,
}

impl Definition {
    pub fn has(&self, event: Event) -> bool {
        self.events.contains(&event)
    }
}

/// The text SQL Server keeps in sys.sql_modules: ALTER becomes CREATE and
/// CREATE OR ALTER drops `OR ALTER`, leaving the rest of the batch unchanged.
pub fn stored_text(sql: &str, definition: &Definition) -> String {
    let replacement = match definition.action {
        Action::Create => return sql.to_string(),
        Action::Alter => "CREATE",
        // SQL Server replaces "OR ALTER" with one blank.
        Action::CreateOrAlter => " ",
    };
    format!(
        "{}{}{}",
        &sql[..definition.keyword.start],
        replacement,
        &sql[definition.keyword.end..]
    )
}

/// Recognize a batch whose first statement is a trigger definition.
/// `None` means the batch is something else.
pub fn definition(sql: &str) -> Option<Result<Definition, SqlError>> {
    let mut lexer = Lexer::new(sql);
    let first = lexer.next();
    let action = match first.word() {
        Some("CREATE") => {
            let mut probe = lexer.clone();
            let next = probe.next();
            if next.is("OR") {
                if !probe.next().is("ALTER") || !probe.next().is("TRIGGER") {
                    return None;
                }
                Action::CreateOrAlter
            } else if next.is("TRIGGER") {
                Action::Create
            } else {
                return None;
            }
        }
        Some("ALTER") => {
            if !lexer.clone().next().is("TRIGGER") {
                return None;
            }
            Action::Alter
        }
        _ => return None,
    };
    Some(header(lexer, action, first.span.start))
}

fn header(mut lexer: Lexer<'_>, action: Action, start: usize) -> Result<Definition, SqlError> {
    let mut keyword = start..lexer.position;
    if action == Action::CreateOrAlter {
        let or = lexer.next();
        keyword = or.span.start..lexer.next().span.end;
    }
    lexer.expect("TRIGGER")?;
    let mut name = lexer.object_name()?;
    if name.len() > 2 {
        return Err(SqlError::syntax(
            166,
            1,
            format!(
                "'{}' does not allow specifying the database name as a prefix to the object name.",
                action.statement()
            ),
        ));
    }
    let trigger = name.pop().unwrap();
    let schema = name.pop();
    lexer.expect("ON")?;
    let mut probe = lexer.clone();
    let target = match probe.next().word() {
        Some("DATABASE") => {
            lexer = probe;
            Target::Ddl
        }
        Some("ALL") if probe.next().is("SERVER") => {
            lexer = probe;
            Target::Ddl
        }
        _ => {
            let parts = lexer.object_name()?;
            if parts.len() > 3 {
                return Err(lexer.near_previous());
            }
            Target::Object(parts)
        }
    };
    // WITH ENCRYPTION | EXECUTE AS ... | SCHEMABINDING | NATIVE_COMPILATION
    if lexer.peek().is("WITH") {
        lexer.next();
        loop {
            let token = lexer.next();
            match token.word() {
                Some("FOR" | "AFTER" | "INSTEAD") => {
                    lexer.back(token);
                    break;
                }
                Some(_) => {}
                None if matches!(token.kind, Kind::Comma | Kind::Text) => {}
                None => return Err(lexer.near(&token)),
            }
        }
    }
    let timing = lexer.next();
    let instead_of = match timing.word() {
        Some("FOR" | "AFTER") => false,
        Some("INSTEAD") => {
            lexer.expect("OF")?;
            true
        }
        _ => return Err(lexer.near(&timing)),
    };
    let mut events = Vec::new();
    loop {
        let token = lexer.next();
        if target == Target::Ddl {
            // DDL event names and groups are not DML events; keep them unparsed.
            if token.is("AS") || token.kind == Kind::End {
                lexer.back(token);
                break;
            }
            continue;
        }
        match token.word().and_then(Event::parse) {
            Some(event) => {
                if events.contains(&event) {
                    return Err(SqlError::syntax(
                        1034,
                        1,
                        format!(
                            "Syntax error: Duplicate specification of the action \"{}\" in the trigger declaration.",
                            event.name()
                        ),
                    ));
                }
                events.push(event);
            }
            None if token.kind == Kind::Comma && !events.is_empty() => {}
            None => {
                if events.is_empty() {
                    return Err(lexer.near(&token));
                }
                lexer.back(token);
                break;
            }
        }
    }
    if lexer.peek().is("WITH") {
        lexer.next();
        lexer.expect("APPEND")?;
    }
    if lexer.peek().is("NOT") {
        lexer.next();
        lexer.expect("FOR")?;
        lexer.expect("REPLICATION")?;
    }
    let as_token = lexer.next();
    if !as_token.is("AS") {
        return Err(lexer.near(&as_token));
    }
    let body = as_token.span.end;
    // An empty body (only blanks, comments and terminators) is a syntax error
    // near AS, as in SQL Server.
    let mut rest = lexer.clone();
    loop {
        let token = rest.next();
        match token.kind {
            Kind::End => return Err(lexer.near(&as_token)),
            Kind::Semicolon => {}
            _ => break,
        }
    }
    Ok(Definition {
        action,
        schema,
        name: trigger,
        target,
        instead_of,
        events,
        keyword,
        body,
    })
}

/// A trigger definition that is not the first statement of its batch
/// (error 111), as the statement name for the message.
pub fn misplaced(sql: &str) -> Option<&'static str> {
    if !sql
        .as_bytes()
        .windows(7)
        .any(|window| window.eq_ignore_ascii_case(b"trigger"))
    {
        return None;
    }
    let mut lexer = Lexer::new(sql);
    let mut previous: [Option<String>; 2] = [None, None];
    loop {
        let token = lexer.next();
        if token.kind == Kind::End {
            return None;
        }
        let word = token.word().map(str::to_string);
        if word.as_deref() == Some("TRIGGER") {
            match (previous[0].as_deref(), previous[1].as_deref()) {
                (Some("OR"), Some("ALTER")) => return Some("CREATE TRIGGER"),
                (_, Some("CREATE")) => return Some("CREATE TRIGGER"),
                (_, Some("ALTER")) => return Some("ALTER TRIGGER"),
                _ => {}
            }
        }
        previous = [previous[1].take(), word];
    }
}

/// The `UPDATE(column)` tests in a trigger body, as byte ranges and column
/// names. They are replaced before the body is parsed, so that batch
/// validation sees an ordinary predicate.
pub fn update_tests(body: &str) -> Vec<(Range<usize>, String)> {
    let mut lexer = Lexer::new(body);
    let mut tests = Vec::new();
    loop {
        let token = lexer.next();
        if token.kind == Kind::End {
            return tests;
        }
        if !token.is("UPDATE") {
            continue;
        }
        let mut probe = lexer.clone();
        let open = probe.next();
        let column = probe.next();
        let close = probe.next();
        if open.kind == Kind::Other
            && open.value == "("
            && column.kind == Kind::Word
            && close.kind == Kind::Other
            && close.value == ")"
        {
            tests.push((token.span.start..close.span.end, column.value));
            lexer = probe;
        }
    }
}

/// The `@@NESTLEVEL` references in a trigger body, as byte ranges.
pub fn nest_level_references(body: &str) -> Vec<Range<usize>> {
    let mut lexer = Lexer::new(body);
    let mut references = Vec::new();
    loop {
        let token = lexer.next();
        match token.kind {
            Kind::End => return references,
            _ if token.is("@@NESTLEVEL") => references.push(token.span),
            _ => {}
        }
    }
}

/// A trigger management statement carried through the batch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// DROP TRIGGER [IF EXISTS] name [, ...] [ON DATABASE | ON ALL SERVER].
    Drop {
        if_exists: bool,
        names: Vec<Vec<String>>,
        ddl: bool,
    },
    /// DISABLE/ENABLE TRIGGER {ALL | name [, ...]} ON {table | DATABASE | ALL SERVER},
    /// or ALTER TABLE table DISABLE/ENABLE TRIGGER {ALL | name [, ...]}.
    Toggle {
        enable: bool,
        names: Option<Vec<Vec<String>>>,
        target: Target,
        alter_table: bool,
    },
}

impl Command {
    fn encode(&self) -> String {
        fn names(out: &mut String, names: &[Vec<String>]) {
            for (index, name) in names.iter().enumerate() {
                if index > 0 {
                    out.push('\u{1e}');
                }
                out.push_str(&name.join("\u{1f}"));
            }
        }
        let mut out = String::new();
        match self {
            Command::Drop {
                if_exists,
                names: list,
                ddl,
            } => {
                out.push_str(&format!(
                    "drop|{}|{}|",
                    u8::from(*if_exists),
                    u8::from(*ddl)
                ));
                names(&mut out, list);
            }
            Command::Toggle {
                enable,
                names: list,
                target,
                alter_table,
            } => {
                out.push_str(&format!(
                    "toggle|{}|{}|{}|",
                    u8::from(*enable),
                    u8::from(*alter_table),
                    match target {
                        Target::Ddl => String::from("\u{1d}"),
                        Target::Object(parts) => parts.join("\u{1f}"),
                    }
                ));
                match list {
                    None => out.push('\u{1d}'),
                    Some(list) => names(&mut out, list),
                }
            }
        }
        out
    }

    fn decode(payload: &str) -> Option<Command> {
        fn names(text: &str) -> Vec<Vec<String>> {
            if text.is_empty() {
                return Vec::new();
            }
            text.split('\u{1e}')
                .map(|name| name.split('\u{1f}').map(str::to_string).collect())
                .collect()
        }
        let mut fields = payload.splitn(5, '|');
        match fields.next()? {
            "drop" => {
                let if_exists = fields.next()? == "1";
                let ddl = fields.next()? == "1";
                Some(Command::Drop {
                    if_exists,
                    names: names(fields.next()?),
                    ddl,
                })
            }
            "toggle" => {
                let enable = fields.next()? == "1";
                let alter_table = fields.next()? == "1";
                let target = match fields.next()? {
                    "\u{1d}" => Target::Ddl,
                    parts => Target::Object(parts.split('\u{1f}').map(str::to_string).collect()),
                };
                let list = match fields.next()? {
                    "\u{1d}" => None,
                    list => Some(names(list)),
                };
                Some(Command::Toggle {
                    enable,
                    names: list,
                    target,
                    alter_table,
                })
            }
            _ => None,
        }
    }

    /// Decode a trigger management carrier.
    pub fn of(statement: &Statement) -> Option<Command> {
        let custom = custom(statement)?;
        if custom.kind != KIND {
            return None;
        }
        Command::decode(custom.payload)
    }
}

/// Parse a statement this feature owns, or decline without consuming tokens.
pub fn parse(parser: &mut Parser) -> Option<Result<Statement, ParserError>> {
    let word = |n: usize| match parser.peek_nth_token(n).token {
        Token::Word(word) if word.quote_style.is_none() => Some(word.value.to_ascii_uppercase()),
        _ => None,
    };
    let first = word(0)?;
    let second = word(1);
    let command = match (first.as_str(), second.as_deref()) {
        ("DROP", Some("TRIGGER")) => parse_drop(parser),
        ("ENABLE" | "DISABLE", Some("TRIGGER")) => parse_toggle(parser),
        ("ALTER", Some("TABLE")) => {
            // ALTER TABLE name {ENABLE | DISABLE} TRIGGER: the name has one
            // to three parts separated by periods.
            let mut index = 2;
            loop {
                if !matches!(parser.peek_nth_token(index).token, Token::Word(_)) {
                    return None;
                }
                index += 1;
                if parser.peek_nth_token(index).token == Token::Period {
                    index += 1;
                } else {
                    break;
                }
            }
            if !matches!(word(index).as_deref(), Some("ENABLE" | "DISABLE"))
                || word(index + 1).as_deref() != Some("TRIGGER")
            {
                return None;
            }
            parse_alter_table(parser)
        }
        _ => return None,
    };
    Some(command.map(|command| carrier(KIND, &command.encode(), Vec::<Expr>::new())))
}

fn names(parser: &mut Parser) -> Result<Vec<Vec<String>>, ParserError> {
    let mut names = Vec::new();
    loop {
        let name = parser.parse_object_name(false)?;
        names.push(
            name.0
                .iter()
                .map(|part| {
                    part.as_ident()
                        .map(|ident| ident.value.clone())
                        .ok_or_else(|| ParserError::ParserError("invalid trigger name".into()))
                })
                .collect::<Result<Vec<_>, _>>()?,
        );
        if !parser.consume_token(&Token::Comma) {
            return Ok(names);
        }
    }
}

fn ddl_target(parser: &mut Parser) -> bool {
    if parser.parse_keyword(Keyword::DATABASE) {
        return true;
    }
    parser.parse_keywords(&[Keyword::ALL, Keyword::SERVER])
}

fn parse_drop(parser: &mut Parser) -> Result<Command, ParserError> {
    parser.next_token();
    parser.next_token();
    let if_exists = parser.parse_keywords(&[Keyword::IF, Keyword::EXISTS]);
    let names = names(parser)?;
    let ddl = if parser.parse_keyword(Keyword::ON) {
        if !ddl_target(parser) {
            return parser.expected("DATABASE or ALL SERVER", parser.peek_token());
        }
        true
    } else {
        false
    };
    Ok(Command::Drop {
        if_exists,
        names,
        ddl,
    })
}

fn toggle_names(parser: &mut Parser) -> Result<Option<Vec<Vec<String>>>, ParserError> {
    if parser.parse_keyword(Keyword::ALL) {
        Ok(None)
    } else {
        names(parser).map(Some)
    }
}

fn parse_toggle(parser: &mut Parser) -> Result<Command, ParserError> {
    let enable = parser
        .next_token()
        .token
        .to_string()
        .eq_ignore_ascii_case("ENABLE");
    parser.next_token();
    let names = toggle_names(parser)?;
    parser.expect_keyword(Keyword::ON)?;
    let target = if ddl_target(parser) {
        Target::Ddl
    } else {
        Target::Object(object(parser)?)
    };
    Ok(Command::Toggle {
        enable,
        names,
        target,
        alter_table: false,
    })
}

fn object(parser: &mut Parser) -> Result<Vec<String>, ParserError> {
    let name = parser.parse_object_name(false)?;
    name.0
        .iter()
        .map(|part| {
            part.as_ident()
                .map(|ident| ident.value.clone())
                .ok_or_else(|| ParserError::ParserError("invalid object name".into()))
        })
        .collect()
}

fn parse_alter_table(parser: &mut Parser) -> Result<Command, ParserError> {
    parser.next_token();
    parser.next_token();
    let target = object(parser)?;
    let enable = parser
        .next_token()
        .token
        .to_string()
        .eq_ignore_ascii_case("ENABLE");
    parser.next_token();
    let names = toggle_names(parser)?;
    Ok(Command::Toggle {
        enable,
        names,
        target: Target::Object(target),
        alter_table: true,
    })
}

/// Whether this feature validates `statement` itself. Carriers are always
/// owned; the feature parses nothing else.
pub fn owns(_statement: &Statement) -> bool {
    false
}

/// The trigger functions this feature lowers inside trigger bodies.
pub fn is_update_function(expr: &Expr) -> bool {
    matches!(expr, Expr::Function(function) if function.name.to_string().eq_ignore_ascii_case("UPDATE"))
}

/// Encode bytes as a T-SQL binary literal.
pub fn binary_literal(bytes: &[u8]) -> Expr {
    Expr::Value(
        Value::HexStringLiteral(bytes.iter().map(|byte| format!("{byte:02X}")).collect()).into(),
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Word,
    Text,
    Comma,
    Period,
    Semicolon,
    Other,
    End,
}

#[derive(Clone, Debug)]
struct Lexeme {
    kind: Kind,
    /// Upper-case value of an unquoted word.
    upper: Option<String>,
    /// Identifier value (unquoted words and bracketed or double-quoted names).
    value: String,
    span: Range<usize>,
}

impl Lexeme {
    fn word(&self) -> Option<&str> {
        self.upper.as_deref()
    }

    fn is(&self, keyword: &str) -> bool {
        self.word() == Some(keyword)
    }
}

/// A small lexer for trigger headers that keeps byte offsets: blanks,
/// `--` and nested `/* */` comments, words, [bracketed] and "quoted" names,
/// 'strings' and punctuation.
#[derive(Clone)]
struct Lexer<'a> {
    sql: &'a str,
    position: usize,
    previous: Option<Lexeme>,
    pending: Option<Lexeme>,
}

impl<'a> Lexer<'a> {
    fn new(sql: &'a str) -> Self {
        Self {
            sql,
            position: 0,
            previous: None,
            pending: None,
        }
    }

    fn skip_trivia(&mut self) {
        let bytes = self.sql.as_bytes();
        loop {
            while self.position < bytes.len() && bytes[self.position].is_ascii_whitespace() {
                self.position += 1;
            }
            if bytes[self.position..].starts_with(b"--") {
                while self.position < bytes.len() && bytes[self.position] != b'\n' {
                    self.position += 1;
                }
            } else if bytes[self.position..].starts_with(b"/*") {
                let mut depth = 0usize;
                while self.position < bytes.len() {
                    if bytes[self.position..].starts_with(b"/*") {
                        depth += 1;
                        self.position += 2;
                    } else if bytes[self.position..].starts_with(b"*/") {
                        depth -= 1;
                        self.position += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        self.position += 1;
                    }
                }
            } else {
                return;
            }
        }
    }

    fn quoted(&mut self, close: u8) -> String {
        let bytes = self.sql.as_bytes();
        let start = self.position + 1;
        let mut value = String::new();
        let mut index = start;
        while index < bytes.len() {
            if bytes[index] == close {
                if bytes.get(index + 1) == Some(&close) {
                    value.push(close as char);
                    index += 2;
                    continue;
                }
                index += 1;
                break;
            }
            let character = self.sql[index..].chars().next().unwrap();
            value.push(character);
            index += character.len_utf8();
        }
        self.position = index;
        value
    }

    fn next(&mut self) -> Lexeme {
        if let Some(pending) = self.pending.take() {
            self.previous = Some(pending.clone());
            return pending;
        }
        self.skip_trivia();
        let bytes = self.sql.as_bytes();
        let start = self.position;
        let lexeme = if start >= bytes.len() {
            Lexeme {
                kind: Kind::End,
                upper: None,
                value: String::new(),
                span: start..start,
            }
        } else {
            let byte = bytes[start];
            let (kind, upper, value) = match byte {
                b'[' => (Kind::Word, None, self.quoted(b']')),
                b'"' => (Kind::Word, None, self.quoted(b'"')),
                b'\'' => (Kind::Text, None, self.quoted(b'\'')),
                b'N' | b'n' if bytes.get(start + 1) == Some(&b'\'') => {
                    self.position += 1;
                    (Kind::Text, None, self.quoted(b'\''))
                }
                b',' | b'.' | b';' => {
                    self.position += 1;
                    let kind = match byte {
                        b',' => Kind::Comma,
                        b'.' => Kind::Period,
                        _ => Kind::Semicolon,
                    };
                    (kind, None, (byte as char).to_string())
                }
                _ if byte.is_ascii_alphanumeric()
                    || matches!(byte, b'_' | b'@' | b'#')
                    || byte >= 0x80 =>
                {
                    let rest = &self.sql[start..];
                    let length = rest
                        .char_indices()
                        .find(|(_, c)| !(c.is_alphanumeric() || matches!(c, '_' | '@' | '#' | '$')))
                        .map_or(rest.len(), |(index, _)| index);
                    self.position += length;
                    let value = rest[..length].to_string();
                    (Kind::Word, Some(value.to_ascii_uppercase()), value)
                }
                _ => {
                    let length = self.sql[start..].chars().next().unwrap().len_utf8();
                    self.position += length;
                    (
                        Kind::Other,
                        None,
                        self.sql[start..start + length].to_string(),
                    )
                }
            };
            Lexeme {
                kind,
                upper,
                value,
                span: start..self.position,
            }
        };
        self.previous = Some(lexeme.clone());
        lexeme
    }

    fn back(&mut self, lexeme: Lexeme) {
        self.pending = Some(lexeme);
    }

    fn peek(&mut self) -> Lexeme {
        let lexeme = self.next();
        self.back(lexeme.clone());
        lexeme
    }

    fn near(&self, lexeme: &Lexeme) -> SqlError {
        let text = if lexeme.kind == Kind::End {
            self.previous
                .as_ref()
                .map(|previous| &self.sql[previous.span.clone()])
                .unwrap_or("")
        } else {
            &self.sql[lexeme.span.clone()]
        };
        SqlError::syntax(102, 1, format!("Incorrect syntax near '{text}'."))
    }

    fn near_previous(&self) -> SqlError {
        let text = self
            .previous
            .as_ref()
            .map(|previous| &self.sql[previous.span.clone()])
            .unwrap_or("");
        SqlError::syntax(102, 1, format!("Incorrect syntax near '{text}'."))
    }

    fn expect(&mut self, keyword: &str) -> Result<(), SqlError> {
        let lexeme = self.next();
        if lexeme.is(keyword) {
            Ok(())
        } else {
            Err(self.near(&lexeme))
        }
    }

    fn object_name(&mut self) -> Result<Vec<String>, SqlError> {
        let mut parts = Vec::new();
        loop {
            let lexeme = self.next();
            if lexeme.kind != Kind::Word {
                return Err(self.near(&lexeme));
            }
            parts.push(lexeme.value);
            let next = self.next();
            if next.kind != Kind::Period {
                self.back(next);
                return Ok(parts);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(sql: &str) -> Definition {
        definition(sql)
            .expect("trigger batch")
            .expect("valid header")
    }

    #[test]
    fn headers_record_target_timing_events_and_body() {
        let sql = "/* c */ CREATE TRIGGER dbo.[t r] ON [dbo].items WITH EXECUTE AS 'x', ENCRYPTION\nAFTER INSERT, UPDATE NOT FOR REPLICATION AS\nBEGIN SELECT 1; END;";
        let definition = parsed(sql);
        assert_eq!(definition.action, Action::Create);
        assert_eq!(definition.schema.as_deref(), Some("dbo"));
        assert_eq!(definition.name, "t r");
        assert_eq!(
            definition.target,
            Target::Object(vec!["dbo".into(), "items".into()])
        );
        assert!(!definition.instead_of);
        assert_eq!(definition.events, [Event::Insert, Event::Update]);
        assert_eq!(&sql[definition.body..], "\nBEGIN SELECT 1; END;");
        assert_eq!(&sql[definition.keyword.clone()], "CREATE");
        let sql = "CREATE  OR\nALTER TRIGGER t ON x AFTER DELETE AS PRINT 1";
        assert_eq!(&sql[parsed(sql).keyword.clone()], "OR\nALTER");

        let definition = parsed("create or alter trigger t on x instead of delete as select 1");
        assert_eq!(definition.action, Action::CreateOrAlter);
        assert!(definition.instead_of);
        assert_eq!(definition.events, [Event::Delete]);
        let definition = parsed("ALTER TRIGGER t ON x FOR DELETE, INSERT AS PRINT 1");
        assert_eq!(definition.action, Action::Alter);
        assert_eq!(definition.events, [Event::Delete, Event::Insert]);
        assert_eq!(
            parsed("CREATE TRIGGER t ON DATABASE FOR CREATE_TABLE AS PRINT 1").target,
            Target::Ddl
        );
    }

    #[test]
    fn other_batches_are_not_definitions() {
        for sql in [
            "SELECT 1",
            "CREATE TABLE t(id INT)",
            "ALTER TABLE t DISABLE TRIGGER ALL",
            "CREATE OR ALTER PROCEDURE p AS SELECT 1",
            "",
        ] {
            assert!(definition(sql).is_none(), "{sql}");
        }
    }

    #[test]
    fn header_errors_match_sql_server() {
        let error = |sql: &str| definition(sql).unwrap().unwrap_err();
        let duplicate = error("CREATE TRIGGER t ON x AFTER INSERT, INSERT AS PRINT 1");
        assert_eq!((duplicate.number, duplicate.severity), (1034, 15));
        assert_eq!(
            duplicate.message,
            "Syntax error: Duplicate specification of the action \"INSERT\" in the trigger declaration."
        );
        let empty = error("CREATE TRIGGER t ON x AFTER INSERT AS  ;\n -- nothing");
        assert_eq!(
            (empty.number, empty.message.as_str()),
            (102, "Incorrect syntax near 'AS'.")
        );
        assert_eq!(error("CREATE TRIGGER t ON x INSERT AS PRINT 1").number, 102);
        assert_eq!(
            error("CREATE TRIGGER db.dbo.t ON x AFTER INSERT AS PRINT 1").number,
            166
        );
    }

    #[test]
    fn stored_text_records_create() {
        let text = |sql: &str| stored_text(sql, &parsed(sql));
        assert_eq!(
            text("ALTER TRIGGER t ON x AFTER UPDATE AS PRINT 'z';"),
            "CREATE TRIGGER t ON x AFTER UPDATE AS PRINT 'z';"
        );
        assert_eq!(
            text("CREATE OR ALTER TRIGGER t8_x ON t8 FOR INSERT, UPDATE AS PRINT 'w'"),
            "CREATE   TRIGGER t8_x ON t8 FOR INSERT, UPDATE AS PRINT 'w'"
        );
        assert_eq!(
            text("CREATE TRIGGER t ON x AFTER UPDATE AS PRINT 1"),
            "CREATE TRIGGER t ON x AFTER UPDATE AS PRINT 1"
        );
    }

    #[test]
    fn misplaced_definitions_are_found_outside_strings() {
        assert_eq!(
            misplaced("SELECT 1\nCREATE TRIGGER t ON x AFTER INSERT AS PRINT 1"),
            Some("CREATE TRIGGER")
        );
        assert_eq!(
            misplaced("SELECT 1 create or alter trigger t on x after insert as print 1"),
            Some("CREATE TRIGGER")
        );
        assert_eq!(
            misplaced("PRINT 1 ALTER TRIGGER t ON x"),
            Some("ALTER TRIGGER")
        );
        assert_eq!(
            misplaced("EXEC('CREATE TRIGGER t ON x AFTER INSERT AS PRINT 1')"),
            None
        );
        assert_eq!(
            misplaced("ALTER TABLE t DISABLE TRIGGER ALL; DROP TRIGGER t"),
            None
        );
    }

    #[test]
    fn update_tests_are_found_outside_strings_and_comments() {
        let body = "IF UPDATE(qty) OR UPDATE ( [Name] ) PRINT 'UPDATE(x)' -- UPDATE(y)\nUPDATE t SET a = 1";
        let tests = update_tests(body);
        assert_eq!(
            tests
                .iter()
                .map(|(range, column)| (&body[range.clone()], column.as_str()))
                .collect::<Vec<_>>(),
            [("UPDATE(qty)", "qty"), ("UPDATE ( [Name] )", "Name")]
        );
        let body = "SELECT @@NESTLEVEL, '@@NESTLEVEL', @@nestlevel";
        assert_eq!(
            nest_level_references(body)
                .into_iter()
                .map(|range| &body[range])
                .collect::<Vec<_>>(),
            ["@@NESTLEVEL", "@@nestlevel"]
        );
    }

    fn command(sql: &str) -> Command {
        let statements = crate::batch::parse(sql).unwrap();
        assert_eq!(statements.len(), 1, "{sql}");
        Command::of(&statements[0]).expect("trigger command")
    }

    #[test]
    fn management_statements_round_trip_through_carriers() {
        assert_eq!(
            command("DROP TRIGGER IF EXISTS dbo.a, [b c]"),
            Command::Drop {
                if_exists: true,
                names: vec![vec!["dbo".into(), "a".into()], vec!["b c".into()]],
                ddl: false
            }
        );
        assert_eq!(
            command("DISABLE TRIGGER ALL ON items"),
            Command::Toggle {
                enable: false,
                names: None,
                target: Target::Object(vec!["items".into()]),
                alter_table: false
            }
        );
        assert_eq!(
            command("enable trigger t1, dbo.t2 on dbo.items;"),
            Command::Toggle {
                enable: true,
                names: Some(vec![vec!["t1".into()], vec!["dbo".into(), "t2".into()]]),
                target: Target::Object(vec!["dbo".into(), "items".into()]),
                alter_table: false
            }
        );
        assert_eq!(
            command("DISABLE TRIGGER ALL ON DATABASE"),
            Command::Toggle {
                enable: false,
                names: None,
                target: Target::Ddl,
                alter_table: false
            }
        );
        assert_eq!(
            command("ALTER TABLE dbo.items ENABLE TRIGGER ALL"),
            Command::Toggle {
                enable: true,
                names: None,
                target: Target::Object(vec!["dbo".into(), "items".into()]),
                alter_table: true
            }
        );
        assert_eq!(
            command("ALTER TABLE items DISABLE TRIGGER a, b"),
            Command::Toggle {
                enable: false,
                names: Some(vec![vec!["a".into()], vec!["b".into()]]),
                target: Target::Object(vec!["items".into()]),
                alter_table: true
            }
        );
        // Other ALTER TABLE forms still reach the built-in parser.
        assert!(Command::of(&crate::batch::parse("ALTER TABLE t ADD c INT").unwrap()[0]).is_none());
        // Commands can follow other statements and appear in control flow.
        let statements = crate::batch::parse(
            "SELECT 1; DISABLE TRIGGER ALL ON t IF 1 = 1 ENABLE TRIGGER ALL ON t",
        )
        .unwrap();
        assert_eq!(statements.len(), 3);
    }
}
