//! Replace calls of user-defined functions with their folded bodies.
//!
//! Scalar calls (`schema.name(...)` in any expression) become expressions and
//! table-valued calls in FROM become derived tables, which APPLY lowers to
//! lateral joins. Folded bodies are expanded again for the functions they
//! call, one nesting level deeper. SQL Server allows 32 levels; a call that
//! would exceed them becomes an expression that raises the nesting error if
//! it is ever evaluated. Loops and recursion cannot fold: the outermost call
//! then runs statement by statement when its arguments are known
//! (`interpret`), and its body's own calls expand again from there.
use super::{Parameter, Session, is_function, modules};
use anyhow::{Result, anyhow, bail};
use msduck_core::diagnostic::SqlError;
use msduck_sql::dialect::ext::functions::{Body, Definition, Returns, definition, fold, interpret};
use sqlparser::ast::*;
use std::{
    collections::HashMap,
    ops::ControlFlow,
    sync::{Arc, Mutex},
};

/// SQL Server's nesting limit for procedures, functions, triggers and views.
const NESTING_LIMIT: usize = 32;
/// Calls one statement may expand, counting nested and recursive calls.
const EXPANSION_LIMIT: usize = 4096;

static DEFINITIONS: Mutex<Option<HashMap<String, Arc<Definition>>>> = Mutex::new(None);

/// The parsed definition of stored module text.
fn parsed(text: &str) -> Result<Arc<Definition>> {
    if let Some(definition) = DEFINITIONS
        .lock()
        .unwrap()
        .get_or_insert_default()
        .get(text)
    {
        return Ok(definition.clone());
    }
    let definition = Arc::new(definition::parse(text)?);
    let mut cache = DEFINITIONS.lock().unwrap();
    let cache = cache.get_or_insert_default();
    if cache.len() >= 512 {
        cache.clear();
    }
    cache.insert(text.to_string(), definition.clone());
    Ok(definition)
}

/// Drop a definition that was altered or dropped from the cache.
pub(super) fn forget(text: &str) {
    if let Some(cache) = DEFINITIONS.lock().unwrap().as_mut() {
        cache.remove(text);
    }
}

pub(super) fn statement(
    session: &Session,
    statement: &mut Statement,
    parameters: &HashMap<String, Parameter>,
) -> Result<()> {
    let expansions = std::cell::Cell::new(0);
    let mut expander = Expander::new(session, parameters, &expansions);
    match VisitMut::visit(statement, &mut expander) {
        ControlFlow::Continue(()) => Ok(()),
        ControlFlow::Break(error) => Err(error),
    }
}

/// Pre-order rewrite of one expression of a scalar evaluation (SET, IF,
/// RETURN ...). Children are visited by the caller afterwards.
pub(super) fn expression(
    session: &Session,
    expr: &mut Expr,
    parameters: &HashMap<String, Parameter>,
) -> Result<()> {
    let expansions = std::cell::Cell::new(0);
    let mut expander = Expander::new(session, parameters, &expansions);
    match expr {
        Expr::Function(_) => {
            if let Some(expanded) = expander.scalar(expr)? {
                *expr = expanded;
            }
            Ok(())
        }
        Expr::Subquery(query)
        | Expr::Exists {
            subquery: query, ..
        }
        | Expr::InSubquery {
            subquery: query, ..
        } => match VisitMut::visit(query.as_mut(), &mut expander) {
            ControlFlow::Continue(()) => Ok(()),
            ControlFlow::Break(error) => Err(error),
        },
        _ => Ok(()),
    }
}

/// A function whose expansion reached itself again.
#[derive(Debug)]
struct Recursive(i32);

impl std::fmt::Display for Recursive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "recursive user-defined function {}", self.0)
    }
}

impl std::error::Error for Recursive {}

/// A function's stored module and parsed definition.
type Resolved = (modules::Module, Arc<Definition>);

struct Expander<'a> {
    session: &'a Session,
    /// The caller's variables, for arguments of interpreted calls.
    parameters: &'a HashMap<String, Parameter>,
    /// Nesting level of the calls being expanded, including interpreted
    /// calls that are running.
    depth: usize,
    /// Object ids of the functions being expanded, outermost first.
    path: Vec<i32>,
    expansions: &'a std::cell::Cell<usize>,
}

/// Name parts, upper level first.
fn parts(name: &ObjectName) -> Option<Vec<String>> {
    name.0
        .iter()
        .map(|part| part.as_ident().map(|ident| ident.value.clone()))
        .collect()
}

impl<'a> Expander<'a> {
    /// Resolve a function name to its stored module. `None` when no module
    /// matches; `schema_named` reports whether the name was schema-qualified
    /// with a schema of this database (so built-ins cannot be meant).
    fn resolve(&self, name: &ObjectName, table: bool) -> Result<(Option<Resolved>, bool)> {
        let Some(parts) = parts(name) else {
            return Ok((None, false));
        };
        let (schema, object) = match parts.as_slice() {
            [object] if table => (None, object.as_str()),
            [schema, object] => (Some(schema.as_str()), object.as_str()),
            [database, schema, object] => {
                let current = self.session.database();
                if !database.eq_ignore_ascii_case(&current.name)
                    && !database.eq_ignore_ascii_case(current.alias())
                {
                    return Ok((None, false));
                }
                (Some(schema.as_str()), object.as_str())
            }
            _ => return Ok((None, false)),
        };
        if schema.is_some_and(|schema| {
            schema.eq_ignore_ascii_case("sys") || schema.eq_ignore_ascii_case("INFORMATION_SCHEMA")
        }) {
            return Ok((None, false));
        }
        if modules::schema_id(&self.session.db, schema).is_err() {
            return Ok((None, false));
        }
        let named = schema.is_some();
        let Some(module) = modules::find(&self.session.db, schema, object)? else {
            return Ok((None, named));
        };
        if !is_function(&module.type_code) || (module.type_code == "FN") == table {
            return Ok((None, named));
        }
        let definition = parsed(&module.definition)?;
        Ok((Some((module, definition)), named))
    }

    fn new(
        session: &'a Session,
        parameters: &'a HashMap<String, Parameter>,
        expansions: &'a std::cell::Cell<usize>,
    ) -> Self {
        Expander {
            session,
            parameters,
            depth: session.ext.functions.depth.get(),
            path: Vec::new(),
            expansions,
        }
    }

    fn nested(&self, object_id: i32) -> Expander<'a> {
        let mut path = self.path.clone();
        path.push(object_id);
        Expander {
            session: self.session,
            parameters: self.parameters,
            depth: self.depth + 1,
            path,
            expansions: self.expansions,
        }
    }

    /// Fold failed for a reason interpretation may overcome: loops or
    /// recursion, in this function or in one it calls. Only the outermost
    /// call of an expansion interprets (its body then calls the others with
    /// known arguments), and only when its own arguments are known before
    /// the statement runs.
    fn fallback(&self, error: &anyhow::Error) -> bool {
        self.path.is_empty()
            && (error.downcast_ref::<fold::Unsupported>().is_some()
                || error.downcast_ref::<Recursive>().is_some())
    }

    /// Run a call with literal arguments and return its value or rows.
    fn interpret<T>(
        &self,
        display: &str,
        arguments: &[Expr],
        error: anyhow::Error,
        run: impl FnOnce(&mut super::run::Evaluation<'_>) -> Result<T>,
    ) -> Result<T> {
        if arguments.iter().any(fold::references_columns) {
            return Err(match error.downcast::<fold::Unsupported>() {
                Ok(limit) => anyhow!(
                    "unsupported: user-defined function {display} with column arguments: {}",
                    limit.0
                ),
                Err(_) => anyhow!(
                    "unsupported: recursive user-defined function {display} with column arguments"
                ),
            });
        }
        if self.depth >= NESTING_LIMIT {
            bail!(nesting());
        }
        let state = &self.session.ext.functions;
        let saved = state.depth.replace(self.depth + 1);
        let result = run(&mut super::run::Evaluation {
            session: self.session,
            parameters: self.parameters,
        });
        state.depth.set(saved);
        result.map_err(|error| match error.downcast::<fold::Unsupported>() {
            Ok(limit) => anyhow!("unsupported: user-defined function {display}: {}", limit.0),
            Err(error) => error,
        })
    }

    fn count(&self) -> Result<()> {
        let next = self.expansions.get() + 1;
        self.expansions.set(next);
        if next > EXPANSION_LIMIT {
            bail!(
                "unsupported: user-defined function calls in this statement expand beyond {EXPANSION_LIMIT} calls (deep recursion)"
            );
        }
        Ok(())
    }

    /// The expansion of a scalar user-defined function call.
    fn scalar(&self, expr: &Expr) -> Result<Option<Expr>> {
        let Expr::Function(function) = expr else {
            return Ok(None);
        };
        if function.name.0.len() < 2 {
            return Ok(None);
        }
        let FunctionArguments::List(list) = &function.args else {
            return Ok(None);
        };
        let display = function.name.to_string();
        let not_found = || {
            SqlError::new(
                4121,
                1,
                format!(
                    "Cannot find either column \"{}\" or the user-defined function or aggregate \"{display}\", or the name is ambiguous.",
                    function.name.0[0]
                ),
            )
        };
        let (found, named) = self.resolve(&function.name, false)?;
        let Some((module, definition)) = found else {
            if named && function.name.0.len() == 2 {
                bail!(not_found());
            }
            return Ok(None);
        };
        if function.over.is_some()
            || function.filter.is_some()
            || !function.within_group.is_empty()
            || list.duplicate_treatment.is_some()
            || !list.clauses.is_empty()
        {
            bail!(not_found());
        }
        let arguments = self.arguments(&list.args)?;
        let arguments = fold::bind_arguments(&definition, &display, &arguments)?;
        let definition = self.prepared(&definition)?;
        let Returns::Scalar(returns) = &definition.returns else {
            return Ok(None);
        };
        if self.path.contains(&module.object_id) {
            bail!(Recursive(module.object_id));
        }
        if self.depth >= NESTING_LIMIT {
            // A call evaluated with known arguments fails now; one inside
            // a folded body fails only if its branch is evaluated.
            if self.path.is_empty() && !arguments.iter().any(fold::references_columns) {
                bail!(nesting());
            }
            return Ok(Some(nesting_error(returns)));
        }
        self.count()?;
        let folded = (|| {
            let mut folded = fold::scalar(
                &definition,
                arguments.clone(),
                &self.session.ext.functions.alias(),
            )?;
            let mut nested = self.nested(module.object_id);
            if let ControlFlow::Break(error) = VisitMut::visit(&mut folded, &mut nested) {
                return Err(error);
            }
            Ok(folded)
        })();
        match folded {
            Ok(folded) => Ok(Some(folded)),
            Err(error) if self.fallback(&error) => self
                .interpret(&display, &arguments, error, |evaluation| {
                    interpret::scalar(&definition, arguments.clone(), evaluation)
                })
                .map(Some),
            Err(error) => Err(self.report(&display, error)),
        }
    }

    /// The definition with the session lowerings the engine applies before
    /// extension rewrites (database functions, database names and session
    /// functions) applied to its body. Session values become literals: the
    /// calling statement's variables cannot be extended from here.
    fn prepared(&self, definition: &Definition) -> Result<Definition> {
        let mut definition = definition.clone();
        match &mut definition.body {
            Body::Statements(statements) => {
                for statement in statements {
                    self.prepare(statement)?;
                }
            }
            Body::Query(query) => self.prepare(query.as_mut())?,
        }
        Ok(definition)
    }

    fn prepare<T: VisitMut + Visit>(&self, node: &mut T) -> Result<()> {
        self.session.lower_database_functions(node)?;
        self.session.qualify_databases(node)?;
        let empty = HashMap::new();
        let lowered = self.session.lower_session_functions_shared(node, &empty)?;
        if lowered.is_empty() {
            return Ok(());
        }
        let mut literals = HashMap::new();
        for (name, parameter) in lowered.iter() {
            let data_type = parameter.ast_type();
            let value = self.session.evaluate_scalar(
                Expr::Identifier(Ident::new(name.clone())),
                data_type.clone(),
                &lowered,
            )?;
            literals.insert(name.to_lowercase(), super::run::literal(value, &data_type)?);
        }
        struct Substitute(HashMap<String, Expr>);
        impl VisitorMut for Substitute {
            type Break = ();
            fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
                if let Expr::Identifier(id) = expr
                    && let Some(value) = self.0.get(&id.value.to_lowercase())
                {
                    *expr = value.clone();
                }
                ControlFlow::Continue(())
            }
        }
        let _ = VisitMut::visit(node, &mut Substitute(literals));
        Ok(())
    }

    /// Call arguments, with the calls they contain expanded at this level:
    /// they belong to the caller, not to the function's body.
    fn arguments(&self, args: &[FunctionArg]) -> Result<Vec<Expr>> {
        args.iter()
            .map(|arg| match arg {
                FunctionArg::Unnamed(FunctionArgExpr::Expr(expr)) => {
                    let mut expr = expr.clone();
                    let mut same = Expander {
                        session: self.session,
                        parameters: self.parameters,
                        depth: self.depth,
                        path: self.path.clone(),
                        expansions: self.expansions,
                    };
                    match VisitMut::visit(&mut expr, &mut same) {
                        ControlFlow::Continue(()) => Ok(expr),
                        ControlFlow::Break(error) => Err(error),
                    }
                }
                _ => bail!("unsupported user-defined function argument {arg}"),
            })
            .collect()
    }

    /// Errors that end the outermost expansion carry the function's name.
    fn report(&self, display: &str, error: anyhow::Error) -> anyhow::Error {
        if !self.path.is_empty() {
            return error;
        }
        match error.downcast::<fold::Unsupported>() {
            Ok(limit) => anyhow!("unsupported: user-defined function {display}: {}", limit.0),
            Err(error) => match error.downcast::<Recursive>() {
                Ok(_) => {
                    anyhow!("unsupported: recursive user-defined function called from {display}")
                }
                Err(error) => error,
            },
        }
    }

    /// The derived table of a table-valued function call in FROM.
    fn table(&self, factor: &TableFactor) -> Result<Option<TableFactor>> {
        let TableFactor::Table {
            name,
            alias,
            args: Some(args),
            ..
        } = factor
        else {
            return Ok(None);
        };
        let (found, named) = self.resolve(name, true)?;
        let Some((module, definition)) = found else {
            if named {
                bail!(SqlError::new(
                    208,
                    1,
                    format!("Invalid object name '{name}'.")
                ));
            }
            return Ok(None);
        };
        let display = name.to_string();
        let arguments = self.arguments(&args.args)?;
        let arguments = fold::bind_arguments(&definition, &display, &arguments)?;
        let definition = self.prepared(&definition)?;
        if self.path.contains(&module.object_id) {
            bail!(Recursive(module.object_id));
        }
        if self.depth >= NESTING_LIMIT {
            bail!(nesting());
        }
        self.count()?;
        let folded = (|| {
            let mut query = fold::table(
                &definition,
                arguments.clone(),
                &self.session.ext.functions.alias(),
            )?;
            let mut nested = self.nested(module.object_id);
            if let ControlFlow::Break(error) = VisitMut::visit(&mut query, &mut nested) {
                return Err(error);
            }
            Ok(query)
        })();
        let query = match folded {
            Ok(query) => query,
            Err(error)
                if self.fallback(&error) && matches!(definition.returns, Returns::Table { .. }) =>
            {
                self.interpret(&display, &arguments, error, |evaluation| {
                    interpret::table(&definition, arguments.clone(), evaluation)
                })?
            }
            Err(error) => return Err(self.report(&display, error)),
        };
        let alias = alias.clone().unwrap_or_else(|| TableAlias {
            explicit: true,
            name: Ident::new(module.name.clone()),
            columns: vec![],
            at: None,
        });
        Ok(Some(TableFactor::Derived {
            lateral: false,
            subquery: Box::new(query),
            alias: Some(alias),
            sample: None,
        }))
    }
}

fn nesting() -> SqlError {
    SqlError::new(
        217,
        1,
        "Maximum stored procedure, function, trigger, or view nesting level exceeded (limit 32).",
    )
}

/// An expression that raises SQL Server's nesting error when evaluated.
fn nesting_error(returns: &DataType) -> Expr {
    let message =
        "Maximum stored procedure, function, trigger, or view nesting level exceeded (limit 32).";
    Expr::Cast {
        kind: CastKind::Cast,
        expr: Box::new(Expr::Function(Function {
            name: ObjectName::from(vec![Ident::new("error")]),
            uses_odbc_syntax: false,
            parameters: FunctionArguments::None,
            args: FunctionArguments::List(FunctionArgumentList {
                duplicate_treatment: None,
                args: vec![FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(
                    Value::SingleQuotedString(message.into()).into(),
                )))],
                clauses: vec![],
            }),
            filter: None,
            null_treatment: None,
            over: None,
            within_group: vec![],
        })),
        data_type: returns.clone(),
        format: None,
    }
}

impl VisitorMut for Expander<'_> {
    type Break = anyhow::Error;

    fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<anyhow::Error> {
        match self.scalar(expr) {
            Ok(Some(expanded)) => *expr = expanded,
            Ok(None) => {}
            Err(error) => return ControlFlow::Break(error),
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_table_factor(&mut self, factor: &mut TableFactor) -> ControlFlow<anyhow::Error> {
        match self.table(factor) {
            Ok(Some(expanded)) => *factor = expanded,
            Ok(None) => {}
            Err(error) => return ControlFlow::Break(error),
        }
        ControlFlow::Continue(())
    }
}
