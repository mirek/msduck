//! SQL Server table hints (`FROM t WITH (NOLOCK)`), which DuckDB does not
//! parse. msduck serializes statements through its own session state, so
//! accepted locking, isolation and access-path hints have no effect beyond
//! the SQL Server compile-time checks captured in
//! reference/tedious-compat-gaps.json.
//!
//! [`normalize`] runs on a parsed batch: it turns the legacy `t (NOLOCK)`
//! form into ordinary hints and reports the captured errors. The root
//! adapter removes the hints with [`clear`] when it lowers a table factor.
use msduck_core::diagnostic::SqlError;
use sqlparser::ast::*;
use std::ops::ControlFlow;

/// Hints accepted and ignored. HOLDLOCK is SERIALIZABLE and NOLOCK is
/// READUNCOMMITTED.
const ACCEPTED: &[&str] = &[
    "NOLOCK",
    "READUNCOMMITTED",
    "READCOMMITTED",
    "READCOMMITTEDLOCK",
    "REPEATABLEREAD",
    "SERIALIZABLE",
    "HOLDLOCK",
    "UPDLOCK",
    "XLOCK",
    "ROWLOCK",
    "PAGLOCK",
    "TABLOCK",
    "TABLOCKX",
    "READPAST",
    "NOWAIT",
    "FORCESCAN",
    "FORCESEEK",
];

/// The isolation level a hint selects, for the conflict check (1047).
fn isolation(hint: &str) -> Option<&'static str> {
    Some(match hint {
        "NOLOCK" | "READUNCOMMITTED" => "READUNCOMMITTED",
        "READCOMMITTED" => "READCOMMITTED",
        "READCOMMITTEDLOCK" => "READCOMMITTEDLOCK",
        "REPEATABLEREAD" => "REPEATABLEREAD",
        "SERIALIZABLE" | "HOLDLOCK" => "SERIALIZABLE",
        _ => return None,
    })
}

fn is_index(name: &ObjectName) -> bool {
    matches!(name.0.as_slice(), [ObjectNamePart::Identifier(id)] if id.quote_style.is_none() && id.value.eq_ignore_ascii_case("INDEX"))
}

/// The upper-cased name of a simple hint, or None for INDEX forms.
fn simple(hint: &Expr) -> Result<Option<String>, SqlError> {
    match hint {
        Expr::Identifier(id) => Ok(Some(id.value.to_ascii_uppercase())),
        // INDEX(0), INDEX(ix, ...) and INDEX = n choose an access path.
        Expr::Function(function) if is_index(&function.name) => Ok(None),
        Expr::BinaryOp {
            left,
            op: BinaryOperator::Eq,
            ..
        } if matches!(left.as_ref(), Expr::Identifier(id) if id.value.eq_ignore_ascii_case("INDEX")) => {
            Ok(None)
        }
        other => Err(SqlError::syntax(
            321,
            1,
            format!("\"{other}\" is not a recognized table hints option."),
        )),
    }
}

/// A legacy hint list without WITH, `t (NOLOCK)`, parses as table function
/// arguments. Only a list of accepted hint names is treated as hints.
fn legacy_hints(args: &TableFunctionArgs) -> Option<Vec<Expr>> {
    if args.settings.is_some() || args.args.is_empty() {
        return None;
    }
    args.args
        .iter()
        .map(|arg| match arg {
            FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Identifier(id)))
                if id.quote_style.is_none()
                    && ACCEPTED.contains(&id.value.to_ascii_uppercase().as_str()) =>
            {
                Some(Expr::Identifier(id.clone()))
            }
            _ => None,
        })
        .collect()
}

fn check(name: &ObjectName, hints: &[Expr], target: bool) -> Result<(), SqlError> {
    let mut level: Option<&str> = None;
    for hint in hints {
        let Some(upper) = simple(hint)? else {
            continue;
        };
        match upper.as_str() {
            "SNAPSHOT" => {
                return Err(SqlError::new(
                    367,
                    1,
                    "The hint 'SNAPSHOT' is valid only with memory optimized tables.",
                ));
            }
            "NOEXPAND" => {
                let Expr::Identifier(id) = hint else {
                    unreachable!()
                };
                let object = name
                    .0
                    .last()
                    .and_then(ObjectNamePart::as_ident)
                    .map_or_else(|| name.to_string(), |id| id.value.clone());
                return Err(SqlError::new(
                    8171,
                    2,
                    format!("Hint '{}' on object '{object}' is invalid.", id.value),
                ));
            }
            known if ACCEPTED.contains(&known) => {}
            _ => {
                let Expr::Identifier(id) = hint else {
                    unreachable!()
                };
                return Err(SqlError::syntax(
                    321,
                    1,
                    format!("\"{}\" is not a recognized table hints option.", id.value),
                ));
            }
        }
        if let Some(selected) = isolation(&upper) {
            if level.is_some_and(|level| level != selected) {
                return Err(SqlError::syntax(
                    1047,
                    1,
                    "Conflicting locking hints specified.",
                ));
            }
            level = Some(selected);
        }
    }
    if target && level == Some("READUNCOMMITTED") {
        return Err(SqlError::syntax(
            1065,
            1,
            "The NOLOCK and READUNCOMMITTED lock hints are not allowed for target tables of INSERT, UPDATE, DELETE or MERGE statements.",
        ));
    }
    Ok(())
}

/// The table an UPDATE or DELETE modifies directly, when it carries hints.
fn is_target(statement: &Statement, factor: &TableFactor) -> bool {
    let same = |relation: &TableFactor| std::ptr::eq(relation, factor);
    match statement {
        Statement::Update(update) => same(&update.table.relation),
        Statement::Delete(delete) => {
            let (FromTable::WithFromKeyword(tables) | FromTable::WithoutKeyword(tables)) =
                &delete.from;
            delete.tables.is_empty() && tables.first().is_some_and(|t| same(&t.relation))
        }
        _ => false,
    }
}

/// Convert legacy hint lists and report the captured compile-time errors.
pub fn normalize(statements: &mut [Statement]) -> Result<(), SqlError> {
    struct Legacy;
    impl VisitorMut for Legacy {
        type Break = ();
        fn pre_visit_table_factor(&mut self, factor: &mut TableFactor) -> ControlFlow<()> {
            if let TableFactor::Table {
                args, with_hints, ..
            } = factor
                && with_hints.is_empty()
                && let Some(hints) = args.as_ref().and_then(legacy_hints)
            {
                *args = None;
                *with_hints = hints;
            }
            ControlFlow::Continue(())
        }
    }
    struct Check<'a>(&'a Statement);
    impl Visitor for Check<'_> {
        type Break = SqlError;
        fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<SqlError> {
            if let TableFactor::Table {
                name, with_hints, ..
            } = factor
                && let Err(error) = check(name, with_hints, is_target(self.0, factor))
            {
                return ControlFlow::Break(error);
            }
            ControlFlow::Continue(())
        }
    }
    for statement in statements.iter_mut() {
        let _ = statement.visit(&mut Legacy);
    }
    for statement in statements.iter() {
        if let ControlFlow::Break(error) = statement.visit(&mut Check(statement)) {
            return Err(error);
        }
    }
    Ok(())
}

/// sqlparser cannot parse hints on an INSERT target:
/// `INSERT [INTO] name WITH (TABLOCK) ...`. When every hint is an accepted
/// name that is valid on a target, the hint list is dropped before parsing.
/// Other lists are left for the parser to reject.
pub fn strip_insert_hints(tokens: &mut Vec<sqlparser::tokenizer::TokenWithSpan>) {
    use super::key_index_type::{is_word, significant};
    use sqlparser::tokenizer::Token;
    let significant = significant(tokens);
    let token = |position: usize| significant.get(position).map(|&index| &tokens[index].token);
    let mut remove = Vec::new();
    let mut position = 0;
    while position < significant.len() {
        if !token(position).is_some_and(|t| is_word(t, "INSERT")) {
            position += 1;
            continue;
        }
        let mut at = position + 1;
        if token(at).is_some_and(|t| is_word(t, "INTO")) {
            at += 1;
        }
        // A one- to four-part object name.
        let mut parts = 0;
        while matches!(token(at), Some(Token::Word(_))) {
            parts += 1;
            at += 1;
            if token(at) != Some(&Token::Period) {
                break;
            }
            at += 1;
        }
        if parts == 0 || parts > 4 || !token(at).is_some_and(|t| is_word(t, "WITH")) {
            position = at.max(position + 1);
            continue;
        }
        let start = at;
        let mut valid = token(at + 1) == Some(&Token::LParen);
        at += 2;
        loop {
            match token(at) {
                Some(Token::Word(w))
                    if w.quote_style.is_none()
                        && ACCEPTED.contains(&w.value.to_ascii_uppercase().as_str())
                        && isolation(&w.value.to_ascii_uppercase()) != Some("READUNCOMMITTED") => {}
                _ => valid = false,
            }
            at += 1;
            match token(at) {
                Some(Token::Comma) => at += 1,
                Some(Token::RParen) => break,
                _ => {
                    valid = false;
                    break;
                }
            }
            if !valid {
                break;
            }
        }
        if valid {
            remove.extend(significant[start]..=significant[at]);
        }
        position = at + 1;
    }
    for index in remove.into_iter().rev() {
        tokens.remove(index);
    }
}

/// Remove the hints of a validated table factor before backend lowering.
pub fn clear(factor: &mut TableFactor) {
    if let TableFactor::Table { with_hints, .. } = factor {
        with_hints.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(sql: &str) -> Result<Vec<Statement>, SqlError> {
        let mut statements = crate::batch::parse(sql).unwrap();
        normalize(&mut statements)?;
        Ok(statements)
    }

    #[test]
    fn accepted_hints_and_legacy_lists() {
        for hint in ACCEPTED {
            run(&format!("SELECT COUNT(*) FROM t WITH ({hint})")).unwrap();
        }
        run("SELECT COUNT(*) FROM t WITH (NOLOCK, INDEX(0), INDEX = 1, UPDLOCK)").unwrap();
        run("SELECT * FROM t AS a WITH (NOLOCK) JOIN u b WITH (NOLOCK) ON 1 = 1").unwrap();
        run("UPDATE t WITH (ROWLOCK) SET id = id WHERE id = 1").unwrap();
        run("DELETE FROM t WITH (ROWLOCK, READPAST) WHERE id = 3").unwrap();
        run("UPDATE h SET id = h.id FROM t h WITH (UPDLOCK) WHERE h.id = 2").unwrap();
        let statements = run("INSERT INTO dbo.t WITH (TABLOCK, ROWLOCK) (id) VALUES (3)").unwrap();
        assert_eq!(
            statements[0].to_string(),
            "INSERT INTO dbo.t (id) VALUES (3)"
        );
        let statements = run("INSERT t WITH (TABLOCK) SELECT 1").unwrap();
        assert_eq!(statements[0].to_string(), "INSERT t SELECT 1");
        assert!(crate::batch::parse("INSERT INTO t WITH (NOLOCK) (id) VALUES (3)").is_err());
        let statements = run("SELECT COUNT(*) FROM t (NOLOCK)").unwrap();
        assert_eq!(
            statements[0].to_string(),
            "SELECT COUNT(*) FROM t WITH (NOLOCK)"
        );
        // A table function call keeps its arguments.
        let statements = run("SELECT * FROM f(x)").unwrap();
        assert_eq!(statements[0].to_string(), "SELECT * FROM f(x)");
    }

    #[test]
    fn captured_errors() {
        for (sql, number, state, message) in [
            (
                "SELECT COUNT(*) AS n FROM HintProbe WITH (bogus)",
                321,
                1,
                "\"bogus\" is not a recognized table hints option.",
            ),
            (
                "SELECT COUNT(*) AS n FROM HintProbe WITH (NOLOCK, SERIALIZABLE)",
                1047,
                1,
                "Conflicting locking hints specified.",
            ),
            (
                "SELECT COUNT(*) AS n FROM HintProbe WITH (noexpand)",
                8171,
                2,
                "Hint 'noexpand' on object 'HintProbe' is invalid.",
            ),
            (
                "SELECT COUNT(*) AS n FROM HintProbe WITH (snapshot)",
                367,
                1,
                "The hint 'SNAPSHOT' is valid only with memory optimized tables.",
            ),
            (
                "UPDATE HintProbe WITH (NOLOCK) SET id = id",
                1065,
                1,
                "The NOLOCK and READUNCOMMITTED lock hints are not allowed for target tables of INSERT, UPDATE, DELETE or MERGE statements.",
            ),
        ] {
            let error = run(sql).unwrap_err();
            assert_eq!(
                (error.number, error.state, error.message.as_str()),
                (number, state, message),
                "{sql}"
            );
        }
        // NOLOCK on a joined source of UPDATE ... FROM is not the target.
        run("UPDATE h SET id = 1 FROM t h JOIN u WITH (NOLOCK) ON 1 = 1").unwrap();
    }
}
