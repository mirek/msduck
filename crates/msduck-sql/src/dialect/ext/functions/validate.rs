//! Compile-time checks SQL Server applies when a function is created.
use super::definition::{Body, Definition, Returns};
use crate::parameter::Parameter;
use anyhow::Result;
use msduck_core::{diagnostic::SqlError, value::Value as ParameterValue};
use sqlparser::ast::*;
use std::{collections::HashMap, ops::ControlFlow};

fn error(number: i32, state: u8, message: impl Into<String>) -> anyhow::Error {
    SqlError::new(number, state, message).into()
}

fn side_effect(operator: &str, state: u8) -> anyhow::Error {
    error(
        443,
        state,
        format!("Invalid use of a side-effecting operator '{operator}' within a function."),
    )
}

/// Reject bodies SQL Server refuses to compile: undeclared variables (137),
/// statements that return data (444) or have side effects (443), a missing
/// final RETURN (455) and RETURN values a table function cannot have (178).
pub fn validate(definition: &Definition) -> Result<()> {
    let mut variables = HashMap::new();
    for parameter in &definition.parameters {
        let data_type = crate::batch::variable_type(parameter.data_type.clone())
            .map_err(|e| error(2715, 6, e.to_string()))?;
        variables.insert(
            parameter.name.to_lowercase(),
            Parameter {
                value: ParameterValue::Null,
                data_type,
            },
        );
    }
    let table_variable = match &definition.returns {
        Returns::Table { variable, .. } => Some(variable.to_lowercase()),
        _ => None,
    };
    match &definition.body {
        Body::Query(query) => {
            let statement = Statement::Query(query.clone());
            declared(std::slice::from_ref(&statement), &variables)?;
            check_expressions(&statement)?;
        }
        Body::Statements(statements) => {
            declared(statements, &variables)?;
            for statement in statements {
                check(statement, definition, table_variable.as_deref())?;
            }
            if !ends_with_return(statements) {
                return Err(error(
                    455,
                    2,
                    "The last statement included within a function must be a return statement.",
                ));
            }
        }
    }
    Ok(())
}

/// Undeclared and redeclared variables, using the batch preflight.
fn declared(statements: &[Statement], variables: &HashMap<String, Parameter>) -> Result<()> {
    let Err(failure) = crate::preflight::variables(statements, variables) else {
        return Ok(());
    };
    let message = failure.to_string();
    if let Some(name) = message.strip_prefix("Must declare the scalar variable ") {
        return Err(error(
            137,
            2,
            format!("Must declare the scalar variable \"{}\".", name.trim()),
        ));
    }
    if message.contains("has already been declared") {
        return Err(error(134, 1, message));
    }
    // Other preflight limits concern execution; the call reports them.
    Ok(())
}

fn ends_with_return(statements: &[Statement]) -> bool {
    match statements.last() {
        Some(Statement::Return(ReturnStatement { value })) => !matches!(
            value,
            Some(ReturnStatementValue::Expr(Expr::Value(ValueWithSpan { value: Value::Placeholder(p), .. }))) if p.starts_with("msduck:")
        ),
        Some(statement) => super::fold::block(statement).is_some_and(ends_with_return),
        None => false,
    }
}

/// Table variables (the return table or local ones) are function-local.
fn target_is(name: &ObjectName, _variable: Option<&str>) -> bool {
    name.0.len() == 1 && name.to_string().starts_with('@')
}

fn check(statement: &Statement, definition: &Definition, variable: Option<&str>) -> Result<()> {
    match statement {
        Statement::Query(query) => {
            if let SetExpr::Select(select) = query.body.as_ref()
                && select.projection.iter().all(|item| {
                    matches!(item, SelectItem::ExprWithAlias { alias, .. }
                        if alias.quote_style.is_none() && alias.value.starts_with('@'))
                })
            {
            } else {
                return Err(error(
                    444,
                    3,
                    "Select statements included within a function cannot return data to a client.",
                ));
            }
        }
        Statement::Insert(insert) => match &insert.table {
            TableObject::TableName(name) if target_is(name, variable) => {}
            _ => return Err(side_effect("INSERT", 15)),
        },
        Statement::Update(update) => match &update.table.relation {
            TableFactor::Table { name, .. } if target_is(name, variable) => {}
            _ => return Err(side_effect("UPDATE", 15)),
        },
        Statement::Delete(delete) => {
            let tables = match &delete.from {
                FromTable::WithFromKeyword(tables) | FromTable::WithoutKeyword(tables) => tables,
            };
            let local = delete.tables.is_empty()
                && tables.len() == 1
                && matches!(&tables[0].relation, TableFactor::Table { name, .. } if target_is(name, variable));
            if !local {
                return Err(side_effect("DELETE", 15));
            }
        }
        Statement::Merge(_) => return Err(side_effect("MERGE", 15)),
        Statement::Truncate(_) => return Err(side_effect("TRUNCATE TABLE", 15)),
        Statement::Print(_) => return Err(side_effect("PRINT", 14)),
        Statement::Throw(_) => return Err(side_effect("THROW", 14)),
        Statement::Commit { .. } => return Err(side_effect("COMMIT TRANSACTION", 15)),
        Statement::Rollback { .. } => return Err(side_effect("ROLLBACK TRANSACTION", 15)),
        Statement::Savepoint { .. } => return Err(side_effect("SAVE TRANSACTION", 15)),
        Statement::StartTransaction {
            has_end_keyword: false,
            ..
        } => return Err(side_effect("BEGIN TRANSACTION", 15)),
        Statement::Return(ReturnStatement { value }) => {
            let marker = matches!(
                value,
                Some(ReturnStatementValue::Expr(Expr::Value(ValueWithSpan { value: Value::Placeholder(p), .. }))) if p.starts_with("msduck:")
            );
            match (&definition.returns, value) {
                (Returns::Table { .. }, Some(_)) if !marker => {
                    return Err(error(
                        178,
                        1,
                        "A RETURN statement with a return value cannot be used in this context.",
                    ));
                }
                (Returns::Scalar(_), None) => {
                    return Err(error(
                        178,
                        1,
                        "A RETURN statement with a return value cannot be used in this context.",
                    ));
                }
                _ => {}
            }
        }
        _ => {}
    }
    if crate::raiserror::call(statement).is_some() {
        return Err(side_effect("RAISERROR", 14));
    }
    check_expressions(statement)?;
    // Nested statements.
    if let Some(statements) = super::fold::block(statement) {
        for nested in statements {
            check(nested, definition, variable)?;
        }
    }
    if let Some((body, handler)) = crate::preflight::try_catch_parts(statement) {
        for nested in body.iter().chain(handler) {
            check(nested, definition, variable)?;
        }
    }
    match statement {
        Statement::If(conditional) => {
            for nested in conditional.if_block.statements() {
                check(nested, definition, variable)?;
            }
            for block in &conditional.elseif_blocks {
                for nested in block.statements() {
                    check(nested, definition, variable)?;
                }
            }
            if let Some(block) = &conditional.else_block {
                for nested in block.statements() {
                    check(nested, definition, variable)?;
                }
            }
        }
        Statement::While(looped) => {
            for nested in looped.while_block.statements() {
                check(nested, definition, variable)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Built-in functions with side effects (443).
fn check_expressions(statement: &Statement) -> Result<()> {
    struct Find;
    impl Visitor for Find {
        type Break = String;
        fn pre_visit_statement(&mut self, statement: &Statement) -> ControlFlow<String> {
            // Nested statements are checked separately.
            let _ = statement;
            ControlFlow::Continue(())
        }
        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<String> {
            if let Expr::Function(function) = expr
                && function.name.0.len() == 1
            {
                let name = function.name.to_string();
                let lower = name.to_ascii_lowercase();
                if matches!(lower.as_str(), "newid" | "rand" | "newsequentialid") {
                    return ControlFlow::Break(lower);
                }
            }
            ControlFlow::Continue(())
        }
    }
    match statement.visit(&mut Find) {
        ControlFlow::Continue(()) => Ok(()),
        ControlFlow::Break(name) => Err(side_effect(&name, 1)),
    }
}

#[cfg(test)]
mod tests {
    use super::super::definition::parse;
    use super::*;

    fn number(sql: &str) -> Option<i32> {
        let definition = parse(sql).unwrap();
        validate(&definition)
            .err()
            .map(|e| e.downcast_ref::<SqlError>().unwrap().number)
    }

    #[test]
    fn compile_errors_match_sql_server() {
        let cases = [
            (
                "CREATE FUNCTION dbo.f(@x int) RETURNS int AS BEGIN RETURN @y END",
                Some(137),
            ),
            (
                "CREATE FUNCTION dbo.f(@x int) RETURNS int AS BEGIN SET @x = 1 END",
                Some(455),
            ),
            (
                "CREATE FUNCTION dbo.f(@x int) RETURNS int AS BEGIN SELECT 1; RETURN 1 END",
                Some(444),
            ),
            (
                "CREATE FUNCTION dbo.f(@x int) RETURNS int AS BEGIN PRINT 'x'; RETURN 1 END",
                Some(443),
            ),
            (
                "CREATE FUNCTION dbo.f(@x int) RETURNS int AS BEGIN IF @x > 0 RETURN 1 ELSE RETURN 2 END",
                Some(455),
            ),
            (
                "CREATE FUNCTION dbo.f(@x int) RETURNS int AS BEGIN IF @x > 0 RETURN 1; RETURN 2 END",
                None,
            ),
            (
                "CREATE FUNCTION dbo.f(@x int) RETURNS int AS BEGIN INSERT INTO t VALUES(1); RETURN 1 END",
                Some(443),
            ),
            (
                "CREATE FUNCTION dbo.f(@x int) RETURNS int AS BEGIN RETURN NEWID() END",
                Some(443),
            ),
            (
                "CREATE FUNCTION dbo.f(@x int) RETURNS @t TABLE(a int) AS BEGIN RETURN 1 END",
                Some(178),
            ),
            (
                "CREATE FUNCTION dbo.f(@x int) RETURNS @t TABLE(a int) AS BEGIN INSERT @t VALUES(@x) RETURN END",
                None,
            ),
            (
                "CREATE FUNCTION dbo.f(@x int) RETURNS int AS BEGIN DECLARE @r int; SELECT @r = COUNT(*) FROM t; RETURN @r END",
                None,
            ),
            (
                "CREATE FUNCTION dbo.f(@n int) RETURNS TABLE AS RETURN SELECT @m AS m",
                Some(137),
            ),
        ];
        for (sql, expected) in cases {
            assert_eq!(number(sql), expected, "{sql}");
        }
    }
}
