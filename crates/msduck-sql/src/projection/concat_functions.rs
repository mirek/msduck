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
    if let Expr::Nested(inner) = expr {
        return call(inner);
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
) -> Result<conversion::Declaration, Error> {
    if let Expr::Nested(inner) = expr {
        return declaration(catalog, inner, sources, scope);
    }
    if conditional::literal_null(expr) {
        return Ok(conversion::Declaration::null_literal());
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
                let n = u16::try_from(s.encode_utf16().count().max(1))
                    .map_err(|_| Error::UnknownOperand)?;
                Type::Character(
                    CharacterType::new(family, Length::Bounded(n))
                        .map_err(|_| Error::UnknownOperand)?,
                )
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
        Some(
            expression_collation(catalog, expr, sources, scope)
                .ok_or(Error::UnknownContext)?
                .map_err(|e| match e {
                    msduck_core::collation::Conflict::Operation(e) => {
                        Error::Function(conversion::Error::Function(function::Error::Sql(e)))
                    }
                    _ => Error::UnknownContext,
                })?,
        )
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
) -> Result<Option<Plan<'a>>, Error> {
    let Some((operation, operands)) = call(expression)? else {
        return Ok(None);
    };
    let declarations = operands
        .iter()
        .map(|e| declaration(catalog, e, sources, scope))
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
/// Bind using the caller's nearest row scopes and declaration-only parameters.
pub fn bind<'a>(
    catalog: &CatalogSnapshot,
    expression: &'a Expr,
    scope: &Scope,
    context: &Context<'_>,
    column: NonZeroUsize,
) -> Result<Option<Plan<'a>>, Error> {
    bind_local(catalog, expression, &[], scope, context, column)
}
/// Bind projected function calls against the same explicit source/CTE machinery
/// as ordinary projection inference. Wildcards advance real SELECT positions.
pub fn query<'a>(
    catalog: &CatalogSnapshot,
    query: &'a Query,
    outer: &Scope,
    context: &Context<'_>,
) -> Result<Vec<(NonZeroUsize, Plan<'a>)>, Error> {
    let SetExpr::Select(select) = query.body.as_ref() else {
        return Err(Error::UnsupportedSyntax);
    };
    let scope = declaration_scopes(catalog, query, outer).body;
    let sources = sources(catalog, select, &scope).ok_or(Error::UnknownOperand)?;
    let mut result = Vec::new();
    let mut position = 1;
    for item in &select.projection {
        match item {
            SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => {
                let column = NonZeroUsize::new(position).ok_or(Error::UnsupportedSyntax)?;
                if let Some(plan) = bind_local(catalog, e, &sources, &scope, context, column)? {
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
