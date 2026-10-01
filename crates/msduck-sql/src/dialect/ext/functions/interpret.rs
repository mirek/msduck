//! Run a function body statement by statement for one set of argument
//! values.
//!
//! Folding cannot express loops or recursion. When every argument of a call
//! is known before the calling statement runs (constants and variables, no
//! columns), the body runs here instead: conditions are evaluated before a
//! branch is chosen, and every assigned variable is reduced to a literal
//! value, so WHILE loops and recursion guarded by IF terminate as on SQL
//! Server. Evaluation itself belongs to the caller through [`Evaluator`].
use super::{
    definition::{Body, Definition, Returns},
    fold::{self, Env},
};
use anyhow::{Result, anyhow, bail};
use msduck_core::diagnostic::SqlError;
use sqlparser::ast::*;

/// Evaluation of column-free expressions for the interpreter.
pub trait Evaluator {
    /// Evaluate `expr` converted to `data_type` and return it as a literal
    /// expression of that type.
    fn value(&mut self, expr: Expr, data_type: &DataType) -> Result<Expr>;
    /// Evaluate a predicate; NULL is false.
    fn truth(&mut self, expr: Expr) -> Result<bool>;
}

/// Statements one call may execute, including loop iterations.
const STEP_LIMIT: usize = 1_000_000;

enum Flow {
    Normal,
    Break,
    Continue,
    Return(Option<Box<Expr>>),
}

struct Run<'a> {
    evaluator: &'a mut dyn Evaluator,
    steps: usize,
    returns: Option<&'a DataType>,
    table: Option<(&'a str, &'a [ColumnDef])>,
    sources: Vec<Query>,
}

fn marker(statement: &Statement) -> Option<&str> {
    match statement {
        Statement::Return(ReturnStatement {
            value:
                Some(ReturnStatementValue::Expr(Expr::Value(ValueWithSpan {
                    value: Value::Placeholder(marker),
                    ..
                }))),
        }) => Some(marker.as_str()),
        _ => None,
    }
}

/// Variables a straight-line statement assigns.
fn assigned(statement: &Statement) -> Vec<String> {
    match statement {
        Statement::Declare { stmts } => stmts
            .iter()
            .flat_map(|declaration| {
                declaration
                    .names
                    .iter()
                    .map(|name| name.value.to_lowercase())
            })
            .collect(),
        Statement::Set(Set::SingleAssignment { variable, .. }) => {
            vec![variable.to_string().to_lowercase()]
        }
        Statement::Query(query) => match query.body.as_ref() {
            SetExpr::Select(select) => select
                .projection
                .iter()
                .filter_map(|item| match item {
                    SelectItem::ExprWithAlias { alias, .. } => Some(alias.value.to_lowercase()),
                    _ => None,
                })
                .collect(),
            _ => vec![],
        },
        _ => vec![],
    }
}

impl Run<'_> {
    fn step(&mut self) -> Result<()> {
        self.steps += 1;
        if self.steps > STEP_LIMIT {
            bail!(
                "unsupported: user-defined function call exceeds {STEP_LIMIT} executed statements"
            );
        }
        Ok(())
    }

    /// Reduce variables to literal values after an assignment.
    fn collapse(&mut self, env: &mut Env, names: &[String]) -> Result<()> {
        for name in names {
            let Some((data_type, value)) = env.vars.get(name).cloned() else {
                continue;
            };
            let value = self.evaluator.value(value, &data_type)?;
            env.vars.insert(name.clone(), (data_type, value));
        }
        Ok(())
    }

    fn run(&mut self, statements: &[Statement], env: &mut Env) -> Result<Flow> {
        for statement in statements {
            self.step()?;
            match marker(statement) {
                Some("msduck:break") => return Ok(Flow::Break),
                Some("msduck:continue") => return Ok(Flow::Continue),
                _ => {}
            }
            if fold::assign(statement, env)? {
                self.collapse(env, &assigned(statement))?;
                continue;
            }
            if let Some(block) = fold::block(statement) {
                match self.run(block, env)? {
                    Flow::Normal => continue,
                    flow => return Ok(flow),
                }
            }
            match statement {
                Statement::If(conditional) => {
                    let (condition, then, otherwise) = fold::branches(conditional)?;
                    let condition = env.substitute(condition)?;
                    let branch = if self.evaluator.truth(condition)? {
                        then
                    } else {
                        otherwise
                    };
                    let branch: Vec<Statement> = branch.into_iter().cloned().collect();
                    match self.run(&branch, env)? {
                        Flow::Normal => {}
                        flow => return Ok(flow),
                    }
                }
                Statement::While(looped) => {
                    let condition = looped
                        .while_block
                        .condition
                        .clone()
                        .ok_or_else(|| anyhow!("missing WHILE condition"))?;
                    loop {
                        self.step()?;
                        if !self.evaluator.truth(env.substitute(condition.clone())?)? {
                            break;
                        }
                        match self.run(looped.while_block.statements(), env)? {
                            Flow::Normal | Flow::Continue => {}
                            Flow::Break => break,
                            flow @ Flow::Return(_) => return Ok(flow),
                        }
                    }
                }
                Statement::Return(ReturnStatement { value }) => {
                    let value = match (value, self.returns) {
                        (Some(ReturnStatementValue::Expr(value)), Some(returns)) => {
                            let value = env.substitute(value.clone())?;
                            Some(self.evaluator.value(value, returns)?)
                        }
                        _ => None,
                    };
                    return Ok(Flow::Return(value.map(Box::new)));
                }
                Statement::Insert(insert) => {
                    let Some((variable, columns)) = self.table else {
                        return Err(fold::unsupported("INSERT"));
                    };
                    let TableObject::TableName(name) = &insert.table else {
                        return Err(fold::unsupported("INSERT target"));
                    };
                    if !fold::is_variable(name, variable) {
                        return Err(fold::unsupported(
                            "INSERT into a table other than the return variable",
                        ));
                    }
                    self.sources
                        .push(fold::insert_rows(insert, env, None, columns)?);
                }
                other => return Err(fold::unsupported(fold::statement_name(other))),
            }
        }
        Ok(Flow::Normal)
    }
}

/// Literal argument values bound to parameters, and every local variable
/// declared up front as NULL (its scope is the whole body).
fn parameters(
    definition: &Definition,
    body: &[Statement],
    arguments: Vec<Expr>,
    evaluator: &mut dyn Evaluator,
) -> Result<Env> {
    fold::check_table_variables(body)?;
    let mut env = Env::default();
    fold::predeclare(body, &mut env)?;
    for (parameter, argument) in definition.parameters.iter().zip(arguments) {
        let value = evaluator.value(argument, &parameter.data_type)?;
        env.declare(&parameter.name, parameter.data_type.clone(), value);
    }
    Ok(env)
}

fn any_null(definition: &Definition, env: &Env) -> bool {
    definition.parameters.iter().any(|parameter| {
        matches!(
            env.vars.get(&parameter.name.to_lowercase()),
            Some((_, Expr::Cast { expr, .. })) if matches!(expr.as_ref(), Expr::Value(ValueWithSpan { value: Value::Null, .. }))
        ) || matches!(
            env.vars.get(&parameter.name.to_lowercase()),
            Some((_, Expr::Value(ValueWithSpan { value: Value::Null, .. })))
        )
    })
}

/// Run a scalar function; the result is a literal of the return type.
pub fn scalar(
    definition: &Definition,
    arguments: Vec<Expr>,
    evaluator: &mut dyn Evaluator,
) -> Result<Expr> {
    let (Returns::Scalar(returns), Body::Statements(body)) =
        (&definition.returns, &definition.body)
    else {
        bail!("not a scalar function");
    };
    let mut env = parameters(definition, body, arguments, evaluator)?;
    if definition.options.returns_null_on_null_input && any_null(definition, &env) {
        return evaluator.value(fold::null(), returns);
    }
    let mut run = Run {
        evaluator,
        steps: 0,
        returns: Some(returns),
        table: None,
        sources: vec![],
    };
    match run.run(body, &mut env)? {
        Flow::Return(Some(value)) => Ok(*value),
        Flow::Return(None) | Flow::Normal => run.evaluator.value(fold::null(), returns),
        Flow::Break | Flow::Continue => Err(SqlError::new(
            135,
            1,
            "Cannot use a BREAK statement outside the scope of a WHILE statement.",
        )
        .into()),
    }
}

/// Run a multi-statement table-valued function; the result is the query of
/// the rows it inserted, with literal values.
pub fn table(
    definition: &Definition,
    arguments: Vec<Expr>,
    evaluator: &mut dyn Evaluator,
) -> Result<Query> {
    let (Returns::Table { variable, columns }, Body::Statements(body)) =
        (&definition.returns, &definition.body)
    else {
        bail!("not a multi-statement table-valued function");
    };
    let mut env = parameters(definition, body, arguments, evaluator)?;
    let mut run = Run {
        evaluator,
        steps: 0,
        returns: None,
        table: Some((variable, columns)),
        sources: vec![],
    };
    run.run(body, &mut env)?;
    fold::union(run.sources, columns)
}

#[cfg(test)]
mod tests {
    use super::super::definition::parse;
    use super::*;

    /// Integer-only evaluation for tests: literals, + - * and comparisons.
    struct Arithmetic;
    fn eval(expr: &Expr) -> Option<i64> {
        match expr {
            Expr::Value(ValueWithSpan {
                value: Value::Number(n, _),
                ..
            }) => n.parse().ok(),
            Expr::Value(ValueWithSpan {
                value: Value::Null, ..
            }) => None,
            Expr::Nested(e) => eval(e),
            Expr::Cast { expr, .. } => eval(expr),
            Expr::BinaryOp { left, op, right } => {
                let (l, r) = (eval(left)?, eval(right)?);
                Some(match op {
                    BinaryOperator::Plus => l + r,
                    BinaryOperator::Minus => l - r,
                    BinaryOperator::Multiply => l * r,
                    BinaryOperator::Lt => i64::from(l < r),
                    BinaryOperator::LtEq => i64::from(l <= r),
                    BinaryOperator::Gt => i64::from(l > r),
                    _ => return None,
                })
            }
            _ => None,
        }
    }
    impl Evaluator for Arithmetic {
        fn value(&mut self, expr: Expr, _: &DataType) -> Result<Expr> {
            Ok(match eval(&expr) {
                Some(n) => Expr::Value(Value::Number(n.to_string(), false).into()),
                None => Expr::Value(Value::Null.into()),
            })
        }
        fn truth(&mut self, expr: Expr) -> Result<bool> {
            Ok(eval(&expr) == Some(1))
        }
    }

    fn number(n: i64) -> Expr {
        Expr::Value(Value::Number(n.to_string(), false).into())
    }

    #[test]
    fn loops_run_with_break_and_continue() {
        let definition = parse(
            "CREATE FUNCTION dbo.f(@x int) RETURNS int AS BEGIN DECLARE @i int = 0, @s int = 0; \
             WHILE @i < @x BEGIN SET @i += 1; IF @i > 4 BREAK; SET @s = @s + @i; END; RETURN @s END",
        )
        .unwrap();
        assert_eq!(
            scalar(&definition, vec![number(3)], &mut Arithmetic).unwrap(),
            number(6)
        );
        assert_eq!(
            scalar(&definition, vec![number(9)], &mut Arithmetic).unwrap(),
            number(10)
        );
    }

    #[test]
    fn table_functions_collect_inserted_rows() {
        let definition = parse(
            "CREATE FUNCTION dbo.f(@n int) RETURNS @t TABLE (i int) AS BEGIN DECLARE @i int = 0; \
             WHILE @i < @n BEGIN SET @i = @i + 1; INSERT @t VALUES (@i) END RETURN END",
        )
        .unwrap();
        let query = table(&definition, vec![number(3)], &mut Arithmetic)
            .unwrap()
            .to_string();
        assert_eq!(query.matches("UNION ALL").count(), 2, "{query}");
    }
}
