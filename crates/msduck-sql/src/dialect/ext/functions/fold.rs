//! Fold a function body into one T-SQL expression or query.
//!
//! Function bodies are straight-line code: DECLARE, SET, SELECT assignments,
//! IF/ELSE, BEGIN/END, RETURN and, for multi-statement table-valued functions,
//! INSERT into the return variable. Executing them symbolically, with every
//! variable bound to the expression that computes its current value, turns a
//! scalar body into a CASE expression and a table-valued body into a UNION ALL
//! of guarded row sources. The result is ordinary T-SQL, so calls inside
//! queries over many rows keep the engine's typing, checked arithmetic and
//! metadata rules, and the backend evaluates them set-wise.
//!
//! Arguments bind by substitution when that cannot change their meaning: when
//! they reference no columns, or when the body reads no tables. Otherwise they
//! are evaluated once in a derived table that the body reads, so the body's own
//! tables can never capture a column name of the caller.
use super::definition::{Body, Definition, Parameter, Returns};
use anyhow::{Result, anyhow, bail};
use msduck_core::diagnostic::SqlError;
use sqlparser::ast::*;
use std::{collections::HashMap, ops::ControlFlow};

/// A construct folding cannot express. The caller may evaluate the function
/// another way or report the limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsupported(pub String);

impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unsupported user-defined function body: {}", self.0)
    }
}

impl std::error::Error for Unsupported {}

pub(super) fn unsupported(what: impl Into<String>) -> anyhow::Error {
    Unsupported(what.into()).into()
}

/// Leaves (paths through IF/ELSE) one body may fold into.
const MAX_PATHS: usize = 4096;

/// Bind call arguments to parameters: `DEFAULT` selects the parameter's
/// default, and every parameter needs a value (SQL Server requires DEFAULT to
/// be explicit for functions).
pub fn bind_arguments(
    definition: &Definition,
    display: &str,
    arguments: &[Expr],
) -> Result<Vec<Expr>> {
    if arguments.len() > definition.parameters.len() {
        bail!(SqlError::new(
            8144,
            2,
            format!("Procedure or function {display} has too many arguments specified.")
        ));
    }
    if arguments.len() < definition.parameters.len() {
        bail!(SqlError::new(
            313,
            2,
            format!(
                "An insufficient number of arguments were supplied for the procedure or function {display}."
            )
        ));
    }
    definition
        .parameters
        .iter()
        .zip(arguments)
        .map(|(parameter, argument)| {
            if is_default_keyword(argument) {
                // A parameter without a default takes NULL.
                Ok(parameter.default.clone().unwrap_or_else(null))
            } else {
                Ok(argument.clone())
            }
        })
        .collect()
}

/// The `DEFAULT` keyword in an argument list.
pub fn is_default_keyword(expr: &Expr) -> bool {
    matches!(expr, Expr::Identifier(ident) if ident.quote_style.is_none() && ident.value.eq_ignore_ascii_case("DEFAULT"))
}

pub(super) fn cast(expr: Expr, data_type: &DataType) -> Expr {
    Expr::Cast {
        kind: CastKind::Cast,
        expr: Box::new(nested(expr)),
        data_type: data_type.clone(),
        format: None,
    }
}

fn nested(expr: Expr) -> Expr {
    match expr {
        Expr::Nested(_)
        | Expr::Identifier(_)
        | Expr::CompoundIdentifier(_)
        | Expr::Value(_)
        | Expr::Cast { .. }
        | Expr::Function(_)
        | Expr::Subquery(_) => expr,
        other => Expr::Nested(Box::new(other)),
    }
}

pub(super) fn null() -> Expr {
    Expr::Value(Value::Null.into())
}

fn ident(name: &str) -> Ident {
    Ident::new(name)
}

const AGGREGATES: &[&str] = &[
    "avg",
    "checksum_agg",
    "count",
    "count_big",
    "grouping",
    "grouping_id",
    "max",
    "min",
    "stdev",
    "stdevp",
    "string_agg",
    "sum",
    "var",
    "varp",
    "approx_count_distinct",
];

fn is_aggregate(function: &Function) -> bool {
    function.over.is_some()
        || (function.name.0.len() == 1
            && AGGREGATES
                .iter()
                .any(|name| function.name.to_string().eq_ignore_ascii_case(name)))
}

/// Whether the expression references a column (an identifier that is not a
/// variable) or a subquery.
fn has_columns(expr: &Expr) -> bool {
    struct Find;
    impl Visitor for Find {
        type Break = ();
        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
            match expr {
                Expr::Identifier(id) if !id.value.starts_with('@') && !is_default_keyword(expr) => {
                    ControlFlow::Break(())
                }
                Expr::CompoundIdentifier(_)
                | Expr::CompoundFieldAccess { .. }
                | Expr::Subquery(_)
                | Expr::Exists { .. }
                | Expr::InSubquery { .. } => ControlFlow::Break(()),
                _ => ControlFlow::Continue(()),
            }
        }
    }
    expr.visit(&mut Find).is_break()
}

/// Whether the expression aggregates rows or is a window function.
fn has_aggregate(expr: &Expr) -> bool {
    struct Find;
    impl Visitor for Find {
        type Break = ();
        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
            match expr {
                Expr::Function(function) if is_aggregate(function) => ControlFlow::Break(()),
                _ => ControlFlow::Continue(()),
            }
        }
    }
    expr.visit(&mut Find).is_break()
}

/// Whether the expression depends on the rows of the calling query:
/// columns, subqueries, aggregates or window functions.
pub fn references_columns(expr: &Expr) -> bool {
    has_columns(expr) || has_aggregate(expr)
}

/// Literals, variables, columns and conversions of them: cheap to repeat.
fn trivial(expr: &Expr) -> bool {
    match expr {
        Expr::Value(_) | Expr::Identifier(_) | Expr::CompoundIdentifier(_) => true,
        Expr::Nested(inner)
        | Expr::Cast { expr: inner, .. }
        | Expr::UnaryOp { expr: inner, .. } => trivial(inner),
        _ => false,
    }
}

/// Whether the tree reads tables or runs queries.
fn has_query<T: Visit>(node: &T) -> bool {
    struct Find;
    impl Visitor for Find {
        type Break = ();
        fn pre_visit_query(&mut self, _: &Query) -> ControlFlow<()> {
            ControlFlow::Break(())
        }
    }
    node.visit(&mut Find).is_break()
}

/// Functions whose repeated evaluation would change results.
fn volatile(expr: &Expr) -> bool {
    struct Find;
    impl Visitor for Find {
        type Break = ();
        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
            if let Expr::Function(function) = expr {
                let name = function.name.to_string().to_ascii_lowercase();
                if matches!(
                    name.as_str(),
                    "newid" | "rand" | "newsequentialid" | "crypt_gen_random"
                ) {
                    return ControlFlow::Break(());
                }
            }
            ControlFlow::Continue(())
        }
    }
    expr.visit(&mut Find).is_break()
}

/// Parse a T-SQL query template and plug expressions into its identifiers
/// and queries into its table names.
fn template(
    sql: &str,
    exprs: HashMap<String, Expr>,
    queries: HashMap<String, Query>,
) -> Result<Query> {
    let statements = crate::batch::parse(sql)?;
    let [Statement::Query(mut query)] =
        <[Statement; 1]>::try_from(statements).map_err(|_| anyhow!("invalid function template"))?
    else {
        bail!("invalid function template");
    };
    struct Plug {
        exprs: HashMap<String, Expr>,
        queries: HashMap<String, Query>,
    }
    impl VisitorMut for Plug {
        type Break = ();
        fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
            if let Expr::Identifier(id) = expr
                && let Some(value) = self.exprs.get(&id.value)
            {
                *expr = value.clone();
            }
            ControlFlow::Continue(())
        }
        fn pre_visit_table_factor(&mut self, factor: &mut TableFactor) -> ControlFlow<()> {
            if let TableFactor::Table { name, alias, .. } = factor
                && let Some(query) = self.queries.get(&name.to_string())
            {
                *factor = TableFactor::Derived {
                    lateral: false,
                    subquery: Box::new(query.clone()),
                    alias: alias.clone(),
                    sample: None,
                };
            }
            ControlFlow::Continue(())
        }
    }
    let _ = VisitMut::visit(query.as_mut(), &mut Plug { exprs, queries });
    Ok(*query)
}

/// Variable state during symbolic execution: name (lower case) to declared
/// type and current value.
#[derive(Clone, Default)]
pub(super) struct Env {
    pub(super) vars: HashMap<String, (DataType, Expr)>,
}

impl Env {
    pub(super) fn substitute(&self, mut expr: Expr) -> Result<Expr> {
        self.substitute_in(&mut expr)?;
        Ok(expr)
    }
    pub(super) fn substitute_in<T: VisitMut>(&self, node: &mut T) -> Result<()> {
        struct Substitute<'a>(&'a Env);
        impl VisitorMut for Substitute<'_> {
            type Break = anyhow::Error;
            fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<anyhow::Error> {
                if let Expr::Identifier(id) = expr
                    && id.value.starts_with('@')
                    && !id.value.starts_with("@@")
                {
                    match self.0.vars.get(&id.value.to_lowercase()) {
                        Some((_, value)) => *expr = nested(value.clone()),
                        None => {
                            return ControlFlow::Break(
                                SqlError::new(
                                    137,
                                    2,
                                    format!("Must declare the scalar variable \"{}\".", id.value),
                                )
                                .into(),
                            );
                        }
                    }
                }
                ControlFlow::Continue(())
            }
        }
        match node.visit(&mut Substitute(self)) {
            ControlFlow::Continue(()) => Ok(()),
            ControlFlow::Break(error) => Err(error),
        }
    }
    pub(super) fn declare(&mut self, name: &str, data_type: DataType, value: Expr) {
        self.vars.insert(name.to_lowercase(), (data_type, value));
    }
    fn assign(&mut self, name: &str, value: Expr) -> Result<()> {
        let (data_type, _) = self
            .vars
            .get(&name.to_lowercase())
            .ok_or_else(|| {
                anyhow::Error::from(SqlError::new(
                    137,
                    2,
                    format!("Must declare the scalar variable \"{name}\"."),
                ))
            })?
            .clone();
        let value = cast(value, &data_type);
        self.vars.insert(name.to_lowercase(), (data_type, value));
        Ok(())
    }
}

fn placeholder(index: usize) -> String {
    format!("__msduck_parameter_{index}")
}

/// Parameters bound to placeholders, and every local variable of the body
/// declared up front: a variable's scope is the whole body, whichever branch
/// declares it.
pub(super) fn parameter_env(parameters: &[Parameter], body: &[Statement]) -> Result<Env> {
    let mut env = Env::default();
    predeclare(body, &mut env)?;
    for (index, parameter) in parameters.iter().enumerate() {
        env.declare(
            &parameter.name,
            parameter.data_type.clone(),
            Expr::Identifier(ident(&placeholder(index))),
        );
    }
    Ok(env)
}

/// Declare every DECLAREd variable of `statements` (at any depth) as NULL.
pub(super) fn predeclare(statements: &[Statement], env: &mut Env) -> Result<()> {
    struct Find<'a> {
        env: &'a mut Env,
        error: Option<anyhow::Error>,
    }
    impl Visitor for Find<'_> {
        type Break = ();
        fn pre_visit_statement(&mut self, statement: &Statement) -> ControlFlow<()> {
            if let Statement::Declare { stmts } = statement {
                for declaration in stmts {
                    if declaration.declare_type.is_some() {
                        self.error = Some(unsupported("table variable or cursor declaration"));
                        return ControlFlow::Break(());
                    }
                    let Some(data_type) = declaration.data_type.clone() else {
                        continue;
                    };
                    let data_type = super::definition::declared_type(data_type);
                    for name in &declaration.names {
                        self.env
                            .declare(&name.value, data_type.clone(), cast(null(), &data_type));
                    }
                }
            }
            ControlFlow::Continue(())
        }
    }
    let mut find = Find { env, error: None };
    for statement in statements {
        let _ = statement.visit(&mut find);
    }
    match find.error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Table variables other than an INSERT target are not supported.
pub(super) fn check_table_variables(statements: &[Statement]) -> Result<()> {
    struct Find;
    impl Visitor for Find {
        type Break = ();
        fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<()> {
            match factor {
                TableFactor::Table { name, .. } if name.to_string().starts_with('@') => {
                    ControlFlow::Break(())
                }
                _ => ControlFlow::Continue(()),
            }
        }
    }
    for statement in statements {
        if statement.visit(&mut Find).is_break() {
            return Err(unsupported("reading a table variable"));
        }
    }
    Ok(())
}

/// Replace parameter placeholders with arguments. An argument is evaluated
/// once in a derived table named `alias` when substitution could change it:
/// a column the body's own tables could capture, or a volatile or costly
/// expression the folded body uses more than once. Aggregates and window
/// functions belong to the calling query and are always substituted.
/// Returns the derived table's columns, if any.
fn finish<T: VisitMut + Visit>(
    node: &mut T,
    parameters: &[Parameter],
    arguments: Vec<Expr>,
    reads_tables: bool,
    alias: &str,
) -> Vec<(String, Expr)> {
    struct Count(HashMap<String, usize>);
    impl Visitor for Count {
        type Break = ();
        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
            if let Expr::Identifier(id) = expr
                && id.value.starts_with("__msduck_parameter_")
            {
                *self.0.entry(id.value.clone()).or_default() += 1;
            }
            ControlFlow::Continue(())
        }
    }
    let mut count = Count(HashMap::new());
    let _ = Visit::visit(node, &mut count);
    let mut values = HashMap::new();
    let mut columns = Vec::new();
    for (index, (parameter, argument)) in parameters.iter().zip(arguments).enumerate() {
        let name = placeholder(index);
        let uses = count.0.get(&name).copied().unwrap_or(0);
        let derive = !has_aggregate(&argument)
            && ((reads_tables && has_columns(&argument))
                || (uses > 1 && (volatile(&argument) || !trivial(&argument))));
        let value = cast(argument, &parameter.data_type);
        let value = if derive {
            let column = format!("p{}", index + 1);
            columns.push((column.clone(), value));
            Expr::CompoundIdentifier(vec![ident(alias), ident(&column)])
        } else {
            nested(value)
        };
        values.insert(name, value);
    }
    struct Replace(HashMap<String, Expr>);
    impl VisitorMut for Replace {
        type Break = ();
        fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
            if let Expr::Identifier(id) = expr
                && let Some(value) = self.0.get(&id.value)
            {
                *expr = value.clone();
            }
            ControlFlow::Continue(())
        }
    }
    let _ = VisitMut::visit(node, &mut Replace(values));
    columns
}

/// How often each variable is referenced.
pub(super) fn variable_uses<T: Visit>(node: &T) -> HashMap<String, usize> {
    struct Count(HashMap<String, usize>);
    impl Visitor for Count {
        type Break = ();
        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
            if let Expr::Identifier(id) = expr
                && id.value.starts_with('@')
            {
                *self.0.entry(id.value.to_lowercase()).or_default() += 1;
            }
            ControlFlow::Continue(())
        }
    }
    let mut count = Count(HashMap::new());
    let _ = node.visit(&mut count);
    count.0
}

/// The derived table that evaluates arguments once.
fn arguments_query(columns: &[(String, Expr)]) -> Result<Query> {
    let mut exprs = HashMap::new();
    let mut items = Vec::new();
    for (index, (column, value)) in columns.iter().enumerate() {
        let slot = format!("__msduck_argument_{index}");
        items.push(format!("{slot} AS {column}"));
        exprs.insert(slot, value.clone());
    }
    template(
        &format!("SELECT {}", items.join(", ")),
        exprs,
        HashMap::new(),
    )
}

/// Fold a scalar function call. `alias` must be unique in the calling
/// statement; it names the derived table of arguments when one is needed.
pub fn scalar(definition: &Definition, arguments: Vec<Expr>, alias: &str) -> Result<Expr> {
    let (Returns::Scalar(returns), Body::Statements(body)) =
        (&definition.returns, &definition.body)
    else {
        bail!("not a scalar function");
    };
    check_table_variables(body)?;
    let env = parameter_env(&definition.parameters, body)?;
    let mut paths = 0;
    let work: Vec<&Statement> = body.iter().collect();
    let mut result = scalar_paths(&work, env, returns, &mut paths)?;
    if definition.options.returns_null_on_null_input && !definition.parameters.is_empty() {
        let condition = (0..definition.parameters.len())
            .map(|index| Expr::IsNull(Box::new(Expr::Identifier(ident(&placeholder(index))))))
            .reduce(|left, right| Expr::BinaryOp {
                left: Box::new(left),
                op: BinaryOperator::Or,
                right: Box::new(right),
            })
            .expect("parameters");
        result = cast(case(condition, cast(null(), returns), result), returns);
    }
    let columns = finish(
        &mut result,
        &definition.parameters,
        arguments,
        has_query(body),
        alias,
    );
    if columns.is_empty() {
        return Ok(result);
    }
    let query = template(
        &format!("SELECT __msduck_result FROM __msduck_arguments AS {alias}"),
        HashMap::from([("__msduck_result".to_string(), result)]),
        HashMap::from([("__msduck_arguments".to_string(), arguments_query(&columns)?)]),
    )?;
    Ok(Expr::Subquery(Box::new(query)))
}

use sqlparser::ast::helpers::attached_token::AttachedToken;

fn case(condition: Expr, then: Expr, otherwise: Expr) -> Expr {
    Expr::Case {
        case_token: AttachedToken::empty(),
        end_token: AttachedToken::empty(),
        operand: None,
        conditions: vec![CaseWhen {
            condition,
            result: then,
        }],
        else_result: Some(Box::new(otherwise)),
    }
}

/// Statements of a BEGIN ... END block (not a transaction or TRY).
pub(super) fn block(statement: &Statement) -> Option<&[Statement]> {
    match statement {
        Statement::StartTransaction {
            has_end_keyword: true,
            statements,
            exception: None,
            modifier: None,
            ..
        } => Some(statements),
        _ => None,
    }
}

/// The branches of an IF: condition, THEN statements, ELSE statements.
pub(super) fn branches(
    statement: &IfStatement,
) -> Result<(Expr, Vec<&Statement>, Vec<&Statement>)> {
    if !statement.elseif_blocks.is_empty() {
        return Err(unsupported("ELSEIF"));
    }
    let condition = statement
        .if_block
        .condition
        .clone()
        .ok_or_else(|| anyhow!("missing IF condition"))?;
    let then = statement.if_block.statements().iter().collect();
    let otherwise = statement
        .else_block
        .as_ref()
        .map(|block| block.statements().iter().collect())
        .unwrap_or_default();
    Ok((condition, then, otherwise))
}

/// Apply a straight-line statement (DECLARE, SET or a SELECT assignment) to
/// the environment. Returns false when the statement is something else.
pub(super) fn assign(statement: &Statement, env: &mut Env) -> Result<bool> {
    match statement {
        Statement::Declare { stmts } => {
            for declaration in stmts {
                if declaration.declare_type.is_some() {
                    return Err(unsupported("table variable or cursor declaration"));
                }
                let data_type = super::definition::declared_type(
                    declaration
                        .data_type
                        .clone()
                        .ok_or_else(|| anyhow!("missing variable type"))?,
                );
                let value = match &declaration.assignment {
                    // Not executable: the variable keeps its value.
                    None => {
                        for name in &declaration.names {
                            if !env.vars.contains_key(&name.value.to_lowercase()) {
                                env.declare(
                                    &name.value,
                                    data_type.clone(),
                                    cast(null(), &data_type),
                                );
                            }
                        }
                        continue;
                    }
                    Some(
                        DeclareAssignment::Expr(value)
                        | DeclareAssignment::Default(value)
                        | DeclareAssignment::MsSqlAssignment(value),
                    ) => cast(env.substitute(value.as_ref().clone())?, &data_type),
                    Some(_) => return Err(unsupported("declaration form")),
                };
                for name in &declaration.names {
                    env.declare(&name.value, data_type.clone(), value.clone());
                }
            }
            Ok(true)
        }
        Statement::Set(Set::SingleAssignment {
            variable, values, ..
        }) if variable.to_string().starts_with('@') => {
            let [value] = values.as_slice() else {
                return Err(unsupported("SET with several values"));
            };
            let value = env.substitute(value.clone())?;
            env.assign(&variable.to_string(), value)?;
            Ok(true)
        }
        Statement::Query(query) => {
            select_assignment(query, env)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// `SELECT @v = expr [, ...] [FROM ...]`. Without FROM the assignments run
/// in order. With FROM, a variable takes the value of the last row and keeps
/// its value when there are no rows.
fn select_assignment(query: &Query, env: &mut Env) -> Result<()> {
    let SetExpr::Select(select) = query.body.as_ref() else {
        return Err(unsupported("SELECT in a function body"));
    };
    let assignments: Vec<(&Ident, &Expr)> = select
        .projection
        .iter()
        .map(|item| match item {
            SelectItem::ExprWithAlias { expr, alias }
                if alias.quote_style.is_none() && alias.value.starts_with('@') =>
            {
                Ok((alias, expr))
            }
            _ => Err(anyhow::Error::from(SqlError::new(
                444,
                3,
                "Select statements included within a function cannot return data to a client.",
            ))),
        })
        .collect::<Result<_>>()?;
    if select.from.is_empty() && query.with.is_none() {
        for (variable, value) in assignments {
            if select.selection.is_some() {
                return Err(unsupported("SELECT assignment with WHERE and no FROM"));
            }
            let value = env.substitute(value.clone())?;
            env.assign(&variable.value, value)?;
        }
        return Ok(());
    }
    if query.limit_clause.is_some() || query.fetch.is_some() || select.top.is_some() {
        return Err(unsupported("SELECT assignment with TOP or OFFSET"));
    }
    let assigned: Vec<String> = assignments
        .iter()
        .map(|(variable, _)| variable.value.to_lowercase())
        .collect();
    let start = env.clone();
    for (variable, value) in assignments {
        if variable_uses(value)
            .keys()
            .any(|name| assigned.contains(name))
        {
            return Err(unsupported("accumulating SELECT assignment"));
        }
        let mut row = query.clone();
        let SetExpr::Select(row_select) = row.body.as_mut() else {
            unreachable!()
        };
        row_select.projection = vec![SelectItem::UnnamedExpr(value.clone())];
        // The last row in the requested order is the first in the reverse.
        if let Some(order) = &mut row.order_by
            && let OrderByKind::Expressions(expressions) = &mut order.kind
        {
            for expression in expressions {
                expression.options.sort = Some(match expression.options.sort {
                    None | Some(OrderBySort::Asc) => OrderBySort::Desc,
                    Some(OrderBySort::Desc) => OrderBySort::Asc,
                    Some(_) => return Err(unsupported("ORDER BY USING")),
                });
                expression.options.nulls_first = None;
            }
        }
        start.substitute_in(&mut row)?;
        let exists = Expr::Exists {
            subquery: Box::new(row.clone()),
            negated: false,
        };
        // TOP 1 of the reversed order is the last row in the requested
        // order. Without ORDER BY the row is unspecified; the capture shows
        // SQL Server keeping the first row of a heap scan here.
        let SetExpr::Select(row_select) = row.body.as_mut() else {
            unreachable!()
        };
        row_select.top = Some(Top {
            with_ties: false,
            percent: false,
            quantity: Some(TopQuantity::Constant(1)),
        });
        let current = env
            .vars
            .get(&variable.value.to_lowercase())
            .map(|(_, value)| value.clone())
            .ok_or_else(|| {
                anyhow::Error::from(SqlError::new(
                    137,
                    2,
                    format!("Must declare the scalar variable \"{}\".", variable.value),
                ))
            })?;
        env.assign(
            &variable.value,
            case(exists, Expr::Subquery(Box::new(row)), current),
        )?;
    }
    Ok(())
}

fn count_path(paths: &mut usize) -> Result<()> {
    *paths += 1;
    if *paths > MAX_PATHS {
        return Err(unsupported("too many IF/ELSE paths"));
    }
    Ok(())
}

fn scalar_paths(
    work: &[&Statement],
    mut env: Env,
    returns: &DataType,
    paths: &mut usize,
) -> Result<Expr> {
    for (index, statement) in work.iter().enumerate() {
        if assign(statement, &mut env)? {
            continue;
        }
        let rest = &work[index + 1..];
        if let Some(statements) = block(statement) {
            let spliced: Vec<&Statement> = statements.iter().chain(rest.iter().copied()).collect();
            return scalar_paths(&spliced, env, returns, paths);
        }
        match statement {
            Statement::If(statement) => {
                let (condition, then, otherwise) = branches(statement)?;
                let condition = env.substitute(condition)?;
                count_path(paths)?;
                let then: Vec<&Statement> = then.into_iter().chain(rest.iter().copied()).collect();
                let otherwise: Vec<&Statement> =
                    otherwise.into_iter().chain(rest.iter().copied()).collect();
                let then = scalar_paths(&then, env.clone(), returns, paths)?;
                let otherwise = scalar_paths(&otherwise, env, returns, paths)?;
                return Ok(case(condition, then, otherwise));
            }
            Statement::Return(ReturnStatement {
                value: Some(ReturnStatementValue::Expr(value)),
            }) if !matches!(value, Expr::Value(ValueWithSpan { value: Value::Placeholder(p), .. }) if p.starts_with("msduck:")) =>
            {
                return Ok(cast(env.substitute(value.clone())?, returns));
            }
            Statement::While(_) => return Err(unsupported("WHILE")),
            other => return Err(unsupported(statement_name(other))),
        }
    }
    Ok(cast(null(), returns))
}

pub(super) fn statement_name(statement: &Statement) -> String {
    let text = statement.to_string();
    text.split_whitespace()
        .next()
        .unwrap_or("statement")
        .to_uppercase()
}

/// Fold a table-valued function call into the query its rows come from.
pub fn table(definition: &Definition, arguments: Vec<Expr>, alias: &str) -> Result<Query> {
    let mut query = match (&definition.returns, &definition.body) {
        (Returns::Inline, Body::Query(query)) => {
            let env = parameter_env(&definition.parameters, &[])?;
            let mut query = query.as_ref().clone();
            env.substitute_in(&mut query)?;
            query
        }
        (Returns::Table { variable, columns }, Body::Statements(body)) => {
            check_table_variables(body)?;
            let env = parameter_env(&definition.parameters, body)?;
            let mut sources = Vec::new();
            let mut paths = 0;
            let work: Vec<&Statement> = body.iter().collect();
            table_paths(
                &work,
                env,
                None,
                variable,
                columns,
                &mut sources,
                &mut paths,
            )?;
            union(sources, columns)?
        }
        _ => bail!("not a table-valued function"),
    };
    let columns = finish(&mut query, &definition.parameters, arguments, true, alias);
    wrap_table(query, alias, &columns)
}

/// With derived arguments, read the body through APPLY from the arguments.
pub(super) fn wrap_table(query: Query, alias: &str, columns: &[(String, Expr)]) -> Result<Query> {
    if columns.is_empty() {
        return Ok(query);
    }
    template(
        &format!(
            "SELECT __msduck_rows.* FROM __msduck_arguments AS {alias} CROSS APPLY __msduck_body AS __msduck_rows"
        ),
        HashMap::new(),
        HashMap::from([
            ("__msduck_arguments".to_string(), arguments_query(columns)?),
            ("__msduck_body".to_string(), query),
        ]),
    )
}

/// UNION ALL of the guarded row sources, or no rows of the declared shape.
pub(super) fn union(sources: Vec<Query>, columns: &[ColumnDef]) -> Result<Query> {
    let mut sources = sources.into_iter();
    let Some(first) = sources.next() else {
        let mut exprs = HashMap::new();
        let mut items = Vec::new();
        for (index, column) in columns.iter().enumerate() {
            let slot = format!("__msduck_column_{index}");
            items.push(format!("{slot} AS {}", quoted(&column.name.value)));
            exprs.insert(slot, cast(null(), &column.data_type));
        }
        return template(
            &format!("SELECT {} WHERE 1 = 0", items.join(", ")),
            exprs,
            HashMap::new(),
        );
    };
    let mut body = SetExpr::Query(Box::new(first));
    for source in sources {
        body = SetExpr::SetOperation {
            op: SetOperator::Union,
            set_quantifier: SetQuantifier::All,
            left: Box::new(body),
            right: Box::new(SetExpr::Query(Box::new(source))),
        };
    }
    Ok(Query {
        with: None,
        body: Box::new(body),
        order_by: None,
        limit_clause: None,
        fetch: None,
        locks: vec![],
        for_clause: None,
        settings: None,
        format_clause: None,
        pipe_operators: vec![],
    })
}

fn quoted(name: &str) -> String {
    format!("[{}]", name.replace(']', "]]"))
}

pub(super) fn is_variable(name: &ObjectName, variable: &str) -> bool {
    name.0.len() == 1 && name.to_string().eq_ignore_ascii_case(variable)
}

#[allow(clippy::too_many_arguments)]
fn table_paths(
    work: &[&Statement],
    mut env: Env,
    guard: Option<Expr>,
    variable: &str,
    columns: &[ColumnDef],
    sources: &mut Vec<Query>,
    paths: &mut usize,
) -> Result<()> {
    for (index, statement) in work.iter().enumerate() {
        if assign(statement, &mut env)? {
            continue;
        }
        let rest = &work[index + 1..];
        if let Some(statements) = block(statement) {
            let spliced: Vec<&Statement> = statements.iter().chain(rest.iter().copied()).collect();
            return table_paths(&spliced, env, guard, variable, columns, sources, paths);
        }
        match statement {
            Statement::If(statement) => {
                let (condition, then, otherwise) = branches(statement)?;
                let condition = env.substitute(condition)?;
                count_path(paths)?;
                let taken = condition.clone();
                // NULL conditions take the ELSE branch.
                let not_taken = Expr::UnaryOp {
                    op: UnaryOperator::Not,
                    expr: Box::new(Expr::Nested(Box::new(case(
                        condition,
                        Expr::Value(Value::Boolean(true).into()),
                        Expr::Value(Value::Boolean(false).into()),
                    )))),
                };
                let and = |guard: &Option<Expr>, condition: Expr| match guard {
                    None => condition,
                    Some(guard) => Expr::BinaryOp {
                        left: Box::new(nested(guard.clone())),
                        op: BinaryOperator::And,
                        right: Box::new(nested(condition)),
                    },
                };
                let then: Vec<&Statement> = then.into_iter().chain(rest.iter().copied()).collect();
                let otherwise: Vec<&Statement> =
                    otherwise.into_iter().chain(rest.iter().copied()).collect();
                table_paths(
                    &then,
                    env.clone(),
                    Some(and(&guard, nested(taken))),
                    variable,
                    columns,
                    sources,
                    paths,
                )?;
                return table_paths(
                    &otherwise,
                    env,
                    Some(and(&guard, not_taken)),
                    variable,
                    columns,
                    sources,
                    paths,
                );
            }
            Statement::Insert(insert) => {
                let TableObject::TableName(name) = &insert.table else {
                    return Err(unsupported("INSERT target"));
                };
                if !is_variable(name, variable) {
                    return Err(unsupported(
                        "INSERT into a table other than the return variable",
                    ));
                }
                sources.push(insert_rows(insert, &env, guard.as_ref(), columns)?);
            }
            Statement::Return(ReturnStatement { value: None }) => return Ok(()),
            Statement::While(_) => return Err(unsupported("WHILE")),
            other => return Err(unsupported(statement_name(other))),
        }
    }
    Ok(())
}

/// The rows one INSERT adds to the return table, projected onto every
/// declared column.
pub(super) fn insert_rows(
    insert: &Insert,
    env: &Env,
    guard: Option<&Expr>,
    columns: &[ColumnDef],
) -> Result<Query> {
    if insert.output.is_some() || !insert.assignments.is_empty() {
        return Err(unsupported("INSERT form"));
    }
    let Some(source) = &insert.source else {
        return Err(unsupported("INSERT DEFAULT VALUES"));
    };
    let targets: Vec<usize> = if insert.columns.is_empty() {
        (0..columns.len()).collect()
    } else {
        insert
            .columns
            .iter()
            .map(|name| {
                let name = name.to_string();
                let name = name.trim_matches(|c| c == '[' || c == ']' || c == '"');
                columns
                    .iter()
                    .position(|column| column.name.value.eq_ignore_ascii_case(name))
                    .ok_or_else(|| {
                        SqlError::new(207, 1, format!("Invalid column name '{name}'.")).into()
                    })
            })
            .collect::<Result<_>>()?
    };
    let mut source = source.as_ref().clone();
    env.substitute_in(&mut source)?;
    // Name the source's columns so the projection can address them.
    let mut exprs = HashMap::new();
    let mut items = Vec::new();
    let names: Vec<String> = (0..targets.len())
        .map(|i| format!("__msduck_c{i}"))
        .collect();
    for (index, column) in columns.iter().enumerate() {
        let slot = format!("__msduck_column_{index}");
        let value = match targets.iter().position(|target| *target == index) {
            Some(position) => {
                Expr::CompoundIdentifier(vec![ident("__msduck_source"), ident(&names[position])])
            }
            None => default_value(column)?,
        };
        items.push(format!("{slot} AS {}", quoted(&column.name.value)));
        exprs.insert(slot, cast(value, &column.data_type));
    }
    let mut sql = format!(
        "SELECT {} FROM __msduck_source_rows AS __msduck_source({})",
        items.join(", "),
        names.join(", ")
    );
    if let Some(guard) = guard {
        sql.push_str(" WHERE __msduck_guard");
        exprs.insert("__msduck_guard".into(), nested(guard.clone()));
    }
    let width = source_width(&source);
    if width.is_some_and(|width| width != targets.len()) {
        bail!(SqlError::new(
            213,
            1,
            "Column name or number of supplied values does not match table definition."
        ));
    }
    template(
        &sql,
        exprs,
        HashMap::from([("__msduck_source_rows".to_string(), source)]),
    )
}

/// The number of columns a query produces, when it is evident.
fn source_width(query: &Query) -> Option<usize> {
    fn width(body: &SetExpr) -> Option<usize> {
        match body {
            SetExpr::Select(select) => {
                if select.projection.iter().any(|item| {
                    matches!(
                        item,
                        SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(..)
                    )
                }) {
                    None
                } else {
                    Some(select.projection.len())
                }
            }
            SetExpr::Values(values) => values.rows.first().map(|row| row.len()),
            SetExpr::Query(query) => width(&query.body),
            SetExpr::SetOperation { left, .. } => width(left),
            _ => None,
        }
    }
    width(&query.body)
}

fn default_value(column: &ColumnDef) -> Result<Expr> {
    for option in &column.options {
        match &option.option {
            ColumnOption::Default(value) => return Ok(value.clone()),
            ColumnOption::Identity(_) => {
                return Err(unsupported("IDENTITY column in a return table"));
            }
            _ => {}
        }
    }
    Ok(null())
}

#[cfg(test)]
mod tests {
    use super::super::definition::parse;
    use super::*;

    fn number(n: i64) -> Expr {
        Expr::Value(Value::Number(n.to_string(), false).into())
    }

    #[test]
    fn scalar_bodies_fold_into_case_expressions() {
        let definition = parse(
            "CREATE FUNCTION dbo.f(@x int) RETURNS int AS BEGIN \
             DECLARE @y int = @x * 2; IF @y > 10 RETURN @y; SET @y += 1; RETURN @y END",
        )
        .unwrap();
        let folded = scalar(&definition, vec![number(3)], "__a")
            .unwrap()
            .to_string();
        assert!(folded.starts_with("CASE WHEN"), "{folded}");
        assert!(!folded.contains('@'), "{folded}");
    }

    #[test]
    fn arguments_with_columns_are_derived_when_the_body_reads_tables() {
        let definition = parse(
            "CREATE FUNCTION dbo.f(@x int) RETURNS int AS BEGIN \
             RETURN (SELECT COUNT(*) FROM t WHERE id <= @x) END",
        )
        .unwrap();
        let column = Expr::Identifier(Ident::new("id"));
        let folded = scalar(&definition, vec![column], "__a1")
            .unwrap()
            .to_string();
        assert!(folded.contains("AS __a1"), "{folded}");
        let folded = scalar(&definition, vec![number(1)], "__a1")
            .unwrap()
            .to_string();
        assert!(!folded.contains("__a1"), "{folded}");
    }

    #[test]
    fn table_bodies_fold_into_unions() {
        let definition = parse(
            "CREATE FUNCTION dbo.mt(@n int) RETURNS @t TABLE(i int NOT NULL, s varchar(5) DEFAULT 'd') AS BEGIN \
             INSERT @t VALUES(@n, 'a'); IF @n > 1 INSERT INTO @t(i) SELECT @n + 1; RETURN; END",
        )
        .unwrap();
        let folded = table(&definition, vec![number(3)], "__a")
            .unwrap()
            .to_string();
        assert!(folded.contains("UNION ALL"), "{folded}");
        assert!(folded.contains("'d'"), "{folded}");
    }

    #[test]
    fn argument_counts_and_defaults() {
        let definition = parse(
            "CREATE FUNCTION dbo.f(@a int, @b int = 7) RETURNS int AS BEGIN RETURN @a + @b END",
        )
        .unwrap();
        let error = bind_arguments(&definition, "dbo.f", &[number(1)]).unwrap_err();
        assert_eq!(error.downcast_ref::<SqlError>().unwrap().number, 313);
        let error =
            bind_arguments(&definition, "dbo.f", &[number(1), number(2), number(3)]).unwrap_err();
        assert_eq!(error.downcast_ref::<SqlError>().unwrap().number, 8144);
        let bound = bind_arguments(
            &definition,
            "dbo.f",
            &[number(1), Expr::Identifier(Ident::new("DEFAULT"))],
        )
        .unwrap();
        assert_eq!(bound[1], number(7));
    }

    #[test]
    fn loops_are_reported_as_unsupported() {
        let definition = parse(
            "CREATE FUNCTION dbo.f(@x int) RETURNS int AS BEGIN DECLARE @i int = 0; \
             WHILE @i < @x SET @i += 1; RETURN @i END",
        )
        .unwrap();
        let error = scalar(&definition, vec![number(3)], "__a").unwrap_err();
        assert!(error.downcast_ref::<Unsupported>().is_some());
    }
}
