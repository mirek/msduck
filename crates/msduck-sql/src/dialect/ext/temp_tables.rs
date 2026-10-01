//! Syntax for #temp tables and table variables.
//!
//! `DECLARE @t TABLE (...)` is parsed into an ordinary scalar declaration of
//! `@t` that carries the table definition, so batch preflight treats the
//! name like any other local variable: a second declaration of `@t` fails
//! with 134, and `OUTPUT ... INTO @t` passes the variable checks. The engine
//! feature (`src/engine/ext/temp_tables.rs`) recognizes the declaration with
//! [`table_variable`] before the scalar path sees it.
//!
//! [`classify`] recognizes `#local`, `##global` and `@variable` names, also
//! when written as `dbo.#t`, `tempdb..#t` or `tempdb.dbo.#t`. [`rewrite`]
//! replaces every such reference in a statement (relations, column
//! qualifiers, `SELECT ... INTO`, `OUTPUT ... INTO`, `DROP TABLE` and
//! `DELETE` targets) with the backend table the caller resolves.
use sqlparser::{
    ast::*,
    keywords::Keyword,
    parser::{Parser, ParserError},
    tokenizer::Token,
};
use std::ops::ControlFlow;

/// Marks the declaration that carries a table variable's definition.
const MARKER: &str = "__msduck_table_variable";

/// Parse `DECLARE @name [AS] TABLE (...)`, or decline without consuming
/// tokens.
pub fn parse(parser: &mut Parser) -> Option<Result<Statement, ParserError>> {
    let [declare, name, next, after] = parser.peek_tokens::<4>();
    let is_word = |token: &Token, keyword: Keyword| matches!(token, Token::Word(w) if w.keyword == keyword && w.quote_style.is_none());
    if !is_word(&declare, Keyword::DECLARE) {
        return None;
    }
    let Token::Word(name) = name else {
        return None;
    };
    if name.quote_style.is_some() || !name.value.starts_with('@') || name.value.starts_with("@@") {
        return None;
    }
    if !(is_word(&next, Keyword::TABLE)
        || is_word(&next, Keyword::AS) && is_word(&after, Keyword::TABLE))
    {
        return None;
    }
    Some(parse_declaration(parser))
}

fn parse_declaration(parser: &mut Parser) -> Result<Statement, ParserError> {
    parser.expect_keyword_is(Keyword::DECLARE)?;
    let name = parser.parse_identifier()?;
    let _ = parser.parse_keyword(Keyword::AS);
    parser.expect_keyword_is(Keyword::TABLE)?;
    parser.expect_token(&Token::LParen)?;
    // Keep the column list as text; the engine creates the table from it
    // through the ordinary CREATE TABLE path.
    let mut depth = 1usize;
    let mut definition = Vec::new();
    loop {
        let token = parser.next_token();
        match &token.token {
            Token::EOF => return parser.expected("')'", token),
            Token::LParen => depth += 1,
            Token::RParen => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
        definition.push(token.token.to_string());
    }
    let definition = definition.join(" ");
    if matches!(parser.peek_token().token, Token::Comma) {
        // SQL Server rejects further declarators after a table variable.
        return parser.expected("end of the table variable declaration", parser.peek_token());
    }
    // Validate the definition now, so malformed declarations fail like any
    // other syntax error rather than when the declaration executes.
    create_table(&definition, &Ident::new("t"))
        .map_err(|error| ParserError::ParserError(error.to_string()))?;
    let marker = |value: &str| {
        SelectItem::UnnamedExpr(Expr::Value(
            Value::SingleQuotedString(value.to_owned()).into(),
        ))
    };
    let mut query = Parser::new(&sqlparser::dialect::MsSqlDialect {})
        .try_with_sql("SELECT 1")?
        .parse_query()?;
    if let SetExpr::Select(select) = query.body.as_mut() {
        select.projection = vec![marker(MARKER), marker(&definition)];
    }
    Ok(Statement::Declare {
        stmts: vec![Declare {
            names: vec![name],
            data_type: Some(DataType::Int(None)),
            assignment: None,
            declare_type: None,
            binary: None,
            sensitive: None,
            scroll: None,
            hold: None,
            for_query: Some(query),
        }],
    })
}

/// A table variable declaration: its name and column list text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableVariable {
    pub name: Ident,
    pub definition: String,
}

/// Decode a declaration produced by [`parse`].
pub fn table_variable(statement: &Statement) -> Option<TableVariable> {
    let Statement::Declare { stmts } = statement else {
        return None;
    };
    let [declaration] = stmts.as_slice() else {
        return None;
    };
    let query = declaration.for_query.as_ref()?;
    if declaration.declare_type.is_some() || declaration.names.len() != 1 {
        return None;
    }
    let SetExpr::Select(select) = query.body.as_ref() else {
        return None;
    };
    let text = |item: &SelectItem| match item {
        SelectItem::UnnamedExpr(Expr::Value(ValueWithSpan {
            value: Value::SingleQuotedString(value),
            ..
        })) => Some(value.clone()),
        _ => None,
    };
    match select.projection.as_slice() {
        [marker, definition] if text(marker).as_deref() == Some(MARKER) => Some(TableVariable {
            name: declaration.names[0].clone(),
            definition: text(definition)?,
        }),
        _ => None,
    }
}

/// Whether `statement` declares a table variable.
pub fn is_declaration(statement: &Statement) -> bool {
    table_variable(statement).is_some()
}

/// `CREATE TABLE dbo.<name> (<definition>)`, parsed with the server dialect.
pub fn create_table(definition: &str, name: &Ident) -> anyhow::Result<Statement> {
    let sql = format!("CREATE TABLE dbo.{name} ({definition})");
    let mut statements = crate::batch::parse(&sql)?;
    anyhow::ensure!(
        statements.len() == 1 && matches!(statements[0], Statement::CreateTable(_)),
        "invalid table variable definition"
    );
    Ok(statements.remove(0))
}

/// Statements this feature validates itself; none, since table variable
/// declarations are ordinary declarations for preflight.
pub fn owns(_statement: &Statement) -> bool {
    false
}

/// Which kind of temporary object a name refers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `#name`: private to the session (or the procedure that created it).
    Local,
    /// `##name`: visible to every session.
    Global,
    /// `@name`: a table variable.
    Variable,
}

/// A reference to a temporary object, as written (including `#`/`@`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TempName {
    pub kind: Kind,
    pub name: String,
}

impl TempName {
    /// Names compare case-insensitively.
    pub fn key(&self) -> String {
        self.name.to_lowercase()
    }
}

fn kind(name: &str) -> Option<Kind> {
    if name.starts_with("##") {
        (name.len() > 2).then_some(Kind::Global)
    } else if name.starts_with('#') {
        (name.len() > 1).then_some(Kind::Local)
    } else if name.starts_with('@') && !name.starts_with("@@") {
        (name.len() > 1).then_some(Kind::Variable)
    } else {
        None
    }
}

/// Classify the parts of a (possibly qualified) object name.
pub fn classify_parts(parts: &[&Ident]) -> Option<TempName> {
    let last = parts.last()?;
    let kind = kind(&last.value)?;
    let qualified = match parts.len() {
        1 => true,
        // `dbo.#t`: SQL Server ignores the schema of a temporary table.
        2 => kind != Kind::Variable,
        // `tempdb..#t` and `tempdb.dbo.#t`.
        3 => kind != Kind::Variable && parts[0].value.eq_ignore_ascii_case("tempdb"),
        _ => false,
    };
    qualified.then(|| TempName {
        kind,
        name: last.value.clone(),
    })
}

/// Classify an object name such as `#t`, `dbo.#t` or `tempdb..#t`.
pub fn classify(name: &ObjectName) -> Option<TempName> {
    let parts = name
        .0
        .iter()
        .map(|part| part.as_ident())
        .collect::<Option<Vec<_>>>()?;
    classify_parts(&parts)
}

/// `dbo.<physical>`, the backend name of a resolved temporary object.
pub fn backend_name(physical: &str) -> ObjectName {
    ObjectName::from(vec![Ident::new("dbo"), Ident::with_quote('"', physical)])
}

/// Every temporary name `statement` references, in visiting order (which
/// follows the text). Declarations are not references.
pub fn references<T: Visit>(node: &T) -> Vec<TempName> {
    let mut names = Vec::new();
    let mut collect = Collect(&mut names);
    let _ = node.visit(&mut collect);
    names
}

struct Collect<'a>(&'a mut Vec<TempName>);
impl Visitor for Collect<'_> {
    type Break = ();
    fn pre_visit_relation(&mut self, relation: &ObjectName) -> ControlFlow<()> {
        self.0.extend(classify(relation));
        ControlFlow::Continue(())
    }
}

/// Read `tempdb.sys.*` and `tempdb.INFORMATION_SCHEMA.*` from the current
/// database, whose catalog holds the backend tables of temporary objects.
/// Returns whether anything changed.
pub fn tempdb_catalog<T: VisitMut>(node: &mut T) -> bool {
    let mut changed = false;
    let _ = visit_relations_mut(node, |relation| {
        if let [
            ObjectNamePart::Identifier(database),
            ObjectNamePart::Identifier(schema),
            _,
        ] = relation.0.as_slice()
            && database.value.eq_ignore_ascii_case("tempdb")
            && (schema.value.eq_ignore_ascii_case("sys")
                || schema.value.eq_ignore_ascii_case("information_schema"))
        {
            relation.0.remove(0);
            changed = true;
        }
        ControlFlow::<()>::Continue(())
    });
    changed
}

/// Temporary objects a statement writes: INSERT, UPDATE, DELETE and MERGE
/// targets and `OUTPUT ... INTO` tables, at any nesting depth.
pub fn targets<T: Visit>(node: &T) -> Vec<TempName> {
    struct Targets(Vec<TempName>);
    impl Targets {
        fn output(&mut self, output: &Option<OutputClause>) {
            if let Some(OutputClause::Output {
                into_table: Some(into),
                ..
            }) = output
            {
                for target in &into.targets {
                    let parts = match target {
                        Expr::Identifier(id) => vec![id],
                        Expr::CompoundIdentifier(ids) => ids.iter().collect(),
                        _ => continue,
                    };
                    self.0.extend(classify_parts(&parts));
                }
            }
        }
        fn factor(&mut self, factor: &TableFactor) {
            if let TableFactor::Table { name, .. } = factor {
                self.0.extend(classify(name));
            }
        }
    }
    impl Visitor for Targets {
        type Break = ();
        fn pre_visit_statement(&mut self, statement: &Statement) -> ControlFlow<()> {
            match statement {
                Statement::Insert(insert) => {
                    if let TableObject::TableName(name) = &insert.table {
                        self.0.extend(classify(name));
                    }
                    self.output(&insert.output);
                }
                Statement::Update(update) => {
                    self.factor(&update.table.relation);
                    // `UPDATE alias SET ... FROM @t alias` writes the aliased table.
                    if let Some(
                        UpdateTableFromKind::AfterSet(from) | UpdateTableFromKind::BeforeSet(from),
                    ) = &update.from
                    {
                        for table in from {
                            self.factor(&table.relation);
                            for join in &table.joins {
                                self.factor(&join.relation);
                            }
                        }
                    }
                    self.output(&update.output);
                }
                Statement::Delete(delete) => {
                    for name in &delete.tables {
                        self.0.extend(classify(name));
                    }
                    let (FromTable::WithFromKeyword(from) | FromTable::WithoutKeyword(from)) =
                        &delete.from;
                    for table in from {
                        self.factor(&table.relation);
                        for join in &table.joins {
                            self.factor(&join.relation);
                        }
                    }
                    self.output(&delete.output);
                }
                Statement::Merge(merge) => {
                    self.factor(&merge.table);
                    self.output(&merge.output);
                }
                _ => {}
            }
            ControlFlow::Continue(())
        }
    }
    let mut targets = Targets(Vec::new());
    let _ = node.visit(&mut targets);
    targets.0
}

/// Replace every temporary-object reference in `node` with the backend
/// table `resolve` returns for it (`Ok(None)` leaves the reference as it
/// is). Column qualifiers such as `#t.id` become the backend table name, so
/// they keep matching an unaliased table. Returns whether anything changed.
pub fn rewrite<T, E>(
    node: &mut T,
    resolve: &mut dyn FnMut(&TempName) -> Result<Option<String>, E>,
) -> Result<bool, E>
where
    T: VisitMut,
{
    let mut rewrite = Rewrite {
        resolve,
        changed: false,
    };
    match node.visit(&mut rewrite) {
        ControlFlow::Continue(()) => Ok(rewrite.changed),
        ControlFlow::Break(error) => Err(error),
    }
}

struct Rewrite<'a, E> {
    resolve: &'a mut dyn FnMut(&TempName) -> Result<Option<String>, E>,
    changed: bool,
}

impl<E> Rewrite<'_, E> {
    fn physical(&mut self, name: &TempName) -> ControlFlow<E, Option<String>> {
        match (self.resolve)(name) {
            Ok(physical) => ControlFlow::Continue(physical),
            Err(error) => ControlFlow::Break(error),
        }
    }

    fn object(&mut self, name: &mut ObjectName) -> ControlFlow<E> {
        if let Some(temp) = classify(name)
            && let Some(physical) = self.physical(&temp)?
        {
            *name = backend_name(&physical);
            self.changed = true;
        }
        ControlFlow::Continue(())
    }

    /// `SELECT ... INTO` and `OUTPUT ... INTO` targets are expressions.
    fn target(&mut self, target: &mut Expr) -> ControlFlow<E> {
        let parts = match target {
            Expr::Identifier(id) => vec![&*id],
            Expr::CompoundIdentifier(ids) => ids.iter().collect(),
            _ => return ControlFlow::Continue(()),
        };
        if let Some(temp) = classify_parts(&parts)
            && let Some(physical) = self.physical(&temp)?
        {
            *target = Expr::CompoundIdentifier(
                backend_name(&physical)
                    .0
                    .into_iter()
                    .filter_map(|part| part.as_ident().cloned())
                    .collect(),
            );
            self.changed = true;
        }
        ControlFlow::Continue(())
    }

    /// The prefix of a qualified column (`#t.id`, `tempdb..#t.id`).
    fn qualifier(&mut self, parts: &mut Vec<Ident>) -> ControlFlow<E> {
        if parts.len() < 2 {
            return ControlFlow::Continue(());
        }
        let prefix = parts[..parts.len() - 1].iter().collect::<Vec<_>>();
        if let Some(temp) = classify_parts(&prefix)
            && let Some(physical) = self.physical(&temp)?
        {
            let column = parts.pop().expect("at least two parts");
            *parts = vec![Ident::with_quote('"', physical), column];
            self.changed = true;
        }
        ControlFlow::Continue(())
    }

    fn output(&mut self, output: &mut Option<OutputClause>) -> ControlFlow<E> {
        if let Some(OutputClause::Output {
            into_table: Some(into),
            ..
        }) = output
        {
            for target in &mut into.targets {
                self.target(target)?;
            }
        }
        ControlFlow::Continue(())
    }
}

impl<E> VisitorMut for Rewrite<'_, E> {
    type Break = E;

    fn pre_visit_relation(&mut self, relation: &mut ObjectName) -> ControlFlow<E> {
        self.object(relation)
    }

    fn pre_visit_statement(&mut self, statement: &mut Statement) -> ControlFlow<E> {
        match statement {
            Statement::Drop {
                object_type: ObjectType::Table,
                names,
                ..
            } => {
                for name in names {
                    self.object(name)?;
                }
            }
            Statement::Delete(delete) => {
                for name in &mut delete.tables {
                    self.object(name)?;
                }
                self.output(&mut delete.output)?;
            }
            Statement::Insert(insert) => self.output(&mut insert.output)?,
            Statement::Update(update) => {
                self.output(&mut update.output)?;
                // `SET #t.col = ...` names the target's own column.
                for assignment in &mut update.assignments {
                    if let AssignmentTarget::ColumnName(column) = &mut assignment.target
                        && column.0.len() >= 2
                    {
                        let parts = column.0[..column.0.len() - 1]
                            .iter()
                            .map(|part| part.as_ident())
                            .collect::<Option<Vec<_>>>();
                        if parts.is_some_and(|parts| classify_parts(&parts).is_some()) {
                            let last = column.0.pop().expect("at least two parts");
                            column.0 = vec![last];
                            self.changed = true;
                        }
                    }
                }
            }
            Statement::Merge(merge) => self.output(&mut merge.output)?,
            Statement::Set(Set::SetSessionParam(SetSessionParamKind::IdentityInsert(setting))) => {
                self.object(&mut setting.obj)?;
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_select(&mut self, select: &mut Select) -> ControlFlow<E> {
        if let Some(into) = &mut select.into {
            for target in &mut into.targets {
                self.target(target)?;
            }
        }
        for item in &mut select.projection {
            if let SelectItem::QualifiedWildcard(
                SelectItemQualifiedWildcardKind::ObjectName(name),
                _,
            ) = item
                && let Some(temp) = classify(name)
                && let Some(physical) = self.physical(&temp)?
            {
                *name = ObjectName::from(vec![Ident::with_quote('"', physical)]);
                self.changed = true;
            }
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<E> {
        if let Expr::CompoundIdentifier(parts) = expr {
            self.qualifier(parts)?;
        }
        ControlFlow::Continue(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn statement(sql: &str) -> Statement {
        crate::batch::parse(sql).unwrap().remove(0)
    }

    #[test]
    fn table_variable_declarations_carry_their_definition() {
        let declared =
            statement("DECLARE @t AS TABLE (id INT PRIMARY KEY, name NVARCHAR(10) DEFAULT N'x')");
        let variable = table_variable(&declared).unwrap();
        assert_eq!(variable.name.value, "@t");
        assert!(is_declaration(&declared));
        let create = create_table(&variable.definition, &Ident::new("x")).unwrap();
        let Statement::CreateTable(table) = create else {
            unreachable!()
        };
        assert_eq!(table.columns.len(), 2);
        assert_eq!(table.columns[0].name.value, "id");
        // Scalar declarations and cursors are not table variables.
        assert!(table_variable(&statement("DECLARE @x INT")).is_none());
        assert!(crate::batch::parse("DECLARE @t TABLE (id INT), @x INT").is_err());
        assert!(crate::batch::parse("DECLARE @t TABLE (id INT").is_err());
    }

    #[test]
    fn names_classify_by_prefix_and_tempdb_qualification() {
        let name = |sql: &str| {
            let Statement::Query(query) = statement(&format!("SELECT * FROM {sql}")) else {
                unreachable!()
            };
            let SetExpr::Select(select) = *query.body else {
                unreachable!()
            };
            let TableFactor::Table { name, .. } = &select.from[0].relation else {
                unreachable!()
            };
            classify(name)
        };
        assert_eq!(name("#t").unwrap().kind, Kind::Local);
        assert_eq!(name("##t").unwrap().kind, Kind::Global);
        assert_eq!(name("@t").unwrap().kind, Kind::Variable);
        assert_eq!(name("tempdb..#t").unwrap().name, "#t");
        assert_eq!(name("tempdb.dbo.#T").unwrap().key(), "#t");
        assert_eq!(name("dbo.#t").unwrap().kind, Kind::Local);
        assert!(name("other.dbo.#t").is_none());
        assert!(name("dbo.t").is_none());
        assert!(name("dbo.@t").is_none());
    }

    #[test]
    fn rewrite_replaces_relations_qualifiers_and_targets() {
        let mut resolve = |name: &TempName| -> Result<Option<String>, String> {
            match name.key().as_str() {
                "#t" => Ok(Some("tt".into())),
                "@v" => Ok(Some("vv".into())),
                other => Err(other.to_owned()),
            }
        };
        let mut query = statement(
            "SELECT #t.id, v.id, #t.* FROM tempdb..#t JOIN @v AS v ON v.id = #t.id WHERE EXISTS (SELECT 1 FROM #t)",
        );
        assert!(rewrite(&mut query, &mut resolve).unwrap());
        assert_eq!(
            query.to_string(),
            r#"SELECT "tt".id, v.id, "tt".* FROM dbo."tt" JOIN dbo."vv" AS v ON v.id = "tt".id WHERE EXISTS (SELECT 1 FROM dbo."tt")"#
        );
        let mut insert = statement("INSERT INTO #t (id) OUTPUT inserted.id INTO @v SELECT 1");
        rewrite(&mut insert, &mut resolve).unwrap();
        assert_eq!(
            insert.to_string(),
            r#"INSERT INTO dbo."tt" (id) OUTPUT inserted.id INTO dbo."vv" SELECT 1"#
        );
        let mut update = statement("UPDATE #t SET #t.id = 2 WHERE #t.id = 1");
        rewrite(&mut update, &mut resolve).unwrap();
        assert_eq!(
            update.to_string(),
            r#"UPDATE dbo."tt" SET id = 2 WHERE "tt".id = 1"#
        );
        let mut drop = statement("DROP TABLE #t, dbo.other");
        rewrite(&mut drop, &mut resolve).unwrap();
        assert_eq!(drop.to_string(), r#"DROP TABLE dbo."tt", dbo.other"#);
        let mut missing = statement("SELECT * FROM #missing");
        assert_eq!(rewrite(&mut missing, &mut resolve), Err("#missing".into()));
        let mut catalog = statement(
            "SELECT c.name FROM tempdb.sys.columns c JOIN tempdb.INFORMATION_SCHEMA.TABLES t ON 1 = 1",
        );
        assert!(tempdb_catalog(&mut catalog));
        assert_eq!(
            catalog.to_string(),
            "SELECT c.name FROM sys.columns c JOIN INFORMATION_SCHEMA.TABLES t ON 1 = 1"
        );
        let mut plain = statement("SELECT * FROM dbo.t");
        assert!(!rewrite(&mut plain, &mut resolve).unwrap());
        assert_eq!(
            references(&statement("SELECT * FROM #a JOIN @b ON 1 = 1"))
                .into_iter()
                .map(|name| name.name)
                .collect::<Vec<_>>(),
            ["#a", "@b"]
        );
    }
}
