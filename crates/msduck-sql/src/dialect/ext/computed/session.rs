//! Session functions in stored column DEFAULTs.
//!
//! SQL Server evaluates a DEFAULT for each insert, in the inserting session.
//! The root adapter mirrors each session's login, client names and
//! `SESSION_CONTEXT` values into DuckDB variables of that session's
//! connection (variables are per connection). This module rewrites the
//! session functions of a DEFAULT into reads of those variables, so DuckDB
//! evaluates the stored default with the inserting session's values.
//!
//! - `SUSER_SNAME()`, `SUSER_NAME()`, `SYSTEM_USER` and `ORIGINAL_LOGIN()`
//!   read the login; `HOST_NAME()` and `APP_NAME()` the LOGIN7 client names.
//!   All are nullable `nvarchar(128)`.
//! - `SESSION_CONTEXT(N'key')` is a `sql_variant`. Inside an explicit
//!   `CAST`/`CONVERT` it converts from its base type, so the conversion is
//!   applied to each supported base type in turn and the stored kind selects
//!   one. A bare value converts implicitly to the column type, which SQL
//!   Server refuses with 257 when the table is created.
use anyhow::{Result, bail};
use msduck_core::diagnostic::SqlError;
use sqlparser::ast::*;
use std::{collections::HashMap, ops::ControlFlow};

use crate::session_function::{self as rules, ContextValue, VariantFunction};

/// The login name.
pub const LOGIN: &str = "__msduck_session_login";
/// The LOGIN7 host name.
pub const HOST: &str = "__msduck_session_host";
/// The LOGIN7 application name.
pub const APP: &str = "__msduck_session_app";
const CONTEXT: &str = "__msduck_session_context_";

/// The variables holding one `SESSION_CONTEXT` key's value text and its base
/// type name. Keys that `keys_match` treats as the same share them.
pub fn context_variables(key: &str) -> (String, String) {
    let (lower, last) = rules::key_identity(key);
    let mut bytes = lower.into_bytes();
    bytes.push(0);
    if let Some(last) = last {
        bytes.extend(last.to_string().bytes());
    }
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    (format!("{CONTEXT}{hex}"), format!("{CONTEXT}{hex}_kind"))
}

/// A stored value as the variables hold it: its base type name and its
/// text. `None` for NULL, which leaves both variables unset.
pub fn context_value(value: &ContextValue) -> Option<(&'static str, String)> {
    Some(match value {
        ContextValue::Null => return None,
        ContextValue::Bit(value) => ("bit", if *value { "1" } else { "0" }.into()),
        ContextValue::TinyInt(value) => ("tinyint", value.to_string()),
        ContextValue::SmallInt(value) => ("smallint", value.to_string()),
        ContextValue::Int(value) => ("int", value.to_string()),
        ContextValue::BigInt(value) => ("bigint", value.to_string()),
        ContextValue::NVarChar { text, .. } => ("nvarchar", text.clone()),
    })
}

/// Base types a context value can have, with the declaration its text is
/// read back as. sp_set_session_context values are at most 8000 bytes.
fn base_types() -> [(&'static str, DataType); 6] {
    [
        (
            "nvarchar",
            DataType::Nvarchar(Some(CharacterLength::IntegerLength {
                length: 4000,
                unit: None,
            })),
        ),
        ("bit", DataType::Bit(None)),
        ("tinyint", DataType::TinyInt(None)),
        ("smallint", DataType::SmallInt(None)),
        ("int", DataType::Int(None)),
        ("bigint", DataType::BigInt(None)),
    ]
}

fn variable(name: &str) -> Expr {
    crate::expr::unary_function(
        "getvariable",
        Expr::Value(Value::SingleQuotedString(name.into()).into()),
    )
}

fn login_name_type() -> DataType {
    DataType::Nvarchar(Some(CharacterLength::IntegerLength {
        length: 128,
        unit: None,
    }))
}

/// A name variable as a nullable `nvarchar(128)`.
pub fn name_value(name: &str) -> Expr {
    Expr::Cast {
        kind: CastKind::Cast,
        expr: Box::new(variable(name)),
        data_type: login_name_type(),
        format: None,
    }
}

fn function_name(function: &Function) -> Option<&str> {
    match function.name.0.as_slice() {
        [ObjectNamePart::Identifier(id)] if id.quote_style.is_none() => Some(&id.value),
        _ => None,
    }
}

fn no_arguments(function: &Function) -> bool {
    match &function.args {
        FunctionArguments::List(list) => list.args.is_empty() && list.clauses.is_empty(),
        FunctionArguments::None => true,
        FunctionArguments::Subquery(_) => false,
    }
}

/// `HOST_NAME()` or `APP_NAME()`: the variable holding its value.
pub fn client_function(function: &Function) -> Option<&'static str> {
    let name = function_name(function)?;
    let variable = if name.eq_ignore_ascii_case("HOST_NAME") {
        HOST
    } else if name.eq_ignore_ascii_case("APP_NAME") {
        APP
    } else {
        return None;
    };
    (no_arguments(function) && function.over.is_none()).then_some(variable)
}

/// The login and client-name functions: the variable holding the value.
fn name_function(expr: &Expr) -> Option<&'static str> {
    match expr {
        Expr::Function(function) => {
            if let Some(variable) = client_function(function) {
                return Some(variable);
            }
            let name = function_name(function)?;
            let login =
                rules::is_login_name(function) || name.eq_ignore_ascii_case("ORIGINAL_LOGIN");
            (login && no_arguments(function) && function.over.is_none()).then_some(LOGIN)
        }
        Expr::Identifier(id) if id.quote_style.is_none() && rules::is_system_user(&id.value) => {
            Some(LOGIN)
        }
        _ => None,
    }
}

/// `HOST_NAME()` and `APP_NAME()` anywhere: lower to the session variables.
/// The value is read when DuckDB binds the statement, so views and defaults
/// see the session that uses them.
pub fn lower_client_name(expr: &mut Expr) {
    if let Expr::Function(function) = expr
        && let Some(variable) = client_function(function)
    {
        *expr = name_value(variable);
    }
}

fn session_context(expr: &Expr) -> Option<&Function> {
    match expr {
        Expr::Function(function)
            if rules::variant_function(function) == Some(VariantFunction::SessionContext) =>
        {
            Some(function)
        }
        Expr::Nested(inner) => session_context(inner),
        _ => None,
    }
}

/// The key of a `SESSION_CONTEXT(key)` call in a stored definition, which
/// must be a constant. `None` is a NULL key.
fn context_key(function: &Function) -> Result<Option<String>> {
    let FunctionArguments::List(list) = &function.args else {
        bail!("unsupported arguments in {function}");
    };
    let [FunctionArg::Unnamed(FunctionArgExpr::Expr(argument))] = list.args.as_slice() else {
        bail!(SqlError::syntax(
            174,
            1,
            "The session_context function requires 1 argument(s)."
        ));
    };
    if function.over.is_some() || function.filter.is_some() || !list.clauses.is_empty() {
        bail!("unsupported modifiers in {function}");
    }
    let argument = rules::name_argument(argument, &HashMap::new()).map_err(|_| {
        anyhow::anyhow!("unsupported non-constant SESSION_CONTEXT key in a DEFAULT")
    })?;
    Ok(rules::context_key(&argument)?.map(str::to_owned))
}

/// The conversion operand that is a SESSION_CONTEXT call, looking through the
/// parser's explicit integer conversion marker.
fn converted_context(expr: &mut Expr) -> Option<&mut Expr> {
    let (operand, target) = match expr {
        Expr::Cast {
            expr, data_type, ..
        } => (expr, &*data_type),
        Expr::Convert {
            expr,
            data_type: Some(data_type),
            ..
        } => (expr, &*data_type),
        _ => return None,
    };
    if crate::variant_pack::is_variant(target) {
        return None;
    }
    if crate::variant_cast::source(operand).is_some() {
        let Expr::Function(marker) = operand.as_mut() else {
            unreachable!("the marker is a function")
        };
        let FunctionArguments::List(list) = &mut marker.args else {
            unreachable!("the marker has one argument")
        };
        let [FunctionArg::Unnamed(FunctionArgExpr::Expr(inner))] = list.args.as_mut_slice() else {
            unreachable!("the marker has one argument")
        };
        return session_context(inner).is_some().then_some(inner);
    }
    session_context(operand)
        .is_some()
        .then_some(operand.as_mut())
}

/// `CAST`/`CONVERT(T, SESSION_CONTEXT(key))`: one conversion per base type,
/// selected by the stored kind; NULL when the key has no value.
fn lower_conversion(expr: &mut Expr) -> Result<bool> {
    let mut template = expr.clone();
    let Some(operand) = converted_context(&mut template) else {
        return Ok(false);
    };
    let Some(function) = session_context(operand) else {
        unreachable!("checked by converted_context")
    };
    let Some(key) = context_key(function)? else {
        *expr = Expr::Cast {
            kind: CastKind::Cast,
            expr: Box::new(Expr::Value(Value::Null.into())),
            data_type: match expr {
                Expr::Cast { data_type, .. } => data_type.clone(),
                Expr::Convert {
                    data_type: Some(data_type),
                    ..
                } => data_type.clone(),
                _ => unreachable!("checked by converted_context"),
            },
            format: None,
        };
        return Ok(true);
    };
    let (value, kind) = context_variables(&key);
    let conditions = base_types()
        .into_iter()
        .map(|(name, base)| {
            let mut branch = template.clone();
            *converted_context(&mut branch).expect("same shape as the template") = Expr::Cast {
                kind: CastKind::Cast,
                expr: Box::new(variable(&value)),
                data_type: base,
                format: None,
            };
            CaseWhen {
                condition: Expr::Value(Value::SingleQuotedString(name.into()).into()),
                result: branch,
            }
        })
        .collect();
    *expr = Expr::Case {
        case_token: sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
        end_token: sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
        operand: Some(Box::new(variable(&kind))),
        conditions,
        else_result: None,
    };
    Ok(true)
}

/// SQL Server's lower-case name of a declared type, as conversion errors
/// print it.
fn type_name(kind: &DataType) -> String {
    let text = kind.to_string();
    text.split('(')
        .next()
        .unwrap_or(&text)
        .trim()
        .to_lowercase()
}

/// Rewrite the session functions of one column DEFAULT for a column of
/// type `target`. Returns whether anything changed.
pub fn rewrite_default(default: &mut Expr, target: &DataType) -> Result<bool> {
    let mut bare = default;
    while let Expr::Nested(inner) = bare {
        bare = inner;
    }
    if session_context(bare).is_some() && !crate::variant_pack::is_variant(target) {
        bail!(SqlError::new(
            257,
            3,
            format!(
                "Implicit conversion from data type sql_variant to {} is not allowed. Use the CONVERT function to run this query.",
                type_name(target)
            )
        ));
    }
    struct Rewrite {
        changed: bool,
    }
    impl VisitorMut for Rewrite {
        type Break = anyhow::Error;
        fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<anyhow::Error> {
            match lower_conversion(expr) {
                Ok(true) => {
                    self.changed = true;
                    return ControlFlow::Continue(());
                }
                Ok(false) => {}
                Err(error) => return ControlFlow::Break(error),
            }
            if let Some(variable) = name_function(expr) {
                *expr = name_value(variable);
                self.changed = true;
            }
            ControlFlow::Continue(())
        }
        fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<anyhow::Error> {
            if session_context(expr).is_some() {
                return ControlFlow::Break(anyhow::anyhow!(
                    "unsupported SESSION_CONTEXT outside an explicit CAST or CONVERT in a DEFAULT"
                ));
            }
            ControlFlow::Continue(())
        }
    }
    let mut rewrite = Rewrite { changed: false };
    match bare.visit(&mut rewrite) {
        ControlFlow::Continue(()) => Ok(rewrite.changed),
        ControlFlow::Break(error) => Err(error),
    }
}

/// Rewrite the column DEFAULTs of a CREATE TABLE or ALTER TABLE ... ADD.
/// Returns whether any default reads session state.
pub fn rewrite_defaults(statement: &mut Statement) -> Result<bool> {
    let columns: Vec<&mut ColumnDef> = match statement {
        Statement::CreateTable(table) => table.columns.iter_mut().collect(),
        Statement::AlterTable(table) => table
            .operations
            .iter_mut()
            .filter_map(|operation| match operation {
                AlterTableOperation::AddColumn { column_def, .. } => Some(column_def),
                _ => None,
            })
            .collect(),
        _ => return Ok(false),
    };
    let mut changed = false;
    for column in columns {
        let target = column.data_type.clone();
        for option in &mut column.options {
            if let ColumnOption::Default(default) = &mut option.option {
                changed |= rewrite_default(default, &target)?;
            }
        }
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rewritten(sql: &str) -> Result<String> {
        let mut statement = crate::batch::parse(sql).unwrap().remove(0);
        rewrite_defaults(&mut statement)?;
        Ok(statement.to_string())
    }

    #[test]
    fn context_variables_follow_key_identity() {
        assert_eq!(context_variables("Email"), context_variables("eMail  "));
        assert_ne!(context_variables("email"), context_variables("EMAIL"));
        let (value, kind) = context_variables("foo");
        assert_eq!(value, "__msduck_session_context_666f6f006f");
        assert_eq!(kind, "__msduck_session_context_666f6f006f_kind");
        assert_eq!(
            context_value(&ContextValue::Bit(true)),
            Some(("bit", "1".into()))
        );
        assert_eq!(context_value(&ContextValue::Null), None);
    }

    #[test]
    fn name_functions_read_session_variables() {
        let sql = rewritten(
            "CREATE TABLE t (a nvarchar(128) DEFAULT (SUSER_SNAME()), b nvarchar(128) DEFAULT HOST_NAME(), c nvarchar(10) DEFAULT (CONVERT(nvarchar(10), APP_NAME())), d nvarchar(128) DEFAULT SYSTEM_USER, e nvarchar(128) DEFAULT (ORIGINAL_LOGIN()), f int DEFAULT (1))",
        )
        .unwrap();
        for expected in [
            "DEFAULT (CAST(getvariable('__msduck_session_login') AS NVARCHAR(128)))",
            "DEFAULT CAST(getvariable('__msduck_session_host') AS NVARCHAR(128))",
            "CAST(getvariable('__msduck_session_app') AS NVARCHAR(128))",
            "d NVARCHAR(128) DEFAULT CAST(getvariable('__msduck_session_login') AS NVARCHAR(128))",
            "f INT DEFAULT (1)",
        ] {
            assert!(sql.contains(expected), "{sql}");
        }
        // SUSER_SNAME(sid) is a lookup, not the session's login.
        assert!(
            rewritten("CREATE TABLE t (a nvarchar(128) DEFAULT (SUSER_SNAME(0x01)))")
                .unwrap()
                .contains("SUSER_SNAME(")
        );
    }

    #[test]
    fn context_conversions_select_the_stored_base_type() {
        let sql = rewritten(
            "CREATE TABLE t (v nvarchar(100) DEFAULT (CONVERT(nvarchar(100), SESSION_CONTEXT(N'foo'))))",
        )
        .unwrap();
        assert!(
            sql.contains("CASE getvariable('__msduck_session_context_666f6f006f_kind') WHEN 'nvarchar' THEN CONVERT(NVARCHAR(100), CAST(getvariable('__msduck_session_context_666f6f006f') AS NVARCHAR(4000))) WHEN 'bit' THEN CONVERT(NVARCHAR(100), CAST(getvariable('__msduck_session_context_666f6f006f') AS BIT))"),
            "{sql}"
        );
        assert!(sql.contains("WHEN 'bigint' THEN"), "{sql}");
        // The explicit integer conversion marker stays around each operand.
        let sql = rewritten("CREATE TABLE t (n int DEFAULT (CAST(SESSION_CONTEXT(N'n') AS int)))")
            .unwrap();
        assert!(
            sql.contains("WHEN 'int' THEN CAST(__msduck_explicit_integer_source(CAST(getvariable("),
            "{sql}"
        );
        let error =
            rewritten("CREATE TABLE t (n int DEFAULT (CAST(SESSION_CONTEXT(NULL) AS int)))")
                .unwrap_err();
        assert_eq!(error.downcast_ref::<SqlError>().unwrap().number, 8116);
        let mut statement = crate::batch::parse(
            "ALTER TABLE t ADD v varchar(5) NULL DEFAULT (CONVERT(varchar(5), SESSION_CONTEXT(N'k')))",
        )
        .unwrap()
        .remove(0);
        assert!(rewrite_defaults(&mut statement).unwrap());
        assert!(statement.to_string().contains("CASE getvariable("));
    }

    #[test]
    fn bare_and_unsupported_context_uses_are_refused() {
        let error =
            rewritten("CREATE TABLE t (v nvarchar(100) NULL DEFAULT (SESSION_CONTEXT(N'foo')))")
                .unwrap_err();
        let error = error.downcast_ref::<SqlError>().unwrap();
        assert_eq!(
            (error.number, error.state, error.message.as_str()),
            (
                257,
                3,
                "Implicit conversion from data type sql_variant to nvarchar is not allowed. Use the CONVERT function to run this query."
            )
        );
        for sql in [
            "CREATE TABLE t (v sql_variant DEFAULT (SESSION_CONTEXT(N'foo')))",
            "CREATE TABLE t (v nvarchar(10) DEFAULT (ISNULL(SESSION_CONTEXT(N'foo'), N'x')))",
        ] {
            let error = rewritten(sql).unwrap_err();
            assert!(
                error.to_string().starts_with("unsupported SESSION_CONTEXT"),
                "{sql}"
            );
        }
        let error =
            rewritten("CREATE TABLE t (v int DEFAULT (CONVERT(int, SESSION_CONTEXT('foo'))))")
                .unwrap_err();
        assert_eq!(error.downcast_ref::<SqlError>().unwrap().number, 8116);
    }

    #[test]
    fn client_names_lower_anywhere() {
        let mut statement = crate::batch::parse("SELECT HOST_NAME() AS h, APP_NAME()")
            .unwrap()
            .remove(0);
        let _ = visit_expressions_mut(&mut statement, |expr| {
            lower_client_name(expr);
            ControlFlow::<()>::Continue(())
        });
        assert_eq!(
            statement.to_string(),
            "SELECT CAST(getvariable('__msduck_session_host') AS NVARCHAR(128)) AS h, CAST(getvariable('__msduck_session_app') AS NVARCHAR(128))"
        );
    }
}
