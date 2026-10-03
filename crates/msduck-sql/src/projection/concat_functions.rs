//! Explicit scoped compile binding. No source operand is evaluated or rewritten.
use super::*;
use crate::{concat_conversion as conversion, concat_ws as function, temporal_guid_text::Language};
use msduck_core::{
    character::{CharacterType, Family, Length},
    result::Properties,
    types::Type,
};
use std::num::NonZeroUsize;

pub struct Context<'a> {
    pub collations: &'a [function::Collation],
    pub language: Language,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    UnsupportedSyntax,
    UnknownOperand,
    UnknownContext,
    PlanMismatch,
    Function(conversion::Error),
}
/// Borrows the one original AST. Declarations and conversion rules are frozen;
/// later physical lowering acquires each operand once and uses the conversion plan.
pub struct Plan<'a> {
    original: &'a Expr,
    operands: Vec<&'a Expr>,
    declarations: Vec<conversion::Declaration>,
    conversion: conversion::Plan,
    info: Info,
    properties: Properties,
}
impl<'a> Plan<'a> {
    pub fn original(&self) -> &'a Expr {
        self.original
    }
    pub fn operands(&self) -> &[&'a Expr] {
        &self.operands
    }
    pub fn declarations(&self) -> &[conversion::Declaration] {
        &self.declarations
    }
    pub fn conversion(&self) -> &conversion::Plan {
        &self.conversion
    }
    pub fn info(&self) -> &Info {
        &self.info
    }
    pub fn properties(&self) -> Properties {
        self.properties
    }
    pub fn result_type(&self) -> crate::result_types::ResultType {
        crate::result_types::character_declaration(self.conversion.result().declaration)
    }
    /// Refuse applying a saved binding to a changed expression. No annotation
    /// names or process-global IDs are introduced into the AST.
    pub fn validate(&self, expression: &Expr) -> Result<(), Error> {
        if self.original == expression {
            Ok(())
        } else {
            Err(Error::PlanMismatch)
        }
    }
}
fn call(expr: &Expr) -> Result<Option<(function::Function, Vec<&Expr>)>, Error> {
    let mut expr = expr;
    let mut depth = 0;
    while let Expr::Nested(inner) = expr {
        depth += 1;
        if depth >= MAX_SCOPE_DEPTH {
            return Err(Error::UnsupportedSyntax);
        }
        expr = inner;
    }
    let Expr::Function(f) = expr else {
        return Ok(None);
    };
    let operation = match f.name.to_string().to_ascii_lowercase().as_str() {
        "concat_ws" => function::Function::ConcatWs,
        "translate" => function::Function::Translate,
        _ => return Ok(None),
    };
    let FunctionArguments::List(args) = &f.args else {
        return Err(Error::UnsupportedSyntax);
    };
    function::validate_arity(operation, args.args.len())
        .map_err(|e| Error::Function(conversion::Error::Function(e)))?;
    if !matches!(f.parameters, FunctionArguments::None)
        || f.over.is_some()
        || f.filter.is_some()
        || f.null_treatment.is_some()
        || !f.within_group.is_empty()
        || args.duplicate_treatment.is_some()
        || !args.clauses.is_empty()
    {
        return Err(Error::UnsupportedSyntax);
    }
    let operands = args
        .args
        .iter()
        .map(|a| match a {
            FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => Ok(e),
            _ => Err(Error::UnsupportedSyntax),
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some((operation, operands)))
}
fn declaration(
    catalog: &CatalogSnapshot,
    expr: &Expr,
    sources: &[Source],
    scope: &Scope,
    context: &Context<'_>,
    column: NonZeroUsize,
    depth: usize,
) -> Result<conversion::Declaration, Error> {
    if depth >= MAX_SCOPE_DEPTH {
        return Err(Error::UnsupportedSyntax);
    }
    if let Expr::Nested(inner) = expr {
        return declaration(catalog, inner, sources, scope, context, column, depth + 1);
    }
    if conditional::literal_null(expr) {
        return Ok(conversion::Declaration::null_literal());
    }
    if let Expr::Collate {
        expr: inner,
        collation,
    } = expr
    {
        let mut declaration =
            declaration(catalog, inner, sources, scope, context, column, depth + 1)?;
        if !matches!(
            declaration.source,
            Some(Type::Character(_) | Type::Text | Type::Ntext)
        ) {
            return Err(Error::UnknownContext);
        }
        declaration.collation = Some(declaration.collation.map_or_else(
            || msduck_core::collation::Label::Explicit(collation.to_string()),
            |label| label.collate(collation.to_string()),
        ));
        return Ok(declaration);
    }
    if let Some(plan) = bind_local(catalog, expr, sources, scope, context, column, depth + 1)? {
        return Ok(conversion::Declaration {
            source: Some(Type::Character(plan.conversion.result().declaration)),
            collation: Some(plan.conversion.result().collation.clone()),
            style: None,
        });
    }
    if let Expr::Subquery(query) = expr {
        let mut input = scope.clone();
        input.rows.push(Some(sources.to_vec()));
        let fields = fields_in(catalog, query, &input, context, depth + 1)?;
        let [field] = fields.as_slice() else {
            return Err(Error::UnknownOperand);
        };
        let source = field
            .info
            .as_ref()
            .and_then(|i| i.logical_type())
            .ok_or(Error::UnknownOperand)?;
        let collation = field
            .collation
            .clone()
            .map(|label| label.map_err(|_| Error::UnknownContext))
            .transpose()?;
        return Ok(conversion::Declaration {
            source: Some(source),
            collation,
            style: None,
        });
    }
    // Empty literals allocate one unit here; typed NULL retains its full type.
    let source = if let Expr::Value(value) = expr {
        match &value.value {
            Value::SingleQuotedString(s) | Value::NationalStringLiteral(s) => {
                let family = if matches!(value.value, Value::NationalStringLiteral(_)) {
                    Family::Nvarchar
                } else {
                    Family::Varchar
                };
                // Literal allocation is grounded under the retained CP1252
                // default. A Unicode companion must not hide unknown native
                // byte widths of ANSI literals in another source code page.
                if family == Family::Varchar && !s.is_ascii() {
                    let encoding = catalog.default_collation.as_deref().and_then(|name| {
                        let mut matches = context
                            .collations
                            .iter()
                            .filter(|c| c.name.eq_ignore_ascii_case(name));
                        let first = matches.next()?;
                        matches.next().is_none().then_some(first.encoding)
                    });
                    if encoding != Some(function::Encoding::Cp1252) {
                        return Err(Error::UnknownContext);
                    }
                }
                if s.is_empty() {
                    Type::Character(
                        CharacterType::new(family, Length::Bounded(1))
                            .map_err(|_| Error::UnknownOperand)?,
                    )
                } else {
                    let kind = crate::expression_metadata::storage::kind(
                        expr,
                        &Default::default(),
                        &|_| None,
                    )
                    .ok_or(Error::UnknownOperand)?;
                    crate::sql_type::declaration(&kind).map_err(|_| Error::UnknownOperand)?
                }
            }
            _ => member_expression(catalog, expr, sources, scope)
                .and_then(|i| i.logical_type())
                .ok_or(Error::UnknownOperand)?,
        }
    } else {
        member_expression(catalog, expr, sources, scope)
            .and_then(|i| i.logical_type())
            .ok_or(Error::UnknownOperand)?
    };
    let collation = if matches!(source, Type::Character(_) | Type::Text | Type::Ntext) {
        let label = expression_collation(catalog, expr, sources, scope).or_else(|| {
            if !matches!(source, Type::Text | Type::Ntext) {
                return None;
            }
            let inner = match expr {
                Expr::Cast { expr, .. } | Expr::Convert { expr, .. } => expr.as_ref(),
                _ => return None,
            };
            expression_collation(catalog, inner, sources, scope).or_else(|| {
                (conditional::literal_null(inner)
                    || member_expression(catalog, inner, sources, scope)
                        .and_then(|i| i.logical_type())
                        .is_some_and(|kind| {
                            !matches!(kind, Type::Character(_) | Type::Text | Type::Ntext)
                        }))
                .then(|| {
                    catalog
                        .default_collation
                        .clone()
                        .map(msduck_core::collation::Label::CoercibleDefault)
                })
                .flatten()
                .map(Ok)
            })
        });
        label
            .map(|label| {
                label.map_err(|e| match e {
                    msduck_core::collation::Conflict::Operation(e) => {
                        Error::Function(conversion::Error::Function(function::Error::Sql(e)))
                    }
                    _ => Error::UnknownContext,
                })
            })
            .transpose()?
    } else {
        None
    };
    Ok(conversion::Declaration {
        source: Some(source),
        collation,
        style: None,
    })
}
fn bind_local<'a>(
    catalog: &CatalogSnapshot,
    expression: &'a Expr,
    sources: &[Source],
    scope: &Scope,
    context: &Context<'_>,
    column: NonZeroUsize,
    depth: usize,
) -> Result<Option<Plan<'a>>, Error> {
    if depth >= MAX_SCOPE_DEPTH {
        return Err(Error::UnsupportedSyntax);
    }
    let Some((operation, operands)) = call(expression)? else {
        return Ok(None);
    };
    let declarations = operands
        .iter()
        .map(|e| declaration(catalog, e, sources, scope, context, column, depth + 1))
        .collect::<Result<Vec<_>, _>>()?;
    let conversion = conversion::plan(
        operation,
        &declarations,
        catalog
            .default_collation
            .as_deref()
            .ok_or(Error::UnknownContext)?,
        context.collations,
        context.language,
        Some(function::DiagnosticContext::SelectColumn(column)),
    )
    .map_err(Error::Function)?;
    let result = conversion.result();
    let mut info = catalog
        .cast_info(&crate::sql_type::ast(Type::Character(result.declaration)))
        .ok_or(Error::UnknownContext)?;
    if info.logical_type() != Some(Type::Character(result.declaration)) {
        return Err(Error::UnknownContext);
    }
    // A computed function result never inherits an operand's alias type ID.
    info.user_type_id = info.system_type_id.map(i32::from);
    info.collation_name = result.collation.name().map(str::to_owned);
    let properties = Properties::expression(operation == function::Function::Translate);
    Ok(Some(Plan {
        original: expression,
        operands,
        declarations,
        conversion,
        info,
        properties,
    }))
}
fn validate_projected(
    catalog: &CatalogSnapshot,
    expression: &Expr,
    sources: &[Source],
    scope: &Scope,
    context: &Context<'_>,
    column: NonZeroUsize,
    query_depth: usize,
) -> Result<(), Error> {
    struct Check<'a, 'b> {
        catalog: &'a CatalogSnapshot,
        sources: &'a [Source],
        scope: &'a Scope,
        context: &'a Context<'b>,
        column: NonZeroUsize,
        depth: usize,
        nodes: usize,
        query_depth: usize,
        nested_queries: usize,
    }
    impl Visitor for Check<'_, '_> {
        type Break = Error;
        fn pre_visit_query(&mut self, query: &Query) -> std::ops::ControlFlow<Error> {
            if self.nested_queries == 0 {
                let mut input = self.scope.clone();
                input.rows.push(Some(self.sources.to_vec()));
                if let Err(error) = query_in(
                    self.catalog,
                    query,
                    &input,
                    self.context,
                    self.query_depth + 1,
                ) {
                    return std::ops::ControlFlow::Break(error);
                }
            }
            self.nested_queries += 1;
            std::ops::ControlFlow::Continue(())
        }
        fn post_visit_query(&mut self, _: &Query) -> std::ops::ControlFlow<Error> {
            self.nested_queries -= 1;
            std::ops::ControlFlow::Continue(())
        }
        fn pre_visit_expr(&mut self, expr: &Expr) -> std::ops::ControlFlow<Error> {
            if self.nested_queries > 0 {
                return std::ops::ControlFlow::Continue(());
            }
            self.depth += 1;
            self.nodes += 1;
            if self.depth >= MAX_SCOPE_DEPTH || self.nodes > 4096 {
                return std::ops::ControlFlow::Break(Error::UnsupportedSyntax);
            }
            if let Err(error) = call(expr) {
                return std::ops::ControlFlow::Break(error);
            }
            std::ops::ControlFlow::Continue(())
        }
        fn post_visit_expr(&mut self, expr: &Expr) -> std::ops::ControlFlow<Error> {
            if self.nested_queries > 0 {
                return std::ops::ControlFlow::Continue(());
            }
            self.depth -= 1;
            match bind_local(
                self.catalog,
                expr,
                self.sources,
                self.scope,
                self.context,
                self.column,
                0,
            ) {
                Err(error) => std::ops::ControlFlow::Break(error),
                _ => std::ops::ControlFlow::Continue(()),
            }
        }
    }
    let mut check = Check {
        catalog,
        sources,
        scope,
        context,
        column,
        depth: 0,
        nodes: 0,
        query_depth,
        nested_queries: 0,
    };
    match expression.visit(&mut check) {
        std::ops::ControlFlow::Continue(()) => Ok(()),
        std::ops::ControlFlow::Break(error) => Err(error),
    }
}
/// Bind using the caller's nearest row scopes and declaration-only parameters.
pub fn bind<'a>(
    catalog: &CatalogSnapshot,
    expression: &'a Expr,
    scope: &Scope,
    context: &Context<'_>,
    column: NonZeroUsize,
) -> Result<Option<Plan<'a>>, Error> {
    bind_local(catalog, expression, &[], scope, context, column, 0)
}
/// Bind projected function calls against the same explicit source/CTE machinery
/// as ordinary projection inference. Wildcards advance real SELECT positions.
pub fn query<'a>(
    catalog: &CatalogSnapshot,
    query: &'a Query,
    outer: &Scope,
    context: &Context<'_>,
) -> Result<Vec<(NonZeroUsize, Plan<'a>)>, Error> {
    query_in(catalog, query, outer, context, 0)
}
fn query_in<'a>(
    catalog: &CatalogSnapshot,
    query: &'a Query,
    outer: &Scope,
    context: &Context<'_>,
    depth: usize,
) -> Result<Vec<(NonZeroUsize, Plan<'a>)>, Error> {
    if depth >= MAX_SCOPE_DEPTH {
        return Err(Error::UnsupportedSyntax);
    }
    if !contains_functions(query) {
        return Ok(Vec::new());
    }
    let SetExpr::Select(select) = query.body.as_ref() else {
        return Err(Error::UnsupportedSyntax);
    };
    let scope = scope_with_functions(catalog, query, outer, context, depth)?;
    let sources = sources_with_functions(catalog, select, &scope, context, depth)?;
    let mut result = Vec::new();
    let mut position = 1;
    for item in &select.projection {
        match item {
            SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => {
                let column = NonZeroUsize::new(position).ok_or(Error::UnsupportedSyntax)?;
                validate_projected(catalog, e, &sources, &scope, context, column, depth)?;
                if let Some(plan) = bind_local(catalog, e, &sources, &scope, context, column, 0)? {
                    result.push((column, plan))
                }
                position += 1;
            }
            SelectItem::Wildcard(options) if *options == WildcardAdditionalOptions::default() => {
                position += sources.iter().map(|s| s.fields.len()).sum::<usize>();
            }
            SelectItem::QualifiedWildcard(
                SelectItemQualifiedWildcardKind::ObjectName(name),
                options,
            ) if *options == WildcardAdditionalOptions::default() => {
                position +=
                    crate::binding_scope::resolve_source(&qualified(name), &sources, &scope.rows)
                        .ok_or(Error::UnknownOperand)?
                        .fields
                        .len();
            }
            _ => return Err(Error::UnsupportedSyntax),
        }
    }
    Ok(result)
}

fn contains_functions(query: &Query) -> bool {
    struct Find(bool);
    impl Visitor for Find {
        type Break = ();
        fn pre_visit_expr(&mut self, expr: &Expr) -> std::ops::ControlFlow<()> {
            if matches!(expr,Expr::Function(f) if matches!(f.name.to_string().to_ascii_lowercase().as_str(),"concat_ws"|"translate"))
            {
                self.0 = true;
                return std::ops::ControlFlow::Break(());
            }
            std::ops::ControlFlow::Continue(())
        }
    }
    let mut find = Find(false);
    let _ = query.visit(&mut find);
    find.0
}
const MAX_SCOPE_DEPTH: usize = 64;
fn scope_with_functions(
    catalog: &CatalogSnapshot,
    query: &Query,
    outer: &Scope,
    context: &Context<'_>,
    depth: usize,
) -> Result<Scope, Error> {
    if depth >= MAX_SCOPE_DEPTH {
        return Err(Error::UnsupportedSyntax);
    }
    let mut scope = declaration_scopes(catalog, query, outer).body;
    if let Some(with) = &query.with {
        // Preserve forward/self shadows: reserve every name before definitions.
        for cte in &with.cte_tables {
            scope.insert(cte.alias.name.value.to_lowercase(), Vec::new());
        }
        for cte in &with.cte_tables {
            if crate::cte_recursion::anchor(cte).is_some() {
                return Err(Error::UnsupportedSyntax);
            }
            let mut input = scope.clone();
            input.rows.clear();
            let mut fields = fields_in(catalog, &cte.query, &input, context, depth + 1)?;
            rename(&mut fields, &cte.alias);
            scope.insert(cte.alias.name.value.to_lowercase(), fields);
        }
    }
    Ok(scope)
}
fn source_with_functions(
    catalog: &CatalogSnapshot,
    factor: &TableFactor,
    scope: &Scope,
    context: &Context<'_>,
    apply: bool,
    depth: usize,
) -> Result<Source, Error> {
    let TableFactor::Derived {
        subquery,
        alias,
        lateral,
        ..
    } = factor
    else {
        return source_with_correlation(catalog, factor, scope, apply).ok_or(Error::UnknownOperand);
    };
    let mut input = scope.clone();
    if !apply && !lateral {
        input.rows.clear();
    }
    let mut fields = fields_in(catalog, subquery, &input, context, depth + 1)?;
    for field in &mut fields {
        if field.properties.origin == msduck_core::result::Origin::Expression {
            field.properties.origin = msduck_core::result::Origin::Derived;
        }
    }
    let alias = alias.as_ref().ok_or(Error::UnknownOperand)?;
    rename(&mut fields, alias);
    Ok(Source {
        qualifiers: vec![alias.name.value.to_lowercase()],
        fields,
    })
}
fn sources_with_functions(
    catalog: &CatalogSnapshot,
    select: &Select,
    scope: &Scope,
    context: &Context<'_>,
    depth: usize,
) -> Result<Vec<Source>, Error> {
    let mut sources = Vec::new();
    for table in &select.from {
        let start = sources.len();
        sources.push(source_with_functions(
            catalog,
            &table.relation,
            scope,
            context,
            false,
            depth,
        )?);
        for join in &table.joins {
            let apply = matches!(
                join.join_operator,
                JoinOperator::CrossApply | JoinOperator::OuterApply
            );
            let mut input = scope.clone();
            if apply {
                input.rows.push(Some(sources[start..].to_vec()));
            }
            let mut right =
                source_with_functions(catalog, &join.relation, &input, context, apply, depth)?;
            if matches!(
                join.join_operator,
                JoinOperator::Right(_) | JoinOperator::RightOuter(_) | JoinOperator::FullOuter(_)
            ) {
                for source in &mut sources[start..] {
                    for field in &mut source.fields {
                        field.properties.null_extend();
                    }
                }
            }
            if matches!(
                join.join_operator,
                JoinOperator::Left(_)
                    | JoinOperator::LeftOuter(_)
                    | JoinOperator::FullOuter(_)
                    | JoinOperator::OuterApply
            ) {
                for field in &mut right.fields {
                    field.properties.null_extend();
                }
            }
            sources.push(right);
        }
    }
    Ok(sources)
}
/// Opt-in result fields with explicit function context. Ordinary field inference
/// is retained; only known function results and their resolved row references are
/// replaced. Recursive CTEs/set members remain an explicit unsupported barrier.
pub fn fields(
    catalog: &CatalogSnapshot,
    query: &Query,
    outer: &Scope,
    context: &Context<'_>,
) -> Result<Vec<Field>, Error> {
    fields_in(catalog, query, outer, context, 0)
}
fn fields_in(
    catalog: &CatalogSnapshot,
    query: &Query,
    outer: &Scope,
    context: &Context<'_>,
    depth: usize,
) -> Result<Vec<Field>, Error> {
    if depth >= MAX_SCOPE_DEPTH {
        return Err(Error::UnsupportedSyntax);
    }
    if !contains_functions(query) {
        return query_fields(catalog, query, outer).ok_or(Error::UnknownOperand);
    }
    let SetExpr::Select(select) = query.body.as_ref() else {
        return Err(Error::UnsupportedSyntax);
    };
    let scope = scope_with_functions(catalog, query, outer, context, depth)?;
    let sources = sources_with_functions(catalog, select, &scope, context, depth)?;
    let ordinary = query_fields(catalog, query, &scope).ok_or(Error::UnknownOperand)?;
    if matches!(query.for_clause, Some(ForClause::Json { .. })) {
        // FOR JSON is one complete output field, not the SELECT-list fields.
        // Validate the underlying original operands without overwriting its
        // established descriptor, name, fragment flag or logical properties.
        query_in(catalog, query, outer, context, depth + 1)?;
        return Ok(ordinary);
    }
    let grouping = crate::grouping_properties::Plan::new(select, &sources, &scope.rows)
        .ok_or(Error::UnknownOperand)?;
    let mut ordinary = ordinary.into_iter();
    let mut result = Vec::new();
    let mut position = 1usize;
    for item in &select.projection {
        match item {
            SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => {
                let mut field = ordinary.next().ok_or(Error::UnknownOperand)?;
                validate_projected(
                    catalog,
                    e,
                    &sources,
                    &scope,
                    context,
                    NonZeroUsize::new(position).ok_or(Error::UnsupportedSyntax)?,
                    depth,
                )?;
                if let Some(plan) = bind_local(
                    catalog,
                    e,
                    &sources,
                    &scope,
                    context,
                    NonZeroUsize::new(position).ok_or(Error::UnsupportedSyntax)?,
                    0,
                )? {
                    field.info = Some(plan.info.clone());
                    field.collation = Some(Ok(plan.conversion.result().collation.clone()));
                    field.properties = plan.properties;
                } else {
                    let ids = match e {
                        Expr::Identifier(id) => Some(vec![id]),
                        Expr::CompoundIdentifier(ids) => Some(ids.iter().collect()),
                        _ => None,
                    };
                    if let Some(ids) = ids
                        && !(ids.len() == 1
                            && ids[0].quote_style.is_none()
                            && ids[0].value.starts_with('@'))
                        && let Some(source) =
                            crate::binding_scope::resolve(&ids, &sources, &scope.rows)
                    {
                        field.info = source.info.clone();
                        field.collation = source.collation.clone();
                        field.properties = grouping.field_properties(source, &sources);
                    }
                }
                result.push(field);
                position += 1;
            }
            SelectItem::Wildcard(options) if *options == WildcardAdditionalOptions::default() => {
                for source in &sources {
                    for field in &source.fields {
                        ordinary.next().ok_or(Error::UnknownOperand)?;
                        let mut output = field.clone();
                        output.properties = grouping.field_properties(field, &sources);
                        result.push(output);
                        position += 1;
                    }
                }
            }
            SelectItem::QualifiedWildcard(
                SelectItemQualifiedWildcardKind::ObjectName(name),
                options,
            ) if *options == WildcardAdditionalOptions::default() => {
                let source =
                    crate::binding_scope::resolve_source(&qualified(name), &sources, &scope.rows)
                        .ok_or(Error::UnknownOperand)?;
                for field in &source.fields {
                    ordinary.next().ok_or(Error::UnknownOperand)?;
                    let mut output = field.clone();
                    output.properties = grouping.field_properties(field, &sources);
                    result.push(output);
                    position += 1;
                }
            }
            _ => return Err(Error::UnsupportedSyntax),
        }
    }
    if ordinary.next().is_some() {
        return Err(Error::UnknownOperand);
    }
    Ok(result)
}
