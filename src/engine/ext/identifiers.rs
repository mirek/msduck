//! Contextual identifiers and object-qualified error messages.
//!
//! SQL Server accepts many words as regular (unquoted) identifiers that the
//! DuckDB grammar reserves, such as `offset`, `limit`, `qualify` or `at`.
//! The statement reaches DuckDB as rendered SQL, so such a name must be
//! delimited before translation or DuckDB rejects the whole statement with a
//! syntax error. This feature delimits those names where they are names
//! (tables, columns, aliases, CTEs, constraints and indexes), and never in
//! function or type names, where the word is syntax rather than a name.
//! DuckDB identifiers are case-insensitive even when quoted, so delimiting
//! does not change name resolution.
//!
//! Object-qualified diagnostics name the session's current database, as SQL
//! Server does: string truncation (2628) through `storage_diagnostic`, and
//! NOT NULL violations (515) by [`not_null`], which turns DuckDB's constraint
//! message into SQL Server's.
use super::{Execution, Feature, Parameter, Session};
use anyhow::Result;
use sqlparser::ast::{
    Expr, Ident, ObjectName, ObjectNamePart, Query, Statement, VisitMut, VisitorMut,
};
use std::{collections::HashMap, ops::ControlFlow};

mod not_null;

#[derive(Default)]
pub(crate) struct State;

pub(super) struct Hooks;

impl Feature for Hooks {
    fn name(&self) -> &'static str {
        "identifiers"
    }

    fn statement(
        &self,
        session: &mut Session,
        statement: &mut Statement,
        parameters: &mut HashMap<String, Parameter>,
    ) -> Result<Option<Execution>> {
        not_null::execute(session, statement, parameters)
    }

    fn rewrite_statement(
        &self,
        _session: &Session,
        statement: &mut Statement,
        _parameters: &HashMap<String, Parameter>,
    ) -> Result<()> {
        if names_objects(statement) {
            delimit(statement);
        }
        Ok(())
    }

    fn rewrite_expr(
        &self,
        _session: &Session,
        expr: &mut Expr,
        _parameters: &HashMap<String, Parameter>,
    ) -> Result<()> {
        // Scalar evaluations (SET, DECLARE, IF and WHILE conditions, RETURN)
        // arrive as bare expressions; their subqueries name objects. Bare
        // words elsewhere in those statements (such as EXEC arguments) are
        // not object names and stay as written.
        if let Expr::Subquery(query)
        | Expr::Exists {
            subquery: query, ..
        }
        | Expr::InSubquery {
            subquery: query, ..
        } = expr
        {
            delimit_query(query);
        }
        Ok(())
    }
}

/// Statements whose identifiers are object, column or alias names. Other
/// statements (SET options, transaction control, EXEC and so on) keep their
/// words, which the engine interprets as syntax.
fn names_objects(statement: &Statement) -> bool {
    matches!(
        statement,
        Statement::Query(_)
            | Statement::Insert(_)
            | Statement::Update(_)
            | Statement::Delete(_)
            | Statement::CreateTable(_)
            | Statement::AlterTable(_)
            | Statement::CreateView(_)
            | Statement::AlterView { .. }
            | Statement::CreateIndex(_)
            | Statement::Drop { .. }
            | Statement::Truncate(_)
    )
}

/// Whether an unquoted SQL Server identifier must be delimited for DuckDB.
pub(crate) fn needs_delimiter(ident: &Ident) -> bool {
    ident.quote_style.is_none()
        && msduck_sql::dialect::ext::identifiers::reserved_by_backend(&ident.value)
}

fn delimit_ident(ident: &mut Ident) {
    if needs_delimiter(ident) {
        ident.quote_style = Some('"');
    }
}

fn delimit_query(query: &mut Query) {
    let _ = query.visit(&mut Delimit::default());
}

fn delimit(statement: &mut Statement) {
    let _ = statement.visit(&mut Delimit::default());
}

/// Delimits reserved names everywhere except the names of called functions,
/// where the word is syntax (DuckDB's `grouping(...)`). sqlparser visits a function's name parts after
/// the function expression itself, so the visitor records which name parts
/// belong to a function (by address, valid for the duration of the visit)
/// and leaves them alone.
#[derive(Default)]
struct Delimit {
    syntax: Vec<*const Ident>,
}

impl Delimit {
    fn protect(&mut self, name: &ObjectName) {
        self.syntax
            .extend(name.0.iter().filter_map(|part| match part {
                ObjectNamePart::Identifier(ident) => Some(ident as *const Ident),
                _ => None,
            }));
    }
}

impl VisitorMut for Delimit {
    type Break = ();

    fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
        if let Expr::Function(function) = expr {
            self.protect(&function.name);
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_ident(&mut self, ident: &mut Ident) -> ControlFlow<()> {
        let address = ident as *const Ident;
        if let Some(index) = self.syntax.iter().position(|p| *p == address) {
            self.syntax.swap_remove(index);
        } else {
            delimit_ident(ident);
        }
        ControlFlow::Continue(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rewrite(sql: &str) -> String {
        let mut statements = msduck_sql::batch::parse(sql).unwrap();
        for statement in &mut statements {
            if names_objects(statement) {
                delimit(statement);
            }
        }
        statements
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ")
    }

    #[test]
    fn delimits_backend_reserved_names() {
        assert_eq!(
            rewrite("CREATE TABLE items(offset int NOT NULL, at int, id int)"),
            "CREATE TABLE items (\"offset\" INT NOT NULL, \"at\" INT, id INT)"
        );
        assert_eq!(
            rewrite("SELECT at.qualify AS offset FROM dbo.limit AS at WHERE at.qualify = 1"),
            "SELECT \"at\".\"qualify\" AS \"offset\" FROM dbo.\"limit\" AS \"at\" WHERE \"at\".\"qualify\" = 1"
        );
    }

    #[test]
    fn keeps_function_names_and_quoted_names() {
        assert_eq!(
            rewrite("SELECT grouping(at), [offset], \"limit\" FROM t GROUP BY ROLLUP(at)"),
            "SELECT grouping(\"at\"), [offset], \"limit\" FROM t GROUP BY ROLLUP (\"at\")"
        );
    }

    #[test]
    fn leaves_ordinary_and_tsql_names_unchanged() {
        assert_eq!(
            rewrite("SELECT id, name FROM items ORDER BY id OFFSET 1 ROWS"),
            "SELECT id, name FROM items ORDER BY id OFFSET 1 ROWS"
        );
    }
}
