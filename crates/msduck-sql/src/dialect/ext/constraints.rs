//! Syntax for ALTER TABLE constraint lifecycle statements:
//!
//! - `ALTER TABLE t [WITH {CHECK | NOCHECK}] ADD <item> [, <item>...]`, where an
//!   item is a column definition or a table constraint: `[CONSTRAINT name]`
//!   followed by `CHECK (expr)`, `DEFAULT expr FOR column [WITH VALUES]`,
//!   `PRIMARY KEY [CLUSTERED | NONCLUSTERED] (columns)`, `UNIQUE (...)` or
//!   `FOREIGN KEY (columns) REFERENCES t [(columns)] [ON DELETE action]
//!   [ON UPDATE action]`;
//! - `ALTER TABLE t [WITH {CHECK | NOCHECK}] {CHECK | NOCHECK} CONSTRAINT
//!   {ALL | name [, name...]}`;
//! - `ALTER TABLE t DROP [CONSTRAINT] [IF EXISTS] name [, ...]`, optionally
//!   mixed with `COLUMN [IF EXISTS] name` items.
//!
//! Plain column additions and drops that need no constraint handling are left
//! to the built-in parser. A claimed statement travels as a carrier whose
//! payload is the canonical text of [`Alter`]; [`decode`] parses it again at
//! execution. See docs/gaps-constraints.md.
use super::{carrier, custom};
use sqlparser::{
    ast::*,
    parser::{Parser, ParserError},
    tokenizer::Token,
};
use std::fmt;

/// Carrier kind of claimed statements.
pub const KIND: &str = "constraints";

/// One ALTER TABLE constraint statement.
#[derive(Clone, Debug, PartialEq)]
pub struct Alter {
    pub table: ObjectName,
    /// `WITH CHECK` (`Some(true)`) or `WITH NOCHECK` (`Some(false)`).
    pub with_check: Option<bool>,
    pub action: Action,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Add(Vec<AddItem>),
    Drop(Vec<DropItem>),
    /// `CHECK CONSTRAINT` (`enable`) or `NOCHECK CONSTRAINT`.
    Toggle {
        enable: bool,
        targets: Targets,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Targets {
    All,
    Names(Vec<Ident>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum AddItem {
    Column(ColumnDef),
    Constraint(Box<Constraint>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Constraint {
    pub name: Option<Ident>,
    pub kind: Kind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Check(Expr),
    Default {
        value: Expr,
        column: Ident,
        with_values: bool,
    },
    PrimaryKey(Vec<Ident>),
    Unique(Vec<Ident>),
    ForeignKey(ForeignKey),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ForeignKey {
    pub columns: Vec<Ident>,
    pub table: ObjectName,
    /// Empty for an implicit reference to the primary key.
    pub referred: Vec<Ident>,
    pub on_delete: Referential,
    pub on_update: Referential,
}

/// SQL Server referential actions, numbered as in `sys.foreign_keys`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Referential {
    #[default]
    NoAction = 0,
    Cascade = 1,
    SetNull = 2,
    SetDefault = 3,
}

impl Referential {
    pub fn code(self) -> i32 {
        self as i32
    }
    pub fn from_code(code: i32) -> Self {
        match code {
            1 => Self::Cascade,
            2 => Self::SetNull,
            3 => Self::SetDefault,
            _ => Self::NoAction,
        }
    }
    /// From the sqlparser representation used by CREATE TABLE.
    pub fn from_ast(action: Option<ReferentialAction>) -> Result<Self, String> {
        Ok(match action {
            None | Some(ReferentialAction::NoAction) => Self::NoAction,
            Some(ReferentialAction::Cascade) => Self::Cascade,
            Some(ReferentialAction::SetNull) => Self::SetNull,
            Some(ReferentialAction::SetDefault) => Self::SetDefault,
            Some(ReferentialAction::Restrict) => {
                return Err("Incorrect syntax near 'RESTRICT'.".into());
            }
        })
    }
    /// `sys.foreign_keys` description.
    pub fn description(self) -> &'static str {
        match self {
            Self::NoAction => "NO_ACTION",
            Self::Cascade => "CASCADE",
            Self::SetNull => "SET_NULL",
            Self::SetDefault => "SET_DEFAULT",
        }
    }
}

impl fmt::Display for Referential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NoAction => "NO ACTION",
            Self::Cascade => "CASCADE",
            Self::SetNull => "SET NULL",
            Self::SetDefault => "SET DEFAULT",
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum DropItem {
    Constraint { name: Ident, if_exists: bool },
    Column { name: Ident, if_exists: bool },
}

fn columns(f: &mut fmt::Formatter<'_>, columns: &[Ident]) -> fmt::Result {
    f.write_str("(")?;
    for (index, column) in columns.iter().enumerate() {
        if index > 0 {
            f.write_str(", ")?;
        }
        write!(f, "{column}")?;
    }
    f.write_str(")")
}

impl fmt::Display for Constraint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(name) = &self.name {
            write!(f, "CONSTRAINT {name} ")?;
        }
        match &self.kind {
            Kind::Check(expr) => write!(f, "CHECK ({expr})"),
            Kind::Default {
                value,
                column,
                with_values,
            } => {
                write!(f, "DEFAULT {value} FOR {column}")?;
                if *with_values {
                    f.write_str(" WITH VALUES")?;
                }
                Ok(())
            }
            Kind::PrimaryKey(list) => {
                f.write_str("PRIMARY KEY ")?;
                columns(f, list)
            }
            Kind::Unique(list) => {
                f.write_str("UNIQUE ")?;
                columns(f, list)
            }
            Kind::ForeignKey(key) => {
                f.write_str("FOREIGN KEY ")?;
                columns(f, &key.columns)?;
                write!(f, " REFERENCES {}", key.table)?;
                if !key.referred.is_empty() {
                    f.write_str(" ")?;
                    columns(f, &key.referred)?;
                }
                write!(
                    f,
                    " ON DELETE {} ON UPDATE {}",
                    key.on_delete, key.on_update
                )
            }
        }
    }
}

impl fmt::Display for Alter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ALTER TABLE {}", self.table)?;
        match self.with_check {
            Some(true) => f.write_str(" WITH CHECK")?,
            Some(false) => f.write_str(" WITH NOCHECK")?,
            None => {}
        }
        match &self.action {
            Action::Add(items) => {
                f.write_str(" ADD ")?;
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        f.write_str(", ")?;
                    }
                    match item {
                        AddItem::Column(column) => write!(f, "{column}")?,
                        AddItem::Constraint(constraint) => write!(f, "{constraint}")?,
                    }
                }
                Ok(())
            }
            Action::Drop(items) => {
                f.write_str(" DROP ")?;
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        f.write_str(", ")?;
                    }
                    let (keyword, name, if_exists) = match item {
                        DropItem::Constraint { name, if_exists } => ("CONSTRAINT", name, if_exists),
                        DropItem::Column { name, if_exists } => ("COLUMN", name, if_exists),
                    };
                    write!(f, "{keyword} ")?;
                    if *if_exists {
                        f.write_str("IF EXISTS ")?;
                    }
                    write!(f, "{name}")?;
                }
                Ok(())
            }
            Action::Toggle { enable, targets } => {
                f.write_str(if *enable { " CHECK" } else { " NOCHECK" })?;
                f.write_str(" CONSTRAINT ")?;
                match targets {
                    Targets::All => f.write_str("ALL"),
                    Targets::Names(names) => {
                        for (index, name) in names.iter().enumerate() {
                            if index > 0 {
                                f.write_str(", ")?;
                            }
                            write!(f, "{name}")?;
                        }
                        Ok(())
                    }
                }
            }
        }
    }
}

/// Whether the next token is the unquoted word `word`.
fn peek_word(parser: &Parser, word: &str) -> bool {
    peek_nth_word(parser, 0, word)
}

fn peek_nth_word(parser: &Parser, n: usize, word: &str) -> bool {
    matches!(&parser.peek_nth_token_ref(n).token,
        Token::Word(w) if w.quote_style.is_none() && w.value.eq_ignore_ascii_case(word))
}

fn take_word(parser: &mut Parser, word: &str) -> bool {
    if peek_word(parser, word) {
        parser.next_token();
        true
    } else {
        false
    }
}

fn expect_word(parser: &mut Parser, word: &str) -> Result<(), ParserError> {
    if take_word(parser, word) {
        Ok(())
    } else {
        parser.expected(word, parser.peek_token())
    }
}

fn decline() -> ParserError {
    ParserError::ParserError("not an ALTER TABLE constraint statement".into())
}

/// A parenthesized key column list; ASC and DESC are accepted and ignored.
fn key_columns(parser: &mut Parser) -> Result<Vec<Ident>, ParserError> {
    parser.expect_token(&Token::LParen)?;
    let mut columns = Vec::new();
    loop {
        columns.push(parser.parse_identifier()?);
        let _ = take_word(parser, "ASC") || take_word(parser, "DESC");
        if !parser.consume_token(&Token::Comma) {
            break;
        }
    }
    parser.expect_token(&Token::RParen)?;
    Ok(columns)
}

fn referential(parser: &mut Parser) -> Result<Referential, ParserError> {
    if take_word(parser, "CASCADE") {
        Ok(Referential::Cascade)
    } else if take_word(parser, "NO") {
        expect_word(parser, "ACTION")?;
        Ok(Referential::NoAction)
    } else if take_word(parser, "SET") {
        if take_word(parser, "NULL") {
            Ok(Referential::SetNull)
        } else {
            expect_word(parser, "DEFAULT")?;
            Ok(Referential::SetDefault)
        }
    } else {
        parser.expected(
            "NO ACTION, CASCADE, SET NULL or SET DEFAULT",
            parser.peek_token(),
        )
    }
}

/// Skip `CLUSTERED` or `NONCLUSTERED`.
fn clustering(parser: &mut Parser) {
    let _ = take_word(parser, "CLUSTERED") || take_word(parser, "NONCLUSTERED");
}

/// Skip `WITH (index options)` and `ON filegroup`, which have no effect here.
fn index_tail(parser: &mut Parser) -> Result<(), ParserError> {
    if peek_word(parser, "WITH") && parser.peek_nth_token_ref(1).token == Token::LParen {
        parser.next_token();
        parser.next_token();
        let mut depth = 1;
        while depth > 0 {
            match parser.next_token().token {
                Token::LParen => depth += 1,
                Token::RParen => depth -= 1,
                Token::EOF => return parser.expected(")", parser.peek_token()),
                _ => {}
            }
        }
    }
    if peek_word(parser, "ON")
        && !matches!(&parser.peek_nth_token_ref(1).token,
            Token::Word(w) if w.quote_style.is_none()
                && (w.value.eq_ignore_ascii_case("DELETE") || w.value.eq_ignore_ascii_case("UPDATE")))
    {
        parser.next_token();
        parser.parse_identifier()?;
    }
    Ok(())
}

fn not_for_replication(parser: &mut Parser) -> Result<(), ParserError> {
    if peek_word(parser, "NOT") && peek_nth_word(parser, 1, "FOR") {
        parser.next_token();
        parser.next_token();
        expect_word(parser, "REPLICATION")?;
    }
    Ok(())
}

fn starts_constraint(parser: &Parser) -> bool {
    [
        "CONSTRAINT",
        "CHECK",
        "PRIMARY",
        "UNIQUE",
        "FOREIGN",
        "DEFAULT",
    ]
    .iter()
    .any(|word| peek_word(parser, word))
}

fn parse_constraint(parser: &mut Parser) -> Result<Constraint, ParserError> {
    let name = if take_word(parser, "CONSTRAINT") {
        Some(parser.parse_identifier()?)
    } else {
        None
    };
    let kind = if take_word(parser, "CHECK") {
        not_for_replication(parser)?;
        parser.expect_token(&Token::LParen)?;
        let expr = parser.parse_expr()?;
        parser.expect_token(&Token::RParen)?;
        Kind::Check(expr)
    } else if take_word(parser, "DEFAULT") {
        let value = parser.parse_expr()?;
        expect_word(parser, "FOR")?;
        let column = parser.parse_identifier()?;
        let with_values = if peek_word(parser, "WITH") && peek_nth_word(parser, 1, "VALUES") {
            parser.next_token();
            parser.next_token();
            true
        } else {
            false
        };
        Kind::Default {
            value,
            column,
            with_values,
        }
    } else if take_word(parser, "PRIMARY") {
        expect_word(parser, "KEY")?;
        clustering(parser);
        let columns = key_columns(parser)?;
        index_tail(parser)?;
        Kind::PrimaryKey(columns)
    } else if take_word(parser, "UNIQUE") {
        clustering(parser);
        let columns = key_columns(parser)?;
        index_tail(parser)?;
        Kind::Unique(columns)
    } else if take_word(parser, "FOREIGN") {
        expect_word(parser, "KEY")?;
        let columns = key_columns(parser)?;
        expect_word(parser, "REFERENCES")?;
        let table = parser.parse_object_name(false)?;
        let referred = if parser.peek_token_ref().token == Token::LParen {
            key_columns(parser)?
        } else {
            Vec::new()
        };
        let mut on_delete = None;
        let mut on_update = None;
        while peek_word(parser, "ON") {
            parser.next_token();
            if take_word(parser, "DELETE") && on_delete.is_none() {
                on_delete = Some(referential(parser)?);
            } else if take_word(parser, "UPDATE") && on_update.is_none() {
                on_update = Some(referential(parser)?);
            } else {
                return parser.expected("DELETE or UPDATE", parser.peek_token());
            }
        }
        not_for_replication(parser)?;
        Kind::ForeignKey(ForeignKey {
            columns,
            table,
            referred,
            on_delete: on_delete.unwrap_or_default(),
            on_update: on_update.unwrap_or_default(),
        })
    } else {
        return parser.expected(
            "CHECK, DEFAULT, PRIMARY KEY, UNIQUE or FOREIGN KEY",
            parser.peek_token(),
        );
    };
    Ok(Constraint { name, kind })
}

/// Whether a column definition needs constraint handling beyond the built-in
/// ADD COLUMN path.
///
/// A named DEFAULT on a nullable column without WITH VALUES is left to the
/// built-in path, which still refuses it explicitly: tests/tedious.test.mjs
/// (owned by another task) expects that refusal. docs/gaps-constraints.md
/// records the gap.
pub fn column_has_constraints(column: &ColumnDef) -> bool {
    let required_or_filled = column.options.iter().any(|option| {
        matches!(option.option, ColumnOption::NotNull)
            || crate::dialect::is_with_values(&option.option)
    });
    column.options.iter().any(|option| match &option.option {
        ColumnOption::Check(_)
        | ColumnOption::ForeignKey(_)
        | ColumnOption::PrimaryKey(_)
        | ColumnOption::Unique(_) => true,
        ColumnOption::Default(_) => option.name.is_some() && required_or_filled,
        _ => option.name.is_some(),
    })
}

fn parse_body(parser: &mut Parser, table: ObjectName) -> Result<Alter, ParserError> {
    let with_check = if peek_word(parser, "WITH")
        && (peek_nth_word(parser, 1, "CHECK") || peek_nth_word(parser, 1, "NOCHECK"))
    {
        parser.next_token();
        Some(take_word(parser, "CHECK") || !take_word(parser, "NOCHECK"))
    } else {
        None
    };
    let action = if peek_word(parser, "CHECK") || peek_word(parser, "NOCHECK") {
        let enable = take_word(parser, "CHECK");
        if !enable {
            expect_word(parser, "NOCHECK")?;
        }
        expect_word(parser, "CONSTRAINT")?;
        let targets = if take_word(parser, "ALL") {
            Targets::All
        } else {
            let mut names = vec![parser.parse_identifier()?];
            while parser.consume_token(&Token::Comma) {
                names.push(parser.parse_identifier()?);
            }
            Targets::Names(names)
        };
        Action::Toggle { enable, targets }
    } else if take_word(parser, "ADD") {
        let mut items = Vec::new();
        loop {
            if items.len() >= 10000 {
                return Err(ParserError::ParserError(
                    "too many ALTER TABLE items".into(),
                ));
            }
            if starts_constraint(parser) {
                items.push(AddItem::Constraint(Box::new(parse_constraint(parser)?)));
            } else {
                items.push(AddItem::Column(parser.parse_column_def()?));
            }
            if !parser.consume_token(&Token::Comma) {
                break;
            }
        }
        let claimed = with_check.is_some()
            || items.iter().any(|item| match item {
                AddItem::Constraint(_) => true,
                AddItem::Column(column) => column_has_constraints(column),
            });
        if !claimed {
            return Err(decline());
        }
        Action::Add(items)
    } else if take_word(parser, "DROP") {
        let mut items = Vec::new();
        let mut column = false;
        loop {
            if items.len() >= 10000 {
                return Err(ParserError::ParserError(
                    "too many ALTER TABLE items".into(),
                ));
            }
            if take_word(parser, "COLUMN") {
                column = true;
            } else if take_word(parser, "CONSTRAINT") {
                column = false;
            }
            let if_exists = if peek_word(parser, "IF") && peek_nth_word(parser, 1, "EXISTS") {
                parser.next_token();
                parser.next_token();
                true
            } else {
                false
            };
            let name = parser.parse_identifier()?;
            items.push(if column {
                DropItem::Column { name, if_exists }
            } else {
                DropItem::Constraint { name, if_exists }
            });
            if !parser.consume_token(&Token::Comma) {
                break;
            }
        }
        if with_check.is_none()
            && items
                .iter()
                .all(|item| matches!(item, DropItem::Column { .. }))
        {
            return Err(decline());
        }
        Action::Drop(items)
    } else {
        return Err(decline());
    };
    Ok(Alter {
        table,
        with_check,
        action,
    })
}

/// `ALTER TABLE name ...` in the forms this module owns.
pub fn parse_alter(parser: &mut Parser) -> Result<Alter, ParserError> {
    if !(peek_word(parser, "ALTER") && peek_nth_word(parser, 1, "TABLE")) {
        return Err(decline());
    }
    parser.next_token();
    parser.next_token();
    let table = parser.parse_object_name(false)?;
    parse_body(parser, table)
}

/// Parse a statement this feature owns, or decline without consuming tokens.
pub fn parse(parser: &mut Parser) -> Option<Result<Statement, ParserError>> {
    if !(peek_word(parser, "ALTER") && peek_nth_word(parser, 1, "TABLE")) {
        return None;
    }
    let alter = parser.try_parse(parse_alter).ok()?;
    Some(Ok(carrier(KIND, &alter.to_string(), vec![])))
}

/// Whether this feature validates `statement` itself. Its statements are
/// carriers, which are always owned.
pub fn owns(_statement: &Statement) -> bool {
    false
}

/// The claimed statement carried by `statement`, parsed again from its
/// canonical text.
pub fn decode(statement: &Statement) -> Option<Result<Alter, ParserError>> {
    let custom = custom(statement)?;
    if custom.kind != KIND {
        return None;
    }
    Some((|| {
        let mut parser =
            Parser::new(&crate::dialect::ServerDialect).try_with_sql(custom.payload)?;
        let alter = parse_alter(&mut parser)?;
        if parser.peek_token_ref().token != Token::EOF {
            return parser.expected("end of statement", parser.peek_token());
        }
        Ok(alter)
    })())
}

#[cfg(test)]
mod tests;
