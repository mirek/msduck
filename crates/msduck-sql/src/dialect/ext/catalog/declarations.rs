//! What a batch declares, as written, for the catalog views.
//!
//! Other features rewrite CREATE TABLE and ALTER TABLE before the catalog
//! sees them: session functions in DEFAULTs become reads of connection
//! variables, computed columns over Unicode carriers become backend
//! expressions, identity and rowversion columns gain backend DEFAULTs, and
//! the CLUSTERED and NONCLUSTERED keywords are dropped while tokenizing.
//! SQL Server's catalog keeps the original declarations, so the root
//! adapter reads them from the batch text with these functions.
use super::definition;
use sqlparser::{
    ast::*,
    tokenizer::{Token, Tokenizer},
};
use std::ops::ControlFlow;

/// A DEFAULT as declared: its constraint name, if given, and its source text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Default {
    pub name: Option<String>,
    /// T-SQL text that parses back to the declared expression.
    pub source: String,
}

/// A column declaration of CREATE TABLE or ALTER TABLE ... ADD.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Column {
    /// The table name's parts, as written.
    pub table: Vec<String>,
    /// CREATE TABLE (true) or ALTER TABLE ... ADD (false).
    pub create: bool,
    pub column: String,
    pub default: Option<Default>,
    /// The computed column expression's source text.
    pub computed: Option<String>,
    /// The column is declared NOT NULL.
    pub not_null: bool,
}

/// `ALTER TABLE ... ADD [CONSTRAINT name] DEFAULT value FOR column`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefaultFor {
    pub table: Vec<String>,
    pub column: String,
    pub default: Default,
}

/// A named CHECK constraint of CREATE TABLE or ALTER TABLE ... ADD.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    pub table: Vec<String>,
    pub name: String,
    pub source: String,
}

/// A batch's declarations.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Declarations {
    pub columns: Vec<Column>,
    pub defaults: Vec<DefaultFor>,
    pub checks: Vec<Check>,
}

impl Declarations {
    pub fn is_empty(&self) -> bool {
        self.columns.is_empty() && self.defaults.is_empty() && self.checks.is_empty()
    }
}

/// The statements of a batch as written: without the normalizations the
/// batch parser applies for execution (such as the default length 30 of
/// `CAST(x AS VARCHAR)`), which SQL Server's catalog text does not show.
pub fn parse(sql: &str) -> Option<Vec<Statement>> {
    let tokens = crate::dialect::tokenize(sql).ok()?;
    let mut parser = sqlparser::parser::Parser::new(&crate::dialect::ServerDialect)
        .with_tokens_with_locations(tokens);
    let mut statements = Vec::new();
    loop {
        while parser.consume_token(&Token::SemiColon) {}
        if parser.peek_token().token == Token::EOF {
            return Some(statements);
        }
        if statements.len() >= 10000 {
            return None;
        }
        statements.push(parser.parse_statement().ok()?);
    }
}

/// Source text of `expr` that parses back to it.
fn source(expr: &Expr) -> String {
    let mut expr = expr.clone();
    super::super::constraints::delimit_expr(&mut expr);
    expr.to_string()
}

fn parts(name: &ObjectName) -> Option<Vec<String>> {
    name.0
        .iter()
        .map(|part| part.as_ident().map(|ident| ident.value.clone()))
        .collect()
}

fn column(table: &[String], create: bool, column: &ColumnDef) -> Option<Column> {
    let default = column
        .options
        .iter()
        .find_map(|option| match &option.option {
            ColumnOption::Default(expr) => Some(Default {
                name: option.name.as_ref().map(|name| name.value.clone()),
                source: source(expr),
            }),
            _ => None,
        });
    let computed = crate::dialect::computed_column::computed(column).map(|(expr, _)| source(expr));
    (default.is_some() || computed.is_some()).then(|| Column {
        table: table.to_vec(),
        create,
        column: column.name.value.clone(),
        default,
        computed,
        not_null: column
            .options
            .iter()
            .any(|option| matches!(option.option, ColumnOption::NotNull)),
    })
}

/// Declarations of the CREATE TABLE and ALTER TABLE ... ADD statements of
/// `sql`, including those nested in blocks. A batch that does not parse
/// declares nothing.
pub fn declarations(sql: &str) -> Declarations {
    let Some(statements) = parse(sql) else {
        return Declarations::default();
    };
    struct Collect(Declarations);
    impl Visitor for Collect {
        type Break = ();
        fn pre_visit_statement(&mut self, statement: &Statement) -> ControlFlow<()> {
            match statement {
                Statement::CreateTable(table) if table.query.is_none() => {
                    if let Some(name) = parts(&table.name) {
                        self.0.columns.extend(
                            table
                                .columns
                                .iter()
                                .filter_map(|definition| column(&name, true, definition)),
                        );
                        let column_checks = table.columns.iter().flat_map(|column| {
                            column
                                .options
                                .iter()
                                .filter_map(|option| match &option.option {
                                    ColumnOption::Check(check) => Some((
                                        option.name.as_ref().or(check.name.as_ref())?,
                                        &*check.expr,
                                    )),
                                    _ => None,
                                })
                        });
                        let table_checks =
                            table
                                .constraints
                                .iter()
                                .filter_map(|constraint| match constraint {
                                    TableConstraint::Check(check) => {
                                        Some((check.name.as_ref()?, &*check.expr))
                                    }
                                    _ => None,
                                });
                        for (check, expr) in column_checks.chain(table_checks) {
                            self.0.checks.push(Check {
                                table: name.clone(),
                                name: check.value.clone(),
                                source: source(expr),
                            });
                        }
                    }
                }
                Statement::AlterTable(table) => {
                    if let Some(name) = parts(&table.name) {
                        for operation in &table.operations {
                            if let AlterTableOperation::AddColumn { column_def, .. } = operation
                                && let Some(column) = column(&name, false, column_def)
                            {
                                self.0.columns.push(column);
                            }
                        }
                    }
                }
                _ => {
                    if let Some(Ok(alter)) = super::super::constraints::decode(statement)
                        && let Some(name) = parts(&alter.table)
                        && let super::super::constraints::Action::Add(items) = &alter.action
                    {
                        for item in items {
                            match item {
                                super::super::constraints::AddItem::Column(definition) => {
                                    if let Some(column) = column(&name, false, definition) {
                                        self.0.columns.push(column);
                                    }
                                    for option in &definition.options {
                                        if let ColumnOption::Check(check) = &option.option
                                            && let Some(check_name) =
                                                option.name.as_ref().or(check.name.as_ref())
                                        {
                                            self.0.checks.push(Check {
                                                table: name.clone(),
                                                name: check_name.value.clone(),
                                                source: source(&check.expr),
                                            });
                                        }
                                    }
                                }
                                super::super::constraints::AddItem::Constraint(constraint) => {
                                    if let (
                                        Some(check),
                                        super::super::constraints::Kind::Check(expr),
                                    ) = (&constraint.name, &constraint.kind)
                                    {
                                        self.0.checks.push(Check {
                                            table: name.clone(),
                                            name: check.value.clone(),
                                            source: source(expr),
                                        });
                                    }
                                    if let super::super::constraints::Kind::Default {
                                        value,
                                        column,
                                        ..
                                    } = &constraint.kind
                                    {
                                        self.0.defaults.push(DefaultFor {
                                            table: name.clone(),
                                            column: column.value.clone(),
                                            default: Default {
                                                name: constraint
                                                    .name
                                                    .as_ref()
                                                    .map(|name| name.value.clone()),
                                                source: source(value),
                                            },
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
            ControlFlow::Continue(())
        }
    }
    let mut collect = Collect(Declarations::default());
    for statement in &statements {
        let _ = statement.visit(&mut collect);
    }
    collect.0
}

/// A PRIMARY KEY or UNIQUE constraint, or an inline index, with the
/// clustering keyword written for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Key {
    pub table: Vec<String>,
    /// CREATE TABLE (true) or ALTER TABLE ... ADD (false).
    pub create: bool,
    pub kind: KeyKind,
    pub name: Option<String>,
    pub columns: Vec<String>,
    /// Whether each column is a descending key column.
    pub descending: Vec<bool>,
    /// CLUSTERED (`Some(true)`), NONCLUSTERED (`Some(false)`) or neither.
    pub clustered: Option<bool>,
    /// The ordinal of the declaring statement within the batch.
    pub statement: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyKind {
    Primary,
    Unique,
    /// An inline `INDEX name [CLUSTERED|NONCLUSTERED] (columns)`.
    Index,
}

struct Tokens {
    tokens: Vec<Token>,
}

impl Tokens {
    fn word(&self, at: usize) -> Option<&sqlparser::tokenizer::Word> {
        match self.tokens.get(at) {
            Some(Token::Word(word)) => Some(word),
            _ => None,
        }
    }
    /// An unquoted keyword.
    fn is(&self, at: usize, keyword: &str) -> bool {
        self.word(at)
            .is_some_and(|w| w.quote_style.is_none() && w.value.eq_ignore_ascii_case(keyword))
    }
    fn token(&self, at: usize) -> Option<&Token> {
        self.tokens.get(at)
    }
    /// A multi-part name starting at `at`; returns the parts and the next
    /// position.
    fn name(&self, mut at: usize) -> Option<(Vec<String>, usize)> {
        let mut parts = vec![self.word(at)?.value.clone()];
        at += 1;
        while self.token(at) == Some(&Token::Period) {
            at += 1;
            // `db..table` leaves the schema empty.
            if self.token(at) == Some(&Token::Period) {
                parts.push(String::new());
                continue;
            }
            parts.push(self.word(at)?.value.clone());
            at += 1;
        }
        Some((parts, at))
    }
    /// A parenthesized column list at `at` (`(a [ASC|DESC], ...)`); returns
    /// the names, their DESC flags and the position after `)`.
    fn columns(&self, mut at: usize) -> Option<(Vec<String>, Vec<bool>, usize)> {
        if self.token(at) != Some(&Token::LParen) {
            return None;
        }
        at += 1;
        let mut columns = Vec::new();
        let mut descending = Vec::new();
        loop {
            columns.push(self.word(at)?.value.clone());
            at += 1;
            descending.push(self.is(at, "DESC"));
            if self.is(at, "ASC") || self.is(at, "DESC") {
                at += 1;
            }
            match self.token(at)? {
                Token::Comma => at += 1,
                Token::RParen => return Some((columns, descending, at + 1)),
                _ => return None,
            }
        }
    }
    /// The end of the parenthesized group opening at `at`.
    fn close(&self, at: usize) -> usize {
        let mut depth = 0usize;
        let mut position = at;
        while let Some(token) = self.token(position) {
            match token {
                Token::LParen => depth += 1,
                Token::RParen => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return position;
                    }
                }
                _ => {}
            }
            position += 1;
        }
        position
    }
    /// `PRIMARY KEY|UNIQUE [CLUSTERED|NONCLUSTERED]` at `at`: the kind,
    /// the clustering and the next position.
    fn key(&self, at: usize) -> Option<(KeyKind, Option<bool>, usize)> {
        let (kind, mut next) = if self.is(at, "PRIMARY") && self.is(at + 1, "KEY") {
            (KeyKind::Primary, at + 2)
        } else if self.is(at, "UNIQUE") {
            (KeyKind::Unique, at + 1)
        } else {
            return None;
        };
        let clustered = if self.is(next, "CLUSTERED") {
            next += 1;
            Some(true)
        } else if self.is(next, "NONCLUSTERED") {
            next += 1;
            Some(false)
        } else {
            None
        };
        Some((kind, clustered, next))
    }
}

/// Statements end at `;` or at a word that starts another statement.
const STATEMENT_WORDS: &[&str] = &[
    "CREATE",
    "ALTER",
    "DROP",
    "INSERT",
    "UPDATE",
    "DELETE",
    "SELECT",
    "EXEC",
    "EXECUTE",
    "IF",
    "BEGIN",
    "END",
    "DECLARE",
    "SET",
    "GO",
    "PRINT",
    "RETURN",
    "WHILE",
    "MERGE",
    "TRUNCATE",
    "USE",
    "COMMIT",
    "ROLLBACK",
    "SAVE",
    "RAISERROR",
    "THROW",
    "WITH",
];

/// The keys of one definition element (a column or a table constraint).
fn element(
    tokens: &Tokens,
    start: usize,
    end: usize,
    table: &[String],
    create: bool,
    statement: usize,
) -> Vec<Key> {
    let mut keys = Vec::new();
    let leading = |at: usize| {
        [
            "CONSTRAINT",
            "PRIMARY",
            "UNIQUE",
            "CHECK",
            "FOREIGN",
            "INDEX",
        ]
        .iter()
        .any(|word| tokens.is(at, word))
    };
    if tokens.is(start, "INDEX") {
        let Some(name) = tokens.word(start + 1) else {
            return keys;
        };
        let mut at = start + 2;
        let clustered = if tokens.is(at, "CLUSTERED") {
            at += 1;
            Some(true)
        } else if tokens.is(at, "NONCLUSTERED") {
            at += 1;
            Some(false)
        } else {
            None
        };
        if let Some((columns, descending, _)) = tokens.columns(at) {
            keys.push(Key {
                table: table.to_vec(),
                create,
                kind: KeyKind::Index,
                name: Some(name.value.clone()),
                columns,
                descending,
                clustered,
                statement,
            });
        }
        return keys;
    }
    // A column definition names its column first.
    let column = (!leading(start))
        .then(|| tokens.word(start).map(|word| word.value.clone()))
        .flatten();
    let mut at = if column.is_some() { start + 1 } else { start };
    let mut name = None;
    while at < end {
        if tokens.token(at) == Some(&Token::LParen) {
            at = tokens.close(at) + 1;
            continue;
        }
        if tokens.is(at, "CONSTRAINT") {
            name = tokens.word(at + 1).map(|word| word.value.clone());
            at += 2;
            continue;
        }
        if let Some((kind, clustered, next)) = tokens.key(at) {
            let (columns, descending, next) = match tokens.columns(next) {
                Some((columns, descending, next)) if next <= end + 1 => (columns, descending, next),
                _ => match &column {
                    Some(column) => (vec![column.clone()], vec![false], next),
                    None => return keys,
                },
            };
            keys.push(Key {
                table: table.to_vec(),
                create,
                kind,
                name: name.take(),
                columns,
                descending,
                clustered,
                statement,
            });
            at = next;
            continue;
        }
        if !matches!(tokens.token(at), Some(Token::Word(_))) {
            name = None;
        }
        at += 1;
    }
    keys
}

/// The PRIMARY KEY and UNIQUE constraints (and inline indexes) that the
/// CREATE TABLE and ALTER TABLE ... ADD statements of `sql` declare, in
/// order, with their clustering keywords. Text that does not tokenize
/// declares nothing.
pub fn keys(sql: &str) -> Vec<Key> {
    let Ok(tokens) = Tokenizer::new(&crate::dialect::ServerDialect, sql).tokenize() else {
        return Vec::new();
    };
    let tokens = Tokens {
        tokens: tokens
            .into_iter()
            .filter(|token| !matches!(token, Token::Whitespace(_)))
            .collect(),
    };
    let mut keys = Vec::new();
    let mut at = 0;
    let mut statement = 0;
    while at < tokens.tokens.len() {
        let create = tokens.is(at, "CREATE") && tokens.is(at + 1, "TABLE");
        let alter = tokens.is(at, "ALTER") && tokens.is(at + 1, "TABLE");
        if !(create || alter) {
            at += 1;
            continue;
        }
        let Some((table, next)) = tokens.name(at + 2) else {
            at += 2;
            continue;
        };
        at = next;
        statement += 1;
        if create {
            if tokens.token(at) != Some(&Token::LParen) {
                continue;
            }
            let close = tokens.close(at);
            let mut start = at + 1;
            let mut position = start;
            let mut depth = 0usize;
            while position < close {
                match tokens.token(position) {
                    Some(Token::LParen) => depth += 1,
                    Some(Token::RParen) => depth = depth.saturating_sub(1),
                    Some(Token::Comma) if depth == 0 => {
                        keys.extend(element(&tokens, start, position, &table, true, statement));
                        start = position + 1;
                    }
                    _ => {}
                }
                position += 1;
            }
            keys.extend(element(&tokens, start, close, &table, true, statement));
            at = close + 1;
            continue;
        }
        // ALTER TABLE name [WITH CHECK|NOCHECK] ADD item, ...
        if tokens.is(at, "WITH") {
            at += 2;
        }
        if !tokens.is(at, "ADD") {
            continue;
        }
        at += 1;
        let mut end = at;
        let mut depth = 0usize;
        while let Some(token) = tokens.token(end) {
            match token {
                Token::LParen => depth += 1,
                Token::RParen => depth = depth.saturating_sub(1),
                Token::SemiColon if depth == 0 => break,
                Token::Word(word)
                    if depth == 0
                        && word.quote_style.is_none()
                        && STATEMENT_WORDS
                            .iter()
                            .any(|w| w.eq_ignore_ascii_case(&word.value)) =>
                {
                    break;
                }
                _ => {}
            }
            end += 1;
        }
        let mut start = at;
        let mut position = at;
        depth = 0;
        while position < end {
            match tokens.token(position) {
                Some(Token::LParen) => depth += 1,
                Some(Token::RParen) => depth = depth.saturating_sub(1),
                Some(Token::Comma) if depth == 0 => {
                    keys.extend(element(&tokens, start, position, &table, false, statement));
                    start = position + 1;
                }
                _ => {}
            }
            position += 1;
        }
        keys.extend(element(&tokens, start, end, &table, false, statement));
        at = end;
    }
    keys
}

/// Whether a stored expression has no SQL Server catalog text because it
/// is a backend expression another feature generated (identity and
/// rowversion allocators, session variable reads, lowered carriers).
pub fn generated(expr: &Expr) -> bool {
    struct Find(bool);
    impl Visitor for Find {
        type Break = ();
        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
            let internal = match expr {
                Expr::Function(function) => {
                    let name = function.name.to_string().to_ascii_lowercase();
                    name == "nextval"
                        || name == "getvariable"
                        || (name.starts_with("__msduck")
                            && crate::variant_cast::source(expr).is_none())
                }
                Expr::Value(value) => matches!(value.value, Value::Placeholder(_)),
                _ => false,
            };
            if internal {
                self.0 = true;
                return ControlFlow::Break(());
            }
            ControlFlow::Continue(())
        }
    }
    let mut find = Find(false);
    let _ = expr.visit(&mut find);
    find.0
}

/// The source text of a stored expression, when it is the declared T-SQL.
pub fn declared_source(expr: &Expr) -> Option<String> {
    (!generated(expr)).then(|| source(expr))
}

/// Whether a DEFAULT is a backend allocator another feature added for an
/// identity or rowversion column (it calls `nextval`, which T-SQL lacks).
pub fn allocator(expr: &Expr) -> bool {
    struct Find(bool);
    impl Visitor for Find {
        type Break = ();
        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
            if let Expr::Function(function) = expr
                && function.name.to_string().eq_ignore_ascii_case("nextval")
            {
                self.0 = true;
                return ControlFlow::Break(());
            }
            ControlFlow::Continue(())
        }
    }
    let mut find = Find(false);
    let _ = expr.visit(&mut find);
    find.0
}

/// SQL Server's generated name of an unnamed DEFAULT constraint:
/// `DF__table__column__XXXXXXXX`, with the object ID in hexadecimal. The
/// table and column parts share 14 characters; when both are longer, the
/// table keeps at least nine and the column at least five.
pub fn default_name(table: &str, column: &str, id: i32) -> String {
    let table: Vec<char> = table.chars().collect();
    let column: Vec<char> = column.chars().collect();
    let (mut kept_table, mut kept_column) = (table.len(), column.len());
    if kept_table + kept_column > 14 {
        kept_table = kept_table.min(9.max(14usize.saturating_sub(kept_column)));
        kept_column = kept_column.min(5.max(14 - kept_table.min(14)));
    }
    let table: String = table[..kept_table].iter().collect();
    let column: String = column[..kept_column].iter().collect();
    format!("DF__{table}__{column}__{:08X}", id as u32)
}

/// Whether a computed column over `expr` is nullable, as SQL Server infers
/// it: only columns declared NOT NULL, constants other than NULL, ISNULL
/// with a non-null operand and CASE whose branches (including an ELSE) are
/// all non-null are known to be non-null. Arithmetic, conversions and
/// other functions may yield NULL (for example on overflow).
pub fn computed_nullable(expr: &Expr, nullable: &dyn Fn(&str) -> bool) -> bool {
    fn unnest(mut expr: &Expr) -> &Expr {
        loop {
            match expr {
                Expr::Nested(inner) => expr = inner,
                _ => match crate::variant_cast::source(expr) {
                    Some(source) => expr = source,
                    None => return expr,
                },
            }
        }
    }
    match unnest(expr) {
        Expr::Identifier(ident) => nullable(&ident.value),
        Expr::CompoundIdentifier(parts) => parts.last().is_none_or(|ident| nullable(&ident.value)),
        Expr::Value(value) => matches!(value.value, Value::Null),
        Expr::UnaryOp {
            op: UnaryOperator::Plus | UnaryOperator::Minus,
            expr: inner,
        } if matches!(unnest(inner), Expr::Value(value) if matches!(value.value, Value::Number(..))) => {
            false
        }
        Expr::Case {
            conditions,
            else_result: Some(else_result),
            ..
        } => {
            conditions
                .iter()
                .any(|when| computed_nullable(&when.result, nullable))
                || computed_nullable(else_result, nullable)
        }
        Expr::Function(function)
            if function.name.to_string().eq_ignore_ascii_case("ISNULL")
                && function.over.is_none() =>
        {
            let FunctionArguments::List(list) = &function.args else {
                return true;
            };
            let arguments: Vec<&Expr> = list
                .args
                .iter()
                .filter_map(|argument| match argument {
                    FunctionArg::Unnamed(FunctionArgExpr::Expr(expr)) => Some(expr),
                    _ => None,
                })
                .collect();
            arguments.len() != 2
                || arguments
                    .iter()
                    .all(|argument| computed_nullable(argument, nullable))
        }
        _ => true,
    }
}

/// Whether `source` has known catalog text.
pub fn has_definition(source: &str) -> bool {
    definition::source_definition(source).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declarations_keep_the_written_expressions() {
        let found = declarations(
            "CREATE TABLE dbo.t(a INT CONSTRAINT DF_a DEFAULT (1), b NVARCHAR(10) DEFAULT SUSER_SNAME(), c AS a * 2 PERSISTED, d INT);
             ALTER TABLE dbo.t ADD e INT DEFAULT 0, CONSTRAINT DF_d DEFAULT 5 FOR d;
             IF 1 = 1 BEGIN CREATE TABLE u(x INT DEFAULT GETDATE()) END",
        );
        assert_eq!(
            found
                .columns
                .iter()
                .map(|c| (
                    c.table.join("."),
                    c.create,
                    c.column.as_str(),
                    c.default
                        .as_ref()
                        .map(|d| (d.name.clone(), d.source.clone())),
                    c.computed.clone()
                ))
                .collect::<Vec<_>>(),
            [
                (
                    "dbo.t".into(),
                    true,
                    "a",
                    Some((Some("DF_a".into()), "(1)".into())),
                    None
                ),
                (
                    "dbo.t".into(),
                    true,
                    "b",
                    Some((None, "SUSER_SNAME()".into())),
                    None
                ),
                ("dbo.t".into(), true, "c", None, Some("a * 2".into())),
                ("dbo.t".into(), false, "e", Some((None, "0".into())), None),
                (
                    "u".into(),
                    true,
                    "x",
                    Some((None, "GETDATE()".into())),
                    None
                ),
            ]
        );
        assert_eq!(
            found.defaults,
            [DefaultFor {
                table: vec!["dbo".into(), "t".into()],
                column: "d".into(),
                default: Default {
                    name: Some("DF_d".into()),
                    source: "5".into()
                }
            }]
        );
        assert!(declarations("SELECT 1").is_empty());
        let checks = declarations(
            "CREATE TABLE t(a INT CONSTRAINT ck_a CHECK (CAST(a AS VARCHAR) <> ''), b INT CHECK (b > 0), CONSTRAINT ck_t CHECK (a < b));
             ALTER TABLE t ADD CONSTRAINT ck_add CHECK (a <> 3)",
        )
        .checks;
        assert_eq!(
            checks
                .iter()
                .map(|c| (c.name.as_str(), c.source.as_str()))
                .collect::<Vec<_>>(),
            [
                ("ck_a", "CAST(a AS VARCHAR) <> ''"),
                ("ck_t", "a < b"),
                ("ck_add", "a <> 3")
            ]
        );
        assert!(declarations("CREATE TABLE (").is_empty());
    }

    #[test]
    fn keys_keep_their_clustering_keywords() {
        let found = keys(
            "CREATE TABLE [dbo].[t](id INT NOT NULL CONSTRAINT pk_t PRIMARY KEY NONCLUSTERED, code INT UNIQUE CLUSTERED, x INT, y INT,
               CONSTRAINT uq_xy UNIQUE (x DESC, y), INDEX ix_y CLUSTERED (y), b INT PRIMARY KEY (x, y));
             ALTER TABLE t ADD CONSTRAINT pk2 PRIMARY KEY CLUSTERED (id), z INT UNIQUE;
             ALTER TABLE t WITH NOCHECK ADD UNIQUE NONCLUSTERED (code) SELECT 1",
        );
        let summary: Vec<_> = found
            .iter()
            .map(|k| {
                (
                    k.create,
                    k.kind,
                    k.name.clone(),
                    k.columns.join(","),
                    k.clustered,
                    k.statement,
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                (
                    true,
                    KeyKind::Primary,
                    Some("pk_t".into()),
                    "id".into(),
                    Some(false),
                    1
                ),
                (true, KeyKind::Unique, None, "code".into(), Some(true), 1),
                (
                    true,
                    KeyKind::Unique,
                    Some("uq_xy".into()),
                    "x,y".into(),
                    None,
                    1
                ),
                (
                    true,
                    KeyKind::Index,
                    Some("ix_y".into()),
                    "y".into(),
                    Some(true),
                    1
                ),
                (true, KeyKind::Primary, None, "x,y".into(), None, 1),
                (
                    false,
                    KeyKind::Primary,
                    Some("pk2".into()),
                    "id".into(),
                    Some(true),
                    2
                ),
                (false, KeyKind::Unique, None, "z".into(), None, 2),
                (false, KeyKind::Unique, None, "code".into(), Some(false), 3),
            ]
        );
        assert_eq!(found[0].table, ["dbo", "t"]);
        assert_eq!(found[2].descending, [true, false]);
    }

    #[test]
    fn default_names_share_fourteen_characters() {
        for (table, column, expected) in [
            (
                "abcdefghijklmnopqrstuvwxyz",
                "colabcdefghij",
                "DF__abcdefghi__colab__0000002A",
            ),
            (
                "abcdefghijklmnopqrstuvwxyz",
                "c",
                "DF__abcdefghijklm__c__0000002A",
            ),
            (
                "abcdefghijklmnopqrstuvwxyz",
                "cccccc",
                "DF__abcdefghi__ccccc__0000002A",
            ),
            ("t", "colabcdefghij", "DF__t__colabcdefghij__0000002A"),
            (
                "tabletwelve",
                "colabcdefghij",
                "DF__tabletwel__colab__0000002A",
            ),
            ("v2_defaults", "a1", "DF__v2_defaults__a1__0000002A"),
        ] {
            assert_eq!(default_name(table, column, 42), expected);
        }
    }

    #[test]
    fn computed_nullability_follows_sql_server() {
        let nullable = |name: &str| name != "a";
        for (source, expected) in [
            ("a + b", true),
            ("a * 2", true),
            ("CAST(a AS VARCHAR(10))", true),
            ("-a", true),
            ("a", false),
            ("(1)", false),
            ("-1", false),
            ("NULL", true),
            ("year(d)", true),
            ("ISNULL(b, a)", false),
            ("ISNULL(b, 0)", false),
            ("ISNULL(b, c)", true),
            ("COALESCE(b, 0)", true),
            ("CASE WHEN a > 0 THEN a ELSE 0 END", false),
            ("CASE WHEN a IN (1,2) THEN 1 END", true),
            ("CASE WHEN a > 0 THEN b ELSE 0 END", true),
            ("lower(s)", true),
        ] {
            let expr = definition::parse_expression(source).unwrap();
            assert_eq!(computed_nullable(&expr, &nullable), expected, "{source}");
        }
    }

    #[test]
    fn generated_expressions_are_recognized() {
        let expr = |sql: &str| definition::parse_expression(sql).unwrap();
        assert!(generated(&expr("nextval('main.__msduck_identity_1')")));
        assert!(generated(&expr("getvariable('__msduck_session_login')")));
        assert!(!generated(&expr("CAST(1 AS BIGINT)")));
        assert!(!generated(&expr("SUSER_SNAME()")));
    }
}
