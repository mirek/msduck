//! Session adapter for SESSIONPROPERTY, SESSION_CONTEXT and
//! `sp_set_session_context`. Rules live in `msduck_sql::session_function`;
//! this module owns only the session's store and SET state, and binds their
//! values as typed, synthetic variables so SQL text never embeds them.
use super::{Parameter, ParameterValue, Session, SqlType};
use anyhow::{Result, bail};
use msduck_core::character::{CharacterType, Family, Length};
use msduck_sql::session_function::{
    self as rules, ContextValue, NameArgument, SessionContext, SessionOptions, VariantFunction,
};
use sqlparser::ast::*;
use std::{collections::HashMap, ops::ControlFlow};

const PREFIX: &str = "@__msduck_session_value_";

impl Session {
    /// msduck accepts only the login value of every option except
    /// ANSI_WARNINGS (other SET forms are refused), so this is the live state.
    pub(super) fn session_options(&self) -> SessionOptions {
        SessionOptions {
            ansi_warnings: self.ansi_warnings,
            ..SessionOptions::LOGIN
        }
    }

    /// Replace SESSIONPROPERTY and SESSION_CONTEXT calls with bound variables
    /// carrying the session's values. Their declared sql_variant results stay
    /// nullable, and a missing value is a typed NULL.
    pub(super) fn lower_session_functions<T: VisitMut>(
        &self,
        node: &mut T,
        parameters: &mut HashMap<String, Parameter>,
    ) -> Result<()> {
        let mut lower = Lower {
            options: self.session_options(),
            context: &self.session_context,
            parameters,
            next: 0,
        };
        match node.visit(&mut lower) {
            ControlFlow::Continue(()) => Ok(()),
            ControlFlow::Break(error) => Err(error),
        }
    }

    /// As `lower_session_functions`, for callers holding shared variables:
    /// copies them only when the node contains a session function.
    pub(super) fn lower_session_functions_shared<'p, T: VisitMut + Visit>(
        &self,
        node: &mut T,
        parameters: &'p HashMap<String, Parameter>,
    ) -> Result<std::borrow::Cow<'p, HashMap<String, Parameter>>> {
        if !contains_session_function(node) {
            return Ok(std::borrow::Cow::Borrowed(parameters));
        }
        let mut owned = parameters.clone();
        self.lower_session_functions(node, &mut owned)?;
        Ok(std::borrow::Cow::Owned(owned))
    }

    /// Execute `EXEC sp_set_session_context`. `Ok(None)` means the statement
    /// is some other procedure call.
    pub(super) fn set_session_context(
        &mut self,
        statement: &Statement,
        variables: &HashMap<String, Parameter>,
    ) -> Option<Result<()>> {
        let arguments = rules::set_call(statement)?;
        Some(arguments.and_then(|arguments| {
            let call = rules::bind(&arguments, variables)?;
            self.session_context
                .set(&call.key, call.value, call.read_only)
                .map_err(anyhow::Error::from)
        }))
    }

    /// RESETCONNECTION and new sessions start with an empty store.
    pub(super) fn empty_session_context() -> SessionContext {
        SessionContext::default()
    }
}

fn contains_session_function<T: Visit>(node: &T) -> bool {
    struct Find;
    impl Visitor for Find {
        type Break = ();
        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
            match expr {
                Expr::Function(f) if rules::variant_function(f).is_some() => ControlFlow::Break(()),
                _ => ControlFlow::Continue(()),
            }
        }
    }
    node.visit(&mut Find).is_break()
}

struct Lower<'a> {
    options: SessionOptions,
    context: &'a SessionContext,
    parameters: &'a mut HashMap<String, Parameter>,
    next: usize,
}
impl Lower<'_> {
    fn bind(&mut self, data_type: SqlType, value: ParameterValue) -> Expr {
        let name = format!("{PREFIX}{}", self.next);
        self.next += 1;
        self.parameters
            .insert(name.clone(), Parameter { value, data_type });
        Expr::Identifier(Ident::new(name))
    }
    fn variant(&mut self, data_type: SqlType, value: ParameterValue) -> Expr {
        Expr::Cast {
            kind: CastKind::Cast,
            expr: Box::new(self.bind(data_type, value)),
            data_type: rules::variant_type(),
            format: None,
        }
    }
    fn argument(&self, function: &Function) -> Result<NameArgument> {
        let FunctionArguments::List(list) = &function.args else {
            bail!("unsupported arguments in {function}");
        };
        let [FunctionArg::Unnamed(FunctionArgExpr::Expr(argument))] = list.args.as_slice() else {
            bail!("unsupported arguments in {function}");
        };
        if function.over.is_some() || function.filter.is_some() || !list.clauses.is_empty() {
            bail!("unsupported modifiers in {function}");
        }
        rules::name_argument(argument, self.parameters)
    }
    /// The function's current value: an option as INT 0/1, or a stored
    /// context value. `None` is NULL.
    fn current(&self, function: &Function) -> Result<Option<ContextValue>> {
        let argument = self.argument(function)?;
        Ok(match rules::variant_function(function) {
            Some(VariantFunction::SessionProperty) => match argument {
                NameArgument::Text {
                    text: Some(text), ..
                } => self
                    .options
                    .property(&text)
                    .map(|on| ContextValue::Int(on.into())),
                _ => None,
            },
            Some(VariantFunction::SessionContext) => {
                rules::context_key(&argument)?.and_then(|key| self.context.get(key).cloned())
            }
            None => unreachable!("only session functions are lowered"),
        }
        .filter(|value| *value != ContextValue::Null))
    }
    /// A value as a bound variable of its own base type.
    fn typed(&mut self, value: ContextValue) -> Result<Expr> {
        Ok(match value {
            ContextValue::Null => Expr::Value(sqlparser::ast::Value::Null.into()),
            ContextValue::Bit(v) => self.bind(SqlType::Bit, ParameterValue::Boolean(v)),
            ContextValue::TinyInt(v) => self.bind(SqlType::TinyInt, ParameterValue::UTinyInt(v)),
            ContextValue::SmallInt(v) => self.bind(SqlType::SmallInt, ParameterValue::SmallInt(v)),
            ContextValue::Int(v) => self.bind(SqlType::Int, ParameterValue::Int(v)),
            ContextValue::BigInt(v) => self.bind(SqlType::BigInt, ParameterValue::BigInt(v)),
            ContextValue::NVarChar { text, max_bytes } => {
                let declared = CharacterType::new(Family::Nvarchar, Length::Bounded(max_bytes / 2))
                    .map_err(|error| anyhow::anyhow!(error.to_string()))?;
                self.bind(SqlType::Character(declared), ParameterValue::Text(text))
            }
        })
    }
    /// An explicit conversion of a sql_variant converts from its base type, so
    /// `CAST(SESSIONPROPERTY(...) AS INT)` binds the INT value directly and a
    /// NULL stays an untyped NULL. The parser's integer-conversion marker is
    /// kept around the operand.
    fn lower(&mut self, expr: &mut Expr) -> Result<()> {
        let (operand, target) = match expr {
            Expr::Cast {
                expr, data_type, ..
            } => (expr, &*data_type),
            Expr::Convert {
                expr,
                data_type: Some(data_type),
                ..
            } => (expr, &*data_type),
            _ => return Ok(()),
        };
        if msduck_sql::variant_pack::is_variant(target) {
            return Ok(());
        }
        let operand: &mut Expr = if msduck_sql::variant_cast::source(operand).is_some() {
            let Expr::Function(marker) = operand.as_mut() else {
                unreachable!("the marker is a function")
            };
            let FunctionArguments::List(list) = &mut marker.args else {
                unreachable!("the marker has one argument")
            };
            let [FunctionArg::Unnamed(FunctionArgExpr::Expr(inner))] = list.args.as_mut_slice()
            else {
                unreachable!("the marker has one argument")
            };
            inner
        } else {
            operand
        };
        if let Expr::Function(f) = &*operand
            && rules::variant_function(f).is_some()
        {
            let value = self.current(f)?.unwrap_or(ContextValue::Null);
            *operand = self.typed(value)?;
        }
        Ok(())
    }
    /// Anywhere else the result is a sql_variant. Only integer base types
    /// have a sql_variant carrier; an nvarchar value there is refused.
    fn lower_call(&mut self, expr: &mut Expr) -> Result<()> {
        let Expr::Function(f) = expr else {
            return Ok(());
        };
        if rules::variant_function(f).is_none() {
            return Ok(());
        }
        let value = self.current(f)?;
        *expr = match value {
            None => self.variant(SqlType::Int, ParameterValue::Null),
            Some(ContextValue::NVarChar { .. }) => bail!(
                "unsupported SESSION_CONTEXT nvarchar value outside an explicit CAST or CONVERT"
            ),
            Some(value) => Expr::Cast {
                kind: CastKind::Cast,
                expr: Box::new(self.typed(value)?),
                data_type: rules::variant_type(),
                format: None,
            },
        };
        Ok(())
    }
}
impl VisitorMut for Lower<'_> {
    type Break = anyhow::Error;
    /// Session values are read when a statement runs; a persisted definition
    /// (view, default, routine, trigger) would freeze today's value instead.
    fn pre_visit_statement(&mut self, statement: &mut Statement) -> ControlFlow<anyhow::Error> {
        let persisted = matches!(
            statement,
            Statement::CreateView { .. }
                | Statement::AlterView { .. }
                | Statement::CreateTable(_)
                | Statement::AlterTable { .. }
                | Statement::CreateFunction(_)
                | Statement::CreateProcedure { .. }
                | Statement::CreateTrigger(_)
        );
        if persisted && contains_session_function(statement) {
            return ControlFlow::Break(anyhow::anyhow!(
                "unsupported SESSIONPROPERTY or SESSION_CONTEXT in a persisted definition"
            ));
        }
        ControlFlow::Continue(())
    }
    fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<anyhow::Error> {
        match self.lower(expr) {
            Ok(()) => ControlFlow::Continue(()),
            Err(error) => ControlFlow::Break(error),
        }
    }
    fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<anyhow::Error> {
        match self.lower_call(expr) {
            Ok(()) => ControlFlow::Continue(()),
            Err(error) => ControlFlow::Break(error),
        }
    }
}
