//! Projection inference over explicit catalog inputs. No database access.
use crate::expression_metadata::{conditional, openjson, storage, temporal};
use crate::{
    binding_scope::{Field, QueryScopes, Scope, Source},
    catalog_snapshot::CatalogSnapshot,
};
use msduck_core::catalog::TypeMetadata as Info;
use sqlparser::ast::*;
use std::collections::HashMap;

pub mod ansi_padding;
pub mod character_extrema;
mod collation_validation;
mod constant_case;
pub use collation_validation::{
    annotate_unicode_case_inputs, lower_bin2_comparisons, lower_unicode_binary_conversions,
    validate_query_operations,
};

/// SQL result labels are independent of backend-generated expression names.
pub(crate) fn expression_name(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Identifier(id) if !id.value.starts_with('@') => Some(id.value.clone()),
        Expr::CompoundIdentifier(ids) => ids.last().map(|id| id.value.clone()),
        Expr::Nested(expr)
        | Expr::UnaryOp {
            op: UnaryOperator::Plus,
            expr,
        } => expression_name(expr),
        _ => None,
    }
}

pub fn scopes(catalog: &CatalogSnapshot, query: &Query, outer: &Scope) -> QueryScopes {
    let mut result = declaration_scopes(catalog, query, outer);
    if let SetExpr::Select(select) = query.body.as_ref() {
        let local = sources(catalog, select, &result.body);
        result.body.rows.push(local);
    }
    result
}

// Shared by visitor snapshots and standalone projection binding. Reserve all
// declaration names before resolving definitions, including self references.
fn declaration_scopes(catalog: &CatalogSnapshot, query: &Query, outer: &Scope) -> QueryScopes {
    let mut body = outer.clone();
    let mut definitions = std::collections::VecDeque::new();
    if let Some(with) = &query.with {
        // Reserve every declaration before inference, so a forward reference
        // cannot accidentally acquire a same-named base table's metadata.
        for cte in &with.cte_tables {
            body.insert(cte.alias.name.value.to_lowercase(), Vec::new());
        }
        for cte in &with.cte_tables {
            // An unresolved/self-recursive CTE must shadow a same-named table.
            let name = cte.alias.name.value.to_lowercase();
            let mut definition = body.clone();
            definition.rows.clear();
            definition.insert(name.clone(), Vec::new());
            let anchor = crate::cte_recursion::anchor(cte);
            let mut fields = if let Some(anchor) = &anchor {
                member_fields(catalog, anchor, &definition).unwrap_or_default()
            } else {
                query_fields(catalog, &cte.query, &definition).unwrap_or_default()
            };
            rename(&mut fields, &cte.alias);
            if anchor.is_some() {
                // SQL Server recursive columns take their anchor types. Bind
                // self references before visiting the definition, retaining the
                // placeholder barrier when anchor inference is unavailable.
                for field in &mut fields {
                    field.json_fragment = false;
                    field.properties.null_extend();
                    field.properties.origin = msduck_core::result::Origin::Derived;
                }
                definition.insert(name.clone(), fields.clone());
            }
            definitions.push_back(definition);
            body.insert(name, fields);
        }
    }
    QueryScopes {
        inherited: outer.clone(),
        body,
        definitions,
    }
}

pub fn query_fields(catalog: &CatalogSnapshot, query: &Query, outer: &Scope) -> Option<Vec<Field>> {
    query_fields_in(catalog, query, outer, false)
}

/// View outputs retain computed and identity provenance. Aggregate/relational
/// outputs become view columns; ordinary derived row sources still erase
/// computed provenance before reaching this boundary.
pub fn view_fields(catalog: &CatalogSnapshot, query: &Query) -> Option<Vec<Field>> {
    let mut fields = query_fields_in(catalog, query, &Scope::default(), true)?;
    for field in &mut fields {
        if field.properties.origin == msduck_core::result::Origin::Derived {
            field.properties.origin = msduck_core::result::Origin::Stored;
        }
    }
    Some(fields)
}

fn query_fields_in(
    catalog: &CatalogSnapshot,
    query: &Query,
    outer: &Scope,
    view_definition: bool,
) -> Option<Vec<Field>> {
    if matches!(query.for_clause, Some(ForClause::Json { .. })) {
        return Some(vec![Field {
            collation: None,
            name: "JSON_F52E2B61-18A1-11d1-B105-00805F49916B".into(),
            info: catalog.cast_info(&DataType::Nvarchar(Some(CharacterLength::Max))),
            properties: Default::default(),
            json_fragment: matches!(
                query.for_clause,
                Some(ForClause::Json {
                    without_array_wrapper: false,
                    ..
                })
            ),
        }]);
    }
    let scope = declaration_scopes(catalog, query, outer).body;
    if query.with.as_ref().is_some_and(|with| {
        with.cte_tables.iter().any(|cte| {
            scope
                .get(&cte.alias.name.value.to_lowercase())
                .is_none_or(Vec::is_empty)
        })
    }) {
        return None;
    }
    body_fields(catalog, &query.body, &scope, view_definition)
}
/// Anchor/member inference adds literal types to the ordinary projection rules
/// without executing expressions or inferring a recursive fixed point.
pub(crate) fn member_fields(
    catalog: &CatalogSnapshot,
    query: &Query,
    scope: &Scope,
) -> Option<Vec<Field>> {
    let mut fields = query_fields(catalog, query, scope)?;
    if let SetExpr::Select(select) = query.body.as_ref()
        && fields.len() == select.projection.len()
        && select.projection.iter().all(|item| {
            matches!(
                item,
                SelectItem::UnnamedExpr(_) | SelectItem::ExprWithAlias { .. }
            )
        })
    {
        let local = sources(catalog, select, scope)?;
        for (field, item) in fields.iter_mut().zip(&select.projection) {
            let expr = match item {
                SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => e,
                _ => unreachable!(),
            };
            if field.info.is_none() {
                field.info = member_expression(catalog, expr, &local, scope);
            }
        }
    }
    Some(fields)
}

fn member_expression(
    catalog: &CatalogSnapshot,
    expr: &Expr,
    sources: &[Source],
    scope: &Scope,
) -> Option<Info> {
    if let Some(info) = expression(catalog, expr, sources, scope) {
        return Some(info);
    }
    if let Some(crate::result_types::ResultType::Character { family, length }) =
        crate::result_types::expression_type(expr)
    {
        use msduck_core::character::{Family, Length};
        let name = match family {
            Family::Char => "char",
            Family::Varchar => "varchar",
            Family::Nchar => "nchar",
            Family::Nvarchar => "nvarchar",
        };
        let mut info = catalog.types.get(name)?.clone();
        info.max_length = Some(match length {
            Length::Max => -1,
            Length::Bounded(width) => i16::try_from(
                i32::from(width)
                    * if matches!(family, Family::Nchar | Family::Nvarchar) {
                        2
                    } else {
                        1
                    },
            )
            .ok()?,
        });
        return Some(info);
    }
    match expr {
        Expr::Nested(value) | Expr::Collate { expr: value, .. } => {
            member_expression(catalog, value, sources, scope)
        }
        Expr::BinaryOp { left, op, right } => {
            let left = member_expression(catalog, left, sources, scope)?;
            let right = member_expression(catalog, right, sources, scope)?;
            crate::expression_metadata::arithmetic::result(catalog, op, &left, &right)
        }
        _ => catalog.cast_info(&storage::kind(expr, &Default::default(), &|_| None)?),
    }
}

type Collation = Result<msduck_core::collation::Label, msduck_core::collation::Conflict>;

/// Report known failures inside sensitive expressions, independently of the
/// eventual output label. This is not complete statement collation validation.
pub fn validate_collation_operations(
    fields: &[Field],
) -> Result<(), msduck_core::diagnostic::SqlError> {
    for field in fields {
        if let Some(Err(msduck_core::collation::Conflict::Operation(error))) = &field.collation {
            return Err(error.clone());
        }
    }
    Ok(())
}

fn merge_collations(left: Option<&Collation>, right: Option<&Collation>) -> Option<Collation> {
    match (left, right) {
        (Some(Err(error)), _) | (_, Some(Err(error))) => Some(Err(error.clone())),
        (Some(Ok(left)), Some(Ok(right))) => Some(left.combine(right)),
        _ => None,
    }
}

fn combine_collations(values: impl Iterator<Item = Option<Collation>>) -> Option<Collation> {
    let mut merged = None;
    let mut unknown = false;
    for value in values {
        if let Some(value) = value {
            merged = match merged {
                None => Some(value),
                Some(left) => merge_collations(Some(&left), Some(&value)),
            };
        } else {
            unknown = true;
        }
    }
    if unknown && !matches!(merged, Some(Err(_))) {
        None
    } else {
        merged
    }
}

fn sensitive_collation(
    values: impl Iterator<Item = Option<Collation>>,
    operation: msduck_core::collation::Operation,
) -> Option<Collation> {
    let values = values.collect::<Vec<_>>();
    // Preserve a known inner failure even when another argument is unknown.
    for value in &values {
        if let Some(Err(error)) = value {
            return Some(Err(error.clone()));
        }
    }
    let labels = values
        .into_iter()
        .collect::<Option<Vec<_>>>()?
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    match operation.resolve(&labels) {
        Ok(label) => label.map(Ok),
        Err(error) => Some(Err(error)),
    }
}

/// Preserve coercion labels independently of type/width and physical storage.
/// Unsupported expressions remain unknown; this does not validate COLLATE names
/// or apply comparisons. Those require a complete binding/operation plan.
fn expression_collation(
    catalog: &CatalogSnapshot,
    expr: &Expr,
    sources: &[Source],
    scope: &Scope,
) -> Option<Collation> {
    use msduck_core::collation::Label;
    let default = || {
        Some(Ok(Label::CoercibleDefault(
            catalog.default_collation.clone()?,
        )))
    };
    let recurse = |value: &Expr| expression_collation(catalog, value, sources, scope);
    let character = |value: &Expr| {
        member_expression(catalog, value, sources, scope).is_some_and(|info| {
            matches!(info.system_type_id, Some(35 | 99 | 167 | 175 | 231 | 239))
        })
    };
    let string_argument = |value: &Expr| {
        if let Some(label) = recurse(value) {
            return Some(label);
        }
        if character(value) {
            return None;
        }
        if !conditional::literal_null(value) {
            member_expression(catalog, value, sources, scope)?;
        }
        default()
    };
    match expr {
        Expr::Nested(value) => recurse(value),
        Expr::Value(value)
            if matches!(
                value.value,
                Value::SingleQuotedString(_) | Value::NationalStringLiteral(_)
            ) =>
        {
            default()
        }
        Expr::Identifier(id) if id.value.starts_with('@') => {
            character(expr).then(default).flatten()
        }
        Expr::Identifier(id) => crate::binding_scope::resolve(&[id], sources, &scope.rows)?
            .collation
            .clone(),
        Expr::CompoundIdentifier(ids) => {
            crate::binding_scope::resolve(&ids.iter().collect::<Vec<_>>(), sources, &scope.rows)?
                .collation
                .clone()
        }
        Expr::Collate {
            expr: value,
            collation,
        } => match recurse(value) {
            Some(Err(error)) => Some(Err(error)),
            Some(Ok(label)) => Some(Ok(label.collate(collation.to_string()))),
            None if character(value) => Some(Ok(Label::Explicit(collation.to_string()))),
            None => None,
        },
        Expr::Cast {
            expr: value,
            data_type,
            ..
        }
        | Expr::Convert {
            expr: value,
            data_type: Some(data_type),
            ..
        } if crate::character_storage::is_character(data_type) => {
            if let Some(label) = recurse(value) {
                return Some(label);
            }
            if character(value) {
                return None;
            }
            // A known non-character input receives the explicit database default.
            if !conditional::literal_null(value) {
                member_expression(catalog, value, sources, scope)?;
            }
            default()
        }
        Expr::Substring { expr: value, .. } => string_argument(value),
        Expr::Trim {
            expr: value,
            trim_what: None,
            trim_characters: None,
            ..
        } => string_argument(value),
        Expr::Trim {
            expr: value,
            trim_what: Some(characters),
            trim_characters: None,
            ..
        } => sensitive_collation(
            // TRIM's character list precedes the source in SQL Server's
            // diagnostic operand order; LTRIM/RTRIM use source, then list.
            [characters.as_ref(), value.as_ref()]
                .into_iter()
                .map(string_argument),
            msduck_core::collation::Operation::Trim,
        ),
        Expr::Function(function) => {
            let FunctionArguments::List(args) = &function.args else {
                return None;
            };
            if matches!(
                function.name.to_string().to_ascii_uppercase().as_str(),
                "MIN" | "MAX"
            ) && crate::aggregate::validate(function).is_ok()
                && let [FunctionArg::Unnamed(FunctionArgExpr::Expr(value))] = args.args.as_slice()
            {
                return recurse(character_extrema::logical_source(value));
            }
            if function.over.is_some()
                || function.filter.is_some()
                || function.null_treatment.is_some()
                || !function.within_group.is_empty()
                || !matches!(function.parameters, FunctionArguments::None)
                || args.duplicate_treatment.is_some()
                || !args.clauses.is_empty()
            {
                return None;
            }
            let values = args
                .args
                .iter()
                .map(|arg| match arg {
                    FunctionArg::Unnamed(FunctionArgExpr::Expr(value)) => Some(value),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()?;
            let name = function.name.to_string().to_ascii_uppercase();
            match (name.as_str(), values.as_slice()) {
                ("CHAR" | "NCHAR" | "SPACE", [_]) => default(),
                ("LOWER" | "UPPER" | "LTRIM" | "RTRIM" | "TRIM" | "REVERSE", [value])
                | ("LEFT" | "RIGHT" | "REPLICATE", [value, _])
                | ("SUBSTRING", [value, _, _]) => string_argument(value),
                ("LTRIM", [_, _]) => sensitive_collation(
                    values.into_iter().map(string_argument),
                    msduck_core::collation::Operation::Ltrim,
                ),
                ("RTRIM", [_, _]) => sensitive_collation(
                    values.into_iter().map(string_argument),
                    msduck_core::collation::Operation::Rtrim,
                ),
                ("ISNULL", [first, replacement]) => {
                    // Replacement conversion adopts the first argument's collation;
                    // an already-invalid replacement expression must remain invalid.
                    if let Some(Err(error)) = recurse(replacement) {
                        return Some(Err(error));
                    }
                    recurse(if conditional::literal_null(first) {
                        replacement
                    } else {
                        first
                    })
                }
                ("COALESCE", [_, _, ..]) | ("IIF", [_, _, _]) | ("CHOOSE", [_, _, _, ..]) => {
                    combine_collations(
                        conditional::values(expr)
                            .into_iter()
                            .filter(|value| !conditional::literal_null(value))
                            .map(recurse),
                    )
                }
                ("CONCAT", [_, _, ..]) => {
                    combine_collations(values.into_iter().map(string_argument))
                }
                ("REPLACE", [_, _, _]) => sensitive_collation(
                    values.into_iter().map(string_argument),
                    msduck_core::collation::Operation::Replace,
                ),
                ("STUFF", [first, _, _, replacement]) => sensitive_collation(
                    [*first, *replacement].into_iter().map(string_argument),
                    msduck_core::collation::Operation::Stuff,
                ),
                ("NULLIF", [first, second]) => {
                    let comparison = sensitive_collation(
                        [*first, *second].into_iter().map(recurse),
                        msduck_core::collation::Operation::Equal,
                    )?;
                    match comparison {
                        Err(error) => Some(Err(error)),
                        Ok(_) => recurse(first),
                    }
                }
                _ => None,
            }
        }
        Expr::Case { .. } => combine_collations(
            conditional::values(expr)
                .into_iter()
                .filter(|value| !conditional::literal_null(value))
                .map(recurse),
        ),
        Expr::BinaryOp {
            left,
            op: BinaryOperator::Plus | BinaryOperator::StringConcat,
            right,
        } => merge_collations(recurse(left).as_ref(), recurse(right).as_ref()),
        Expr::Subquery(query) => {
            let mut inherited = scope.clone();
            inherited.rows.push(Some(sources.to_vec()));
            let fields = query_fields(catalog, query, &inherited)?;
            (fields.len() == 1)
                .then(|| fields[0].collation.clone())
                .flatten()
        }
        _ => None,
    }
}

fn body_fields(
    catalog: &CatalogSnapshot,
    body: &SetExpr,
    scope: &Scope,
    view_definition: bool,
) -> Option<Vec<Field>> {
    let select = match body {
        SetExpr::Select(s) => s,
        SetExpr::Query(q) => return query_fields_in(catalog, q, scope, view_definition),
        SetExpr::SetOperation { left, right, .. } => {
            let (Some(left), Some(right)) = (
                body_fields(catalog, left, scope, view_definition),
                body_fields(catalog, right, scope, view_definition),
            ) else {
                return None;
            };
            if left.len() != right.len() {
                return None;
            }
            let mut fields = Vec::with_capacity(left.len());
            for (left, right) in left.into_iter().zip(right) {
                let info = match (
                    time_scale(left.info.as_ref()),
                    time_scale(right.info.as_ref()),
                ) {
                    (Some(a), Some(b)) => catalog.cast_info(&DataType::Time(
                        Some(u64::from(a.max(b))),
                        TimezoneInfo::None,
                    )),
                    _ => left
                        .info
                        .as_ref()
                        .zip(right.info.as_ref())
                        .and_then(|(a, b)| {
                            crate::expression_metadata::character::set_info(catalog, a, b).or_else(
                                || crate::expression_metadata::arithmetic::set_info(catalog, a, b),
                            )
                        }),
                };
                fields.push(Field {
                    collation: merge_collations(left.collation.as_ref(), right.collation.as_ref()),
                    name: left.name,
                    properties: left.properties.union(right.properties),
                    json_fragment: false,
                    info,
                });
            }
            return Some(fields);
        }
        SetExpr::Values(values) => {
            let first = values.rows.first()?;
            if values.rows.iter().any(|r| r.len() != first.len()) {
                return None;
            }
            let mut fields = Vec::new();
            for index in 0..first.len() {
                let info = if let Some(scale) =
                    temporal::common_scale(values.rows.iter().map(|r| &r[index]))
                {
                    catalog.cast_info(&DataType::Time(Some(u64::from(scale)), TimezoneInfo::None))
                } else {
                    let mut members = values.rows.iter().map(|row| &row[index]).filter(
                        |expr| !matches!(expr, Expr::Value(v) if matches!(v.value, Value::Null)),
                    );
                    members.next().and_then(|first| {
                        let first = member_expression(catalog, first, &[], scope)?;
                        members.try_fold(first, |left, expr| {
                            let right = member_expression(catalog, expr, &[], scope)?;
                            crate::expression_metadata::character::set_info(catalog, &left, &right)
                                .or_else(|| {
                                    crate::expression_metadata::arithmetic::set_info(
                                        catalog, &left, &right,
                                    )
                                })
                        })
                    })
                };
                fields.push(Field {
                    collation: combine_collations(
                        values
                            .rows
                            .iter()
                            .map(|row| &row[index])
                            .filter(|value| !conditional::literal_null(value))
                            .map(|value| expression_collation(catalog, value, &[], scope)),
                    ),
                    name: format!("col{}", index + 1),
                    properties: values
                        .rows
                        .iter()
                        .map(|row| {
                            crate::result_properties::expression(&row[index], &[], &scope.rows)
                        })
                        .reduce(|a, b| a.union(b))
                        .map(|mut p| {
                            p.origin = msduck_core::result::Origin::Derived;
                            p
                        })
                        .unwrap_or_default(),
                    json_fragment: false,
                    info,
                });
            }
            return Some(fields);
        }
        _ => return None,
    };
    let sources = sources(catalog, select, scope)?;
    let grouping = crate::grouping_properties::Plan::new(select, &sources, &scope.rows)?;
    let properties =
        |expr: &Expr| constant_case::properties(catalog, expr, &sources, scope, &grouping, 0);
    let grouped_field = |field: &Field| {
        let mut output = field.clone();
        output.properties = grouping.field_properties(field, &sources);
        output
    };
    let mut out = Vec::new();
    for item in &select.projection {
        match item {
            SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => {
                let name = match item {
                    SelectItem::ExprWithAlias { alias, .. } => alias.value.clone(),
                    _ => expression_name(e).unwrap_or_default(),
                };
                let info = expression(catalog, e, &sources, scope);
                let mut properties = properties(e);
                if properties.nullable == Some(false)
                    && matches!(e, Expr::Function(f) if f.name.to_string().eq_ignore_ascii_case("COALESCE"))
                {
                    let values = conditional::values(e);
                    let first = values
                        .iter()
                        .find(|value| !crate::result_properties::literal_null(value));
                    let first_info =
                        first.and_then(|value| member_expression(catalog, value, &sources, scope));
                    let arguments = values
                        .iter()
                        .filter(|value| !conditional::literal_null(value))
                        .map(|value| member_expression(catalog, value, &sources, scope))
                        .collect::<Option<Vec<_>>>();
                    if first_info
                        .as_ref()
                        .zip(arguments.as_ref())
                        .and_then(|(first, arguments)| {
                            crate::result_properties::coalesce_conversion(first, arguments)
                        })
                        != Some(false)
                    {
                        properties.null_extend();
                    }
                }
                // SQL Server omits fComputed for these result type families.
                if !view_definition
                    && expression_name(e).is_none()
                    && properties.origin == msduck_core::result::Origin::Expression
                    && info
                        .as_ref()
                        .is_some_and(|info| matches!(info.system_type_id, Some(41 | 42 | 43 | 98)))
                {
                    properties.origin = msduck_core::result::Origin::Derived;
                }
                if properties.origin == msduck_core::result::Origin::Expression {
                    if info.is_none()
                        && (matches!(e, Expr::Case { .. })
                            || matches!(e, Expr::Function(f) if matches!(f.name.to_string().to_ascii_uppercase().as_str(), "IIF" | "COALESCE" | "ISNULL" | "CHOOSE")))
                        && !conditional::values(e).iter().all(|value| {
                            conditional::literal_null(value)
                                || member_expression(catalog, value, &sources, scope)
                                    .and_then(|info| info.system_type_id)
                                    .is_some_and(|id| !matches!(id, 41 | 42 | 43 | 98))
                        })
                    {
                        properties = Default::default();
                    }
                    if let Expr::Cast { expr: input, .. } = e {
                        let input = crate::variant_cast::source(input).unwrap_or(input);
                        let source_info = expression(catalog, input, &sources, scope);
                        let source_properties = crate::result_properties::expression_with(
                            input,
                            &sources,
                            &scope.rows,
                            &|value| grouping.properties(value, &sources, &scope.rows),
                        );
                        let aggregate = matches!(input, Expr::Function(f) if source_properties.origin == msduck_core::result::Origin::Derived || matches!(f.name.to_string().to_ascii_uppercase().as_str(), "GROUPING" | "GROUPING_ID"));
                        if !view_definition
                            && (aggregate
                                || source_info
                                    .as_ref()
                                    .is_some_and(|i| i.system_type_id == Some(98)))
                        {
                            properties.origin = msduck_core::result::Origin::Derived;
                        } else if source_info.is_none()
                            && !matches!(input, Expr::Value(_))
                            && source_properties
                                != msduck_core::result::Properties::expression(false)
                        {
                            properties = Default::default();
                        }
                    }
                }
                out.push(Field {
                    collation: expression_collation(catalog, e, &sources, scope),
                    name,
                    info,
                    properties,
                    json_fragment: fragment(e, &sources, &scope.rows),
                });
            }
            SelectItem::Wildcard(options) if *options == WildcardAdditionalOptions::default() => {
                for s in &sources {
                    out.extend(s.fields.iter().map(grouped_field))
                }
            }
            SelectItem::QualifiedWildcard(
                SelectItemQualifiedWildcardKind::ObjectName(name),
                options,
            ) if *options == WildcardAdditionalOptions::default() => {
                let qualifier = qualified(name);
                let source =
                    crate::binding_scope::resolve_source(&qualifier, &sources, &scope.rows)?;
                out.extend(source.fields.iter().map(grouped_field));
            }
            _ => return None,
        }
    }
    Some(out)
}
fn sources(catalog: &CatalogSnapshot, select: &Select, scope: &Scope) -> Option<Vec<Source>> {
    let mut sources = Vec::new();
    for table in &select.from {
        let start = sources.len();
        sources.push(source(catalog, &table.relation, scope)?);
        for join in &table.joins {
            let apply = matches!(
                join.join_operator,
                JoinOperator::CrossApply | JoinOperator::OuterApply
            );
            let mut input = scope.clone();
            if apply {
                input.rows.push(Some(sources[start..].to_vec()));
            }
            let mut right = source_with_correlation(catalog, &join.relation, &input, apply)?;
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
    Some(sources)
}

fn qualified(name: &ObjectName) -> String {
    name.0
        .iter()
        .filter_map(|p| p.as_ident().map(|i| i.value.to_lowercase()))
        .collect::<Vec<_>>()
        .join(".")
}
fn rename(fields: &mut [Field], alias: &TableAlias) {
    for (field, name) in fields.iter_mut().zip(&alias.columns) {
        field.name = name.name.value.clone()
    }
}
fn source(catalog: &CatalogSnapshot, factor: &TableFactor, scope: &Scope) -> Option<Source> {
    source_with_correlation(catalog, factor, scope, false)
}
fn source_with_correlation(
    catalog: &CatalogSnapshot,
    factor: &TableFactor,
    scope: &Scope,
    apply: bool,
) -> Option<Source> {
    let (mut fields, alias, qualifiers) = match factor {
        TableFactor::Table { alias, .. } if crate::generate_series::is_series(factor) => {
            let info = crate::generate_series::result_type(factor, &Default::default())
                .and_then(|kind| catalog.cast_info(&kind));
            (
                vec![Field {
                    collation: None,
                    name: "value".into(),
                    info,
                    properties: Default::default(),
                    json_fragment: false,
                }],
                alias,
                vec!["generate_series".into()],
            )
        }
        TableFactor::OpenJsonTable { columns, alias, .. } => {
            let fields = openjson::columns(columns)
                .into_iter()
                .map(|(name, kind)| Field {
                    collation: None,
                    name,
                    info: catalog.cast_info(&kind.unwrap()),
                    properties: Default::default(),
                    json_fragment: false,
                })
                .collect();
            (fields, alias, vec!["openjson".into()])
        }
        TableFactor::Table {
            name,
            alias,
            args: None,
            ..
        } => {
            let key = qualified(name);
            let fields = if name.0.len() == 1 && scope.contains_key(&key) {
                scope[&key].clone()
            } else {
                catalog
                    .tables
                    .get(&name.to_string())
                    .cloned()
                    .unwrap_or_default()
            };
            // A real SQL table has columns. Empty metadata means unresolved
            // scope, not a zero-column source that a star may silently skip.
            if fields.is_empty() {
                return None;
            }
            let short = name
                .0
                .last()
                .and_then(|p| p.as_ident())
                .map(|i| i.value.to_lowercase())
                .unwrap_or_default();
            (fields, alias, vec![key, short])
        }
        TableFactor::Derived {
            subquery,
            alias,
            lateral,
            ..
        } => {
            let mut inherited = scope.clone();
            if !apply && !lateral {
                inherited.rows.clear();
            }
            let fields = query_fields(catalog, subquery, &inherited)?;
            (fields, alias, vec![])
        }
        _ => return None,
    };
    // CTE/derived expression columns lose computed provenance. A real catalog
    // view may expose computed columns, whose flags survive the outer SELECT.
    let catalog_source = matches!(factor, TableFactor::Table { name, args: None, .. }
        if name.0.len() != 1 || !scope.contains_key(&qualified(name)));
    for field in &mut fields {
        if !catalog_source && field.properties.origin == msduck_core::result::Origin::Expression {
            field.properties.origin = msduck_core::result::Origin::Derived;
        }
    }
    let qualifiers = if let Some(alias) = alias {
        rename(&mut fields, alias);
        vec![alias.name.value.to_lowercase()]
    } else {
        qualifiers
    };
    Some(Source { qualifiers, fields })
}
fn fragment(expr: &Expr, sources: &[Source], outer: &[Option<Vec<Source>>]) -> bool {
    if crate::for_json::fragment(expr) {
        return true;
    }
    let ids = match expr {
        Expr::Nested(inner) => return fragment(inner, sources, outer),
        Expr::Identifier(id) => vec![id],
        Expr::CompoundIdentifier(ids) => ids.iter().collect(),
        _ => return false,
    };
    crate::binding_scope::resolve(&ids, sources, outer).is_some_and(|field| field.json_fragment)
}

fn expression(
    catalog: &CatalogSnapshot,
    e: &Expr,
    sources: &[Source],
    scope: &Scope,
) -> Option<Info> {
    if let Expr::Collate { expr, .. } = e {
        return member_expression(catalog, expr, sources, scope);
    }
    if let Expr::Subquery(query) = e {
        let mut inherited = scope.clone();
        inherited.rows.push(Some(sources.to_vec()));
        let fields = query_fields(catalog, query, &inherited)?;
        return (fields.len() == 1)
            .then(|| fields[0].info.clone())
            .flatten();
    }
    if conditional::candidate(e)
        || matches!(
            e,
            Expr::Function(_) | Expr::BinaryOp { .. } | Expr::UnaryOp { .. }
        )
    {
        let currency_column = |value: &Expr| {
            if !matches!(
                value,
                Expr::Identifier(_) | Expr::CompoundIdentifier(_) | Expr::Subquery(_)
            ) {
                return None;
            }
            let info = expression(catalog, value, sources, scope)?;
            Some(match info.system_type_id? {
                48 => DataType::TinyInt(None),
                52 => DataType::SmallInt(None),
                56 => DataType::Int(None),
                127 => DataType::BigInt(None),
                104 => DataType::Bit(None),
                // Currency precedence needs the character family, not its width.
                167 | 175 => DataType::Varchar(None),
                231 | 239 => DataType::Nvarchar(None),
                id @ (60 | 122) => storage::catalog_scalar(i32::from(id), 0, 0)?,
                id => storage::catalog_scalar(
                    i32::from(id),
                    i32::from(info.precision?),
                    i32::from(info.scale?),
                )?,
            })
        };
        if let Expr::Function(function) = e
            && let Some(kind) =
                storage::decimal_aggregate(function, &HashMap::new(), &currency_column)
        {
            return catalog.cast_info(&kind);
        }
        if let Some(kind) =
            crate::expression_metadata::currency::kind(e, &HashMap::new(), &currency_column)
        {
            return catalog.cast_info(&crate::expression_metadata::currency::declaration(kind));
        }
    }
    if let Some(value) = storage::retained_argument(e) {
        let info = member_expression(catalog, value, sources, scope);
        if let Some(mut info) = info {
            let id = info.system_type_id?;
            let id = match id {
                167 | 175 => 167,
                231 | 239 => 231,
                _ => return None,
            };
            info.system_type_id = Some(id);
            info.user_type_id = Some(i32::from(id));
            if id == 231
                && info.max_length == Some(0)
                && matches!(e, Expr::Function(f) if matches!(f.name.to_string().to_ascii_uppercase().as_str(), "LOWER" | "UPPER"))
            {
                // SQL Server advertises NVARCHAR(1) for casing an empty literal.
                info.max_length = Some(2);
            }
            return Some(info);
        }
        return None;
    }
    let character_column = |value: &Expr| {
        if !matches!(value, Expr::Identifier(_) | Expr::CompoundIdentifier(_)) {
            return None;
        }
        let info = expression(catalog, value, sources, scope)?;
        let id = info.system_type_id?;
        if matches!(id, 167 | 175 | 231 | 239) {
            let unicode = matches!(id, 231 | 239);
            let length = if info.max_length? == -1 {
                msduck_core::character::Length::Max
            } else {
                msduck_core::character::Length::Bounded(
                    u16::try_from(info.max_length?).ok()? / if unicode { 2 } else { 1 },
                )
            };
            let family = match id {
                175 => msduck_core::character::Family::Char,
                231 => msduck_core::character::Family::Nvarchar,
                239 => msduck_core::character::Family::Nchar,
                _ => msduck_core::character::Family::Varchar,
            };
            let kind = msduck_core::character::CharacterType::new(family, length).ok()?;
            return Some(crate::sql_type::ast(msduck_core::types::Type::Character(
                kind,
            )));
        }
        if matches!(id, 165 | 173) {
            let length = if info.max_length? == -1 {
                BinaryLength::Max
            } else {
                BinaryLength::IntegerLength {
                    length: u64::try_from(info.max_length?).ok()?,
                }
            };
            return Some(if id == 173 {
                DataType::Binary(Some(u64::try_from(info.max_length?).ok()?))
            } else {
                DataType::Varbinary(Some(length))
            });
        }
        storage::catalog_scalar(
            i32::from(id),
            i32::from(info.precision.unwrap_or(0)),
            i32::from(info.scale.unwrap_or(0)),
        )
    };
    if let Some(kind) = crate::replicate::result_type(e, &HashMap::new(), &character_column)
        .or_else(|| crate::left_right::result_type(e, &HashMap::new(), &character_column))
    {
        return catalog.cast_info(&crate::sql_type::ast(msduck_core::types::Type::Character(
            kind,
        )));
    }
    if storage::string_escape_call(e) {
        return catalog.cast_info(&DataType::Nvarchar(Some(CharacterLength::Max)));
    }
    if let Expr::Function(f) = e
        && matches!(
            f.name.to_string().to_ascii_lowercase().as_str(),
            "json_value" | "__msduck_json_value"
        )
    {
        return catalog.cast_info(&DataType::Nvarchar(Some(CharacterLength::IntegerLength {
            length: 4000,
            unit: None,
        })));
    }

    if let Expr::Function(f) = e
        && let Some(scale) = temporal::timefromparts_scale(f)
    {
        return catalog.cast_info(&DataType::Time(Some(u64::from(scale)), TimezoneInfo::None));
    }
    if let Expr::Function(f) = e
        && let Some(value) = temporal::retained_argument(f)
    {
        return expression(catalog, value, sources, scope);
    }
    if let Expr::Function(f) = e
        && f.name.to_string().eq_ignore_ascii_case("DATEADD")
        && let FunctionArguments::List(args) = &f.args
        && let Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(value))) = args.args.get(2)
    {
        return expression(catalog, value, sources, scope);
    }
    if let Expr::Function(f) = e
        && let Ok(Some([first, replacement])) = conditional::isnull_args(f)
    {
        return expression(
            catalog,
            if conditional::literal_null(first) {
                replacement
            } else {
                first
            },
            sources,
            scope,
        );
    }

    if let Expr::Function(f) = e
        && let Ok(Some([first, _])) = conditional::nullif_args(f)
    {
        return expression(catalog, first, sources, scope).or_else(|| {
            let kind = storage::kind(first, &HashMap::new(), &|_| None)?;
            catalog.cast_info(&kind)
        });
    }

    if conditional::candidate(e) {
        let numeric = conditional::values(e)
            .into_iter()
            .filter(|value| !conditional::literal_null(value))
            .map(|value| {
                expression(catalog, value, sources, scope).or_else(|| {
                    catalog.cast_info(&storage::kind(value, &HashMap::new(), &|_| None)?)
                })
            })
            .collect::<Option<Vec<_>>>();
        if let Some(values) = numeric
            && let Some(first) = values.first()
            && let Some(initial) =
                crate::expression_metadata::arithmetic::set_info(catalog, first, first)
            && let Some(info) = values.iter().skip(1).try_fold(initial, |left, right| {
                crate::expression_metadata::arithmetic::set_info(catalog, &left, right)
            })
        {
            return Some(info);
        }
        let mut scale = None;
        for value in conditional::values(e) {
            if let Some(info) = expression(catalog, value, sources, scope) {
                match info.system_type_id {
                    Some(41) => {
                        let s = info.scale.filter(|s| *s <= 7)?;
                        scale = Some(scale.map_or(s, |previous: u8| previous.max(s)));
                    }
                    Some(167 | 175 | 231 | 239) => {}
                    _ => return None,
                }
            } else if !matches!(value,Expr::Value(v) if matches!(v.value,sqlparser::ast::Value::Null|sqlparser::ast::Value::SingleQuotedString(_)|sqlparser::ast::Value::NationalStringLiteral(_)))
            {
                return None;
            }
        }
        if let Some(scale) = scale {
            return catalog.cast_info(&DataType::Time(Some(scale as u64), TimezoneInfo::None));
        }
    }

    if let Expr::Function(function) = e
        && let Some(kind) = crate::grouping::result_type(function)
            .or_else(|| crate::session_function::result_type(function))
    {
        return catalog.cast_info(&kind);
    }
    if let Expr::Identifier(id) = e
        && let Some(kind) = crate::session_function::counter_type(&id.value)
    {
        return catalog.cast_info(&kind);
    }
    if let Expr::BinaryOp { left, op, right } = e {
        let mut left_info = member_expression(catalog, left, sources, scope);
        let mut right_info = member_expression(catalog, right, sources, scope);
        let null_info = |info: &Info| {
            let mut info = info.clone();
            info.max_length = Some(match info.system_type_id? {
                167 | 175 => 1,
                231 | 239 => 2,
                _ => return None,
            });
            Some(info)
        };
        if matches!(op, BinaryOperator::Plus | BinaryOperator::StringConcat) {
            if conditional::literal_null(left) {
                left_info = right_info.as_ref().and_then(null_info);
            }
            if conditional::literal_null(right) {
                right_info = left_info.as_ref().and_then(null_info);
            }
            if let Some(info) = left_info
                .as_ref()
                .zip(right_info.as_ref())
                .and_then(|(a, b)| {
                    crate::expression_metadata::character::concat_info(catalog, a, b)
                })
            {
                return Some(info);
            }
        }
        let left = left_info?;
        let right = right_info?;
        return crate::expression_metadata::arithmetic::result(catalog, op, &left, &right);
    }
    let ids = match e {
        Expr::Identifier(i) if i.value.starts_with('@') => {
            return scope.parameters.get(&i.value.to_lowercase()).cloned();
        }
        Expr::Identifier(i) => vec![i],
        Expr::CompoundIdentifier(ids) => ids.iter().collect(),
        Expr::Nested(e) => return expression(catalog, e, sources, scope),
        Expr::Cast { data_type, .. }
        | Expr::Convert {
            data_type: Some(data_type),
            ..
        } => return catalog.cast_info(data_type),
        _ => return None,
    };
    crate::binding_scope::resolve(&ids, sources, &scope.rows).and_then(|field| field.info.clone())
}

fn time_scale(info: Option<&Info>) -> Option<u8> {
    info?.time_scale()
}

/// Expand qualified stars into quoted column references for backends that cannot
/// bind an outer star. Preserve unqualified stars, expression order and lexical qualifiers.
pub fn expand_qualified_stars(
    catalog: &CatalogSnapshot,
    query: &mut Query,
    outer: &Scope,
) -> Option<()> {
    expand_stars_impl(catalog, query, outer, false)
}

/// Expand all stars before adding private execution columns.
pub fn expand_stars(catalog: &CatalogSnapshot, query: &mut Query, outer: &Scope) -> Option<()> {
    expand_stars_impl(catalog, query, outer, true)
}
fn expand_stars_impl(
    catalog: &CatalogSnapshot,
    query: &mut Query,
    outer: &Scope,
    all: bool,
) -> Option<()> {
    let scopes = scopes(catalog, query, outer);
    let select = match query.body.as_mut() {
        SetExpr::Select(select) => select,
        SetExpr::Query(inner) => return expand_stars_impl(catalog, inner, &scopes.body, all),
        _ => return None,
    };
    let (local, outer_rows) = scopes.body.rows.split_last()?;
    let local = local.as_deref()?;
    let mut expanded = Vec::new();
    for item in &select.projection {
        if all && let SelectItem::Wildcard(options) = item {
            if *options != WildcardAdditionalOptions::default() {
                return None;
            }
            for source in local {
                let qualifier = source.qualifiers.last()?;
                for field in &source.fields {
                    expanded.push(SelectItem::UnnamedExpr(Expr::CompoundIdentifier(vec![
                        Ident::with_quote('"', qualifier),
                        Ident::with_quote('"', &field.name),
                    ])));
                }
            }
        } else if let SelectItem::QualifiedWildcard(
            SelectItemQualifiedWildcardKind::ObjectName(name),
            options,
        ) = item
        {
            if *options != WildcardAdditionalOptions::default() {
                return None;
            }
            let source = crate::binding_scope::resolve_source(&qualified(name), local, outer_rows)?;
            if source.fields.is_empty() {
                return None;
            }
            let prefix = name
                .0
                .iter()
                .map(|part| part.as_ident().cloned())
                .collect::<Option<Vec<_>>>()?;
            for field in &source.fields {
                let mut ids = prefix.clone();
                ids.push(Ident::with_quote('"', &field.name));
                expanded.push(SelectItem::UnnamedExpr(Expr::CompoundIdentifier(ids)));
            }
        } else {
            expanded.push(item.clone());
        }
    }
    select.projection = expanded;
    Some(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn view_projection_properties_match_captured_sql_server_shapes() {
        use msduck_core::result::{Origin, Properties};
        let mut catalog = catalog();
        for (name, id) in [("bigint", 127), ("decimal", 106), ("sql_variant", 98)] {
            catalog.types.insert(
                name.into(),
                Info {
                    system_type_id: Some(id),
                    user_type_id: Some(i32::from(id)),
                    ..Default::default()
                },
            );
        }
        let int = catalog.cast_info(&DataType::Int(None));
        catalog.tables.insert(
            "dbo.probe_base".into(),
            [
                ("id", Origin::Identity, false),
                ("v", Origin::Stored, true),
                ("nn", Origin::Stored, false),
            ]
            .into_iter()
            .map(|(name, origin, nullable)| Field {
                name: name.into(),
                info: int.clone(),
                collation: None,
                json_fragment: false,
                properties: Properties {
                    nullable: Some(nullable),
                    origin,
                },
            })
            .collect(),
        );
        // Each shape was captured identically in two fresh SQL Server databases.
        for (sql, expected) in [
            (
                "SELECT id,v,nn FROM dbo.probe_base",
                vec![
                    (Origin::Identity, false),
                    (Origin::Stored, true),
                    (Origin::Stored, false),
                ],
            ),
            (
                "SELECT MIN(v),MAX(nn),COUNT(v),SUM(v) FROM dbo.probe_base",
                vec![(Origin::Stored, true); 4],
            ),
            (
                "SELECT 1,id+1,CAST(id AS BIGINT),v+1,ISNULL(v,0) FROM dbo.probe_base",
                vec![
                    (Origin::Expression, false),
                    (Origin::Expression, true),
                    (Origin::Expression, true),
                    (Origin::Expression, true),
                    (Origin::Expression, false),
                ],
            ),
            (
                "SELECT CAST(MIN(v) AS BIGINT),MIN(v)+1,ISNULL(MIN(v),0) FROM dbo.probe_base",
                vec![
                    (Origin::Expression, true),
                    (Origin::Expression, true),
                    (Origin::Expression, false),
                ],
            ),
            (
                "SELECT nn FROM dbo.probe_base UNION ALL SELECT nn FROM dbo.probe_base",
                vec![(Origin::Stored, false)],
            ),
            (
                "SELECT nn FROM dbo.probe_base GROUP BY nn",
                vec![(Origin::Stored, false)],
            ),
            (
                "SELECT ROW_NUMBER() OVER(ORDER BY id),SUM(v) OVER() FROM dbo.probe_base",
                vec![(Origin::Stored, true); 2],
            ),
            (
                "SELECT CAST('12:34:56.1234567' AS TIME(7)),CAST(1 AS DECIMAL(12,3)),CAST(1 AS SQL_VARIANT)",
                vec![(Origin::Expression, true); 3],
            ),
            (
                "WITH c AS (SELECT CAST('12:34' AS TIME) AS t,1 AS n) SELECT t,n FROM c",
                vec![(Origin::Stored, true), (Origin::Stored, false)],
            ),
            (
                "SELECT t,n FROM (SELECT CAST('12:34' AS TIME) AS t,1 AS n) d",
                vec![(Origin::Stored, true), (Origin::Stored, false)],
            ),
        ] {
            let fields = view_fields(&catalog, &query(sql)).unwrap();
            assert_eq!(
                fields
                    .iter()
                    .map(|f| (f.properties.origin, f.properties.nullable))
                    .collect::<Vec<_>>(),
                expected
                    .into_iter()
                    .map(|(origin, nullable)| (origin, Some(nullable)))
                    .collect::<Vec<_>>(),
                "{sql}"
            );
        }
        let fields = view_fields(
            &catalog,
            &query("SELECT CAST('12:34' AS TIME) AS t,CAST(1 AS SQL_VARIANT) AS v"),
        )
        .unwrap();
        catalog.tables.insert("dbo.typed_view".into(), fields);
        for sql in [
            "SELECT t,v FROM dbo.typed_view",
            "SELECT * FROM dbo.typed_view",
        ] {
            for fields in [
                view_fields(&catalog, &query(sql)).unwrap(),
                query_fields(&catalog, &query(sql), &Scope::default()).unwrap(),
            ] {
                assert!(
                    fields
                        .iter()
                        .all(|f| f.properties == Properties::expression(true)),
                    "{sql}"
                );
            }
        }
        assert_eq!(
            query_fields(
                &catalog,
                &query("SELECT CAST('12:34' AS TIME)"),
                &Scope::default()
            )
            .unwrap()[0]
                .properties
                .origin,
            Origin::Derived
        );
        assert_eq!(
            query_fields(
                &catalog,
                &query("SELECT CAST(MIN(v) AS BIGINT) FROM dbo.probe_base"),
                &Scope::default()
            )
            .unwrap()[0]
                .properties
                .origin,
            Origin::Derived
        );
    }

    #[test]
    fn concatenation_declarations_match_live_reference_and_bind_bin2() {
        let mut catalog = catalog();
        catalog.default_collation = Some("SQL_Latin1_General_CP1_CI_AS".into());
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../reference/character-concat-declarations.json"
        ))
        .unwrap();
        for entry in fixture["results"].as_array().unwrap() {
            let query = query(entry["query"].as_str().unwrap());
            let fields = query_fields(&catalog, &query, &Scope::default()).unwrap();
            let info = fields[0]
                .info
                .as_ref()
                .unwrap_or_else(|| panic!("{}", entry["name"]));
            let expected = &entry["reference"]["sets"][0]["columns"][0];
            let id = match expected["type"].as_str().unwrap() {
                "Char" => 175,
                "VarChar" => 167,
                "NChar" => 239,
                "NVarChar" => 231,
                value => panic!("{value}"),
            };
            let bytes = expected["length"].as_i64().unwrap();
            assert_eq!(info.system_type_id, Some(id), "{}", entry["name"]);
            assert_eq!(
                info.max_length,
                Some(if bytes == 65535 { -1 } else { bytes as i16 }),
                "{}",
                entry["name"]
            );
        }
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../../reference/bin2-constant-edges.json"))
                .unwrap();
        for entry in fixture["results"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["name"].as_str().unwrap().contains("plus"))
        {
            let mut query = query(entry["query"].as_str().unwrap());
            lower_bin2_comparisons(&catalog, &mut query, &Scope::default()).unwrap();
            assert!(!query.to_string().contains("COLLATE"), "{}", entry["name"]);
        }
    }
    #[test]
    fn bin2_comparison_plans_match_reference_diagnostics_and_are_atomic() {
        use msduck_core::collation::Label;
        let mut catalog = catalog();
        catalog.default_collation = Some("SQL_Latin1_General_CP1_CI_AS".into());
        catalog.tables.insert(
            "coll_scope".into(),
            [
                ("a", "Latin1_General_100_CI_AS"),
                ("b", "Latin1_General_100_CS_AS"),
            ]
            .into_iter()
            .map(|(name, collation)| Field {
                name: name.into(),
                info: catalog.cast_info(&DataType::Nvarchar(Some(
                    CharacterLength::IntegerLength {
                        length: 10,
                        unit: None,
                    },
                ))),
                properties: Default::default(),
                json_fragment: false,
                collation: Some(Ok(Label::Implicit(collation.into()))),
            })
            .collect(),
        );
        let reference: serde_json::Value =
            serde_json::from_str(include_str!("../../../reference/bin2-comparisons.json")).unwrap();
        let mut counts = (0, 0);
        for case in reference["results"].as_array().unwrap() {
            let mut query = query(case["query"].as_str().unwrap());
            let before = query.clone();
            let result = lower_bin2_comparisons(&catalog, &mut query, &Scope::default());
            let errors = case["reference"]["errors"].as_array().unwrap();
            if let Some(expected) = errors.first() {
                let error = result.unwrap_err();
                assert_eq!(error.number, expected["number"].as_i64().unwrap() as i32);
                assert_eq!(error.state, expected["state"].as_u64().unwrap() as u8);
                assert_eq!(error.message, expected["message"].as_str().unwrap());
                assert_eq!(query, before);
                counts.0 += 1;
            } else {
                result.unwrap();
                assert_eq!(
                    query.to_string().matches("__msduck_bin2_compare").count(),
                    12,
                    "{}",
                    case["name"]
                );
                assert!(!query.to_string().contains("COLLATE"));
                let once = query.clone();
                lower_bin2_comparisons(&catalog, &mut query, &Scope::default()).unwrap();
                assert_eq!(query, once);
                counts.1 += 1;
            }
        }
        assert_eq!(counts, (16, 5));
        let ansi: serde_json::Value = serde_json::from_str(include_str!(
            "../../../reference/bin2-ansi-comparisons.json"
        ))
        .unwrap();
        for case in ansi["results"].as_array().unwrap() {
            let mut query = query(case["query"].as_str().unwrap());
            lower_bin2_comparisons(&catalog, &mut query, &Scope::default()).unwrap();
            let name = if case["name"] == "mixed unicode" {
                "__msduck_bin2_compare"
            } else {
                "__msduck_bin2_ansi_compare"
            };
            assert_eq!(
                query.to_string().matches(name).count(),
                12,
                "{}",
                case["name"]
            );
            assert!(!query.to_string().contains("COLLATE"));
            let once = query.clone();
            lower_bin2_comparisons(&catalog, &mut query, &Scope::default()).unwrap();
            assert_eq!(query, once);
        }
    }

    #[test]
    fn sensitive_operations_validate_all_query_expressions_with_lexical_scopes() {
        use msduck_core::collation::Label;
        let mut catalog = catalog();
        catalog.default_collation = Some("SQL_Latin1_General_CP1_CI_AS".into());
        catalog.tables.insert(
            "coll_scope".into(),
            [
                ("a", "Latin1_General_100_CI_AS"),
                ("b", "Latin1_General_100_CS_AS"),
            ]
            .into_iter()
            .map(|(name, collation)| Field {
                name: name.into(),
                info: None,
                properties: Default::default(),
                json_fragment: false,
                collation: Some(Ok(Label::Implicit(collation.into()))),
            })
            .collect(),
        );
        for sql in [
            "SELECT 1 FROM coll_scope WHERE a=b",
            "SELECT 1 FROM coll_scope WHERE REPLACE(a,b,N'z') IS NULL",
            "SELECT unknown_wrapper(REPLACE(a,b,N'z')) FROM coll_scope",
            "SELECT COUNT(*) FROM coll_scope GROUP BY REPLACE(a,b,N'z')",
            "SELECT a FROM coll_scope ORDER BY REPLACE(a,b,N'z')",
            "SELECT (SELECT REPLACE(t.a,t.b,N'z')) FROM coll_scope t",
            "WITH c AS (SELECT a,b FROM coll_scope) SELECT 1 FROM c WHERE a=b",
            "SELECT 1 FROM (SELECT a,b FROM coll_scope) d WHERE a=b",
            "SELECT N'ok' UNION ALL SELECT REPLACE(a,b,N'z') FROM coll_scope",
        ] {
            let error =
                validate_query_operations(&catalog, &query(sql), &Scope::default()).unwrap_err();
            assert_eq!((error.number, error.state), (468, 9), "{sql}: {error}");
        }
        for sql in [
            "SELECT REPLACE(a,b,N'z' COLLATE Latin1_General_100_BIN2) FROM coll_scope",
            "SELECT 1 FROM coll_scope WHERE a=b COLLATE Latin1_General_100_BIN2",
            "SELECT (CASE WHEN 1=1 THEN a ELSE b END) COLLATE Latin1_General_100_BIN2 FROM coll_scope",
            "SELECT * FROM coll_scope t, (SELECT REPLACE(t.a,t.b,N'z') AS s) d",
            "SELECT (WITH c AS (SELECT REPLACE(t.a,t.b,N'z') AS s) SELECT s FROM c) FROM coll_scope t",
            "SELECT (SELECT REPLACE(a,b,N'z') FROM unknown_table) FROM coll_scope",
        ] {
            assert!(
                validate_query_operations(&catalog, &query(sql), &Scope::default()).is_ok(),
                "{sql}"
            );
        }
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../../../reference/collation-join-scopes.json"
        ))
        .unwrap();
        let cases = reference["results"].as_array().unwrap();
        assert_eq!(cases.len(), 9);
        for case in cases {
            let sql = case["query"].as_str().unwrap();
            let actual = validate_query_operations(&catalog, &query(sql), &Scope::default());
            let expected = &case["reference"]["errors"][0];
            if expected["number"] == 468 {
                let error = actual.unwrap_err();
                assert_eq!(error.number, 468, "{sql}");
                assert_eq!(error.state, expected["state"].as_u64().unwrap() as u8);
                assert_eq!(error.message, expected["message"].as_str().unwrap());
            } else {
                // Missing identifiers are rejected by name binding, not by
                // borrowing a collation from an out-of-scope row source.
                assert!(actual.is_ok(), "{sql}: {actual:?}");
            }
        }
    }

    #[test]
    fn character_function_labels_match_live_reference_properties_and_precedence() {
        use msduck_core::collation::{Conflict, Label};
        let mut catalog = catalog();
        catalog.default_collation = Some("SQL_Latin1_General_CP1_CI_AS".into());
        catalog.tables.insert(
            "coll_scope".into(),
            [
                ("a", "Latin1_General_100_CI_AS"),
                ("b", "Latin1_General_100_CS_AS"),
            ]
            .into_iter()
            .map(|(name, collation)| Field {
                name: name.into(),
                info: catalog.cast_info(&DataType::Nvarchar(Some(
                    CharacterLength::IntegerLength {
                        length: 10,
                        unit: None,
                    },
                ))),
                collation: Some(Ok(Label::Implicit(collation.into()))),
                json_fragment: false,
                properties: Default::default(),
            })
            .collect(),
        );
        let reference: serde_json::Value =
            serde_json::from_str(include_str!("../../../reference/collation-functions.json"))
                .unwrap();
        let sensitive: serde_json::Value =
            serde_json::from_str(include_str!("../../../reference/collation-sensitive.json"))
                .unwrap();
        let cases = reference["results"]
            .as_array()
            .unwrap()
            .iter()
            .chain(sensitive["results"].as_array().unwrap())
            .collect::<Vec<_>>();
        let mut matched = 0;
        let mut diagnosed = 0;
        for case in cases
            .iter()
            .filter(|c| c["name"].as_str().unwrap().ends_with(" property"))
        {
            let expression = case["expression"].as_str().unwrap();
            let fields = query_fields(
                &catalog,
                &query(&format!("SELECT {expression} AS s FROM coll_scope")),
                &Scope::default(),
            )
            .unwrap();
            let label = fields[0].collation.as_ref();
            if let Some(Err(Conflict::Operation(error))) = label {
                let expected = &case["reference"]["errors"][0];
                assert_eq!(
                    error.number,
                    expected["number"].as_i64().unwrap() as i32,
                    "{expression}"
                );
                assert_eq!(error.state, expected["state"].as_u64().unwrap() as u8);
                assert_eq!(error.severity, expected["class"].as_u64().unwrap() as u8);
                assert_eq!(error.message, expected["message"].as_str().unwrap());
                let outer = query_fields(
                    &catalog,
                    &query(&format!(
                        "SELECT ({expression}) COLLATE Latin1_General_100_BIN2 AS s FROM coll_scope"
                    )),
                    &Scope::default(),
                )
                .unwrap();
                assert_eq!(outer[0].collation.as_ref(), label);
                diagnosed += 1;
                continue;
            }
            matched += 1;
            let label = label
                .unwrap_or_else(|| panic!("unknown collation: {expression}"))
                .as_ref()
                .unwrap();
            if case["reference"]["errors"].as_array().unwrap().is_empty() {
                assert_eq!(
                    label.name(),
                    case["reference"]["sets"][0]["rows"][0][0].as_str(),
                    "{expression}"
                );
            } else {
                assert!(
                    matches!(label, Label::NoCollation { .. }),
                    "{expression}: {label:?}"
                );
                assert_eq!(case["reference"]["errors"][0]["number"], 456);
            }
            let comparison_name = case["name"]
                .as_str()
                .unwrap()
                .replace(" property", " comparison");
            let comparison = cases.iter().find(|c| c["name"] == comparison_name).unwrap();
            match label.combine(&Label::Explicit("Latin1_General_100_CS_AS".into())) {
                Err(Conflict::Explicit { .. }) => assert_eq!(
                    comparison["reference"]["errors"][0]["number"], 468,
                    "{expression}"
                ),
                Ok(_) => assert!(
                    comparison["reference"]["errors"]
                        .as_array()
                        .unwrap()
                        .is_empty(),
                    "{expression}"
                ),
                result => panic!("unexpected resolution for {expression}: {result:?}"),
            }
        }
        assert_eq!((matched, diagnosed), (30, 9));
    }

    #[test]
    fn collation_labels_survive_derived_cte_case_and_set_boundaries() {
        use msduck_core::collation::{Conflict, Label};
        let mut catalog = catalog();
        let ci = "Latin1_General_100_CI_AS";
        let cs = "Latin1_General_100_CS_AS";
        let bin = "Latin1_General_100_BIN2";
        let default = "SQL_Latin1_General_CP1_CI_AS";
        catalog.default_collation = Some(default.into());
        catalog.tables.insert(
            "coll_scope".into(),
            [("a", ci), ("b", cs)]
                .into_iter()
                .map(|(name, collation)| Field {
                    name: name.into(),
                    info: catalog.cast_info(&DataType::Nvarchar(Some(
                        CharacterLength::IntegerLength {
                            length: 10,
                            unit: None,
                        },
                    ))),
                    collation: Some(Ok(Label::Implicit(collation.into()))),
                    json_fragment: false,
                    properties: Default::default(),
                })
                .collect(),
        );
        let label = |sql: &str| {
            query_fields(&catalog, &query(sql), &Scope::default()).unwrap()[0]
                .collation
                .clone()
        };
        for sql in [
            "SELECT x FROM (SELECT N'a' COLLATE Latin1_General_100_BIN2 AS x) q",
            "WITH q AS (SELECT N'a' COLLATE Latin1_General_100_BIN2 AS x) SELECT x FROM q",
            "SELECT (CASE WHEN 1=1 THEN a ELSE b END) COLLATE Latin1_General_100_BIN2 FROM coll_scope",
            "SELECT s COLLATE Latin1_General_100_BIN2 FROM (SELECT a AS s FROM coll_scope UNION ALL SELECT b FROM coll_scope) q",
        ] {
            assert_eq!(label(sql), Some(Ok(Label::Explicit(bin.into()))), "{sql}");
        }
        assert_eq!(
            label("SELECT x FROM (SELECT N'a' AS x) q"),
            Some(Ok(Label::CoercibleDefault(default.into())))
        );
        assert_eq!(
            label("SELECT x FROM (SELECT a AS x FROM coll_scope) q"),
            Some(Ok(Label::Implicit(ci.into())))
        );
        for sql in [
            "SELECT CASE WHEN 1=1 THEN a ELSE b END FROM coll_scope",
            "SELECT a FROM coll_scope UNION ALL SELECT b FROM coll_scope",
        ] {
            assert_eq!(
                label(sql),
                Some(Ok(Label::NoCollation {
                    left: ci.into(),
                    right: cs.into()
                })),
                "{sql}"
            );
        }
        assert_eq!(
            label(
                "SELECT CASE WHEN 1=1 THEN N'a' COLLATE Latin1_General_100_BIN2 ELSE N'b' COLLATE Latin1_General_100_CS_AS END"
            ),
            Some(Err(Conflict::Explicit {
                left: bin.into(),
                right: cs.into()
            }))
        );
        assert_eq!(
            label("SELECT (N'a' COLLATE Latin1_General_100_BIN2) COLLATE Latin1_General_100_CS_AS"),
            Some(Ok(Label::Explicit(cs.into())))
        );
        assert_eq!(
            label("SELECT CAST(12 AS NVARCHAR(4))"),
            Some(Ok(Label::CoercibleDefault(default.into())))
        );
        assert_eq!(label("SELECT unknown_function(a) FROM coll_scope"), None);
        assert_eq!(label("SELECT 12"), None);
        let conflict = Err(Conflict::Explicit {
            left: bin.into(),
            right: cs.into(),
        });
        assert_eq!(
            combine_collations([None, Some(conflict.clone())].into_iter()),
            Some(conflict.clone())
        );
        assert_eq!(merge_collations(Some(&conflict), None), Some(conflict));
        assert_eq!(
            combine_collations([None, Some(Ok(Label::Implicit(ci.into())))].into_iter()),
            None
        );
    }

    use super::*;
    fn catalog() -> CatalogSnapshot {
        let mut catalog = CatalogSnapshot::default();
        for (name, id) in [
            ("nvarchar", 231),
            ("varchar", 167),
            ("nchar", 239),
            ("char", 175),
            ("time", 41),
            ("money", 60),
            ("int", 56),
        ] {
            catalog.types.insert(
                name.into(),
                Info {
                    system_type_id: Some(id),
                    user_type_id: Some(i32::from(id)),
                    ..Default::default()
                },
            );
        }
        catalog
    }
    fn query(sql: &str) -> Box<Query> {
        let Statement::Query(query) =
            sqlparser::parser::Parser::parse_sql(&crate::dialect::ServerDialect, sql)
                .unwrap()
                .remove(0)
        else {
            unreachable!()
        };
        query
    }
    #[test]
    fn numeric_conditionals_bind_columns_literals_and_unknowns() {
        let mut catalog = catalog();
        catalog.types.insert(
            "bigint".into(),
            Info {
                system_type_id: Some(127),
                user_type_id: Some(127),
                ..Default::default()
            },
        );
        for sql in [
            "SELECT CASE WHEN id=32 THEN 30 ELSE id END FROM (SELECT CAST(1 AS INT) AS id) t",
            "SELECT COALESCE(id,30) FROM (SELECT CAST(1 AS INT) AS id) t",
        ] {
            let fields = query_fields(&catalog, &query(sql), &Scope::default()).unwrap();
            assert_eq!(
                fields[0].info.as_ref().and_then(|info| info.logical_type()),
                Some(msduck_core::types::Type::Int)
            );
        }
        let fields = query_fields(
            &catalog,
            &query("SELECT CASE WHEN 1=1 THEN CAST(1 AS INT) ELSE CAST(2 AS BIGINT) END"),
            &Scope::default(),
        )
        .unwrap();
        assert_eq!(
            fields[0].info.as_ref().and_then(|info| info.logical_type()),
            Some(msduck_core::types::Type::BigInt)
        );
        let fields = query_fields(
            &catalog,
            &query("SELECT CASE WHEN 1=1 THEN 30 ELSE unknown_column END"),
            &Scope::default(),
        )
        .unwrap();
        assert!(fields[0].info.is_none());
    }
    #[test]
    fn result_labels_follow_sql_expressions_stars_and_left_set_members() {
        let catalog = catalog();
        for (sql, expected) in [
            (
                "SELECT 1,@@ROWCOUNT,1+2,(SELECT 1),1 AS named",
                vec!["", "", "", "", "named"],
            ),
            ("SELECT 1 UNION ALL SELECT 2 AS other", vec![""]),
            (
                "SELECT 1 AS first_name UNION ALL SELECT 2 AS other",
                vec!["first_name"],
            ),
            (
                "SELECT n,N,(n),+n,n+0,s.n,(s.n) FROM (VALUES(1)) s(n)",
                vec!["n", "N", "n", "n", "", "n", "n"],
            ),
            (
                "SELECT *,1,s.*,1+2 AS total FROM (VALUES(1,2)) s(a,b)",
                vec!["a", "b", "", "a", "b", "total"],
            ),
        ] {
            let ast = query(sql);
            let before = ast.clone();
            let fields = query_fields(&catalog, &ast, &Scope::default()).unwrap();
            assert_eq!(
                fields
                    .iter()
                    .map(|field| field.name.as_str())
                    .collect::<Vec<_>>(),
                expected,
                "{sql}"
            );
            assert_eq!(ast, before);
        }
    }

    #[test]
    fn arithmetic_projection_uses_parameter_declarations_without_values() {
        let catalog = catalog();
        let mut scope = Scope::default();
        scope.parameters.insert(
            "@p".into(),
            catalog.cast_info(&DataType::Int(None)).unwrap(),
        );
        for sql in [
            "SELECT 10/@p",
            "SELECT (@p+1)*2",
            "WITH q AS (SELECT 10/@p AS n) SELECT n FROM q",
        ] {
            let fields = query_fields(&catalog, &query(sql), &scope).unwrap();
            assert_eq!(
                fields[0].info.as_ref().unwrap().system_type_id,
                Some(56),
                "{sql}"
            );
        }
        let fields = query_fields(&catalog, &query("SELECT 10/@unknown"), &scope).unwrap();
        assert!(fields[0].info.is_none());
    }

    #[test]
    fn parameter_declarations_survive_cte_boundaries_without_runtime_values() {
        let catalog = catalog();
        let mut scope = Scope::default();
        let declaration = catalog
            .cast_info(&DataType::Varchar(Some(CharacterLength::Max)))
            .unwrap();
        scope.parameters.insert("@v".into(), declaration.clone());
        for sql in [
            "SELECT @V AS value",
            "WITH q AS (SELECT @V AS value) SELECT value FROM q",
            "SELECT (SELECT @V) AS value",
        ] {
            let ast = query(sql);
            let before = ast.to_string();
            let fields = query_fields(&catalog, &ast, &scope).unwrap();
            assert_eq!(fields[0].info.as_ref().unwrap(), &declaration);
            assert_eq!(ast.to_string(), before);
        }
        let fields =
            query_fields(&catalog, &query("SELECT COALESCE(@V,'fallback')"), &scope).unwrap();
        assert_eq!(
            fields[0].properties,
            msduck_core::result::Properties::expression(true)
        );
        assert_eq!(scope.parameters["@v"], declaration);
    }

    #[test]
    fn scalar_queries_inherit_ctes_and_rows_without_guessing_through_shadowing() {
        let catalog = catalog();
        let fields = query_fields(&catalog, &query("WITH q AS (SELECT CAST(12 AS MONEY) AS m) SELECT (SELECT m FROM q) AS a, (SELECT (SELECT p.m)) AS b, COALESCE((SELECT p.m),'$2') AS c FROM q p"), &Scope::default()).unwrap();
        for field in fields {
            assert_eq!(field.info.unwrap().system_type_id, Some(60));
        }
        for sql in [
            "SELECT (SELECT p.m FROM (SELECT unknown_value AS m) p) FROM (SELECT CAST(12 AS MONEY) AS m) p",
            "SELECT (SELECT CAST(1 AS MONEY),CAST(2 AS MONEY))",
        ] {
            let fields = query_fields(&catalog, &query(sql), &Scope::default()).unwrap();
            assert!(fields[0].info.is_none());
        }
    }
    #[test]
    fn nullif_retains_first_declaration_including_literal_and_column_widths() {
        let catalog = catalog();
        let fields = query_fields(&catalog, &query("SELECT NULLIF('$13',CAST(12 AS MONEY)) AS literal_text, NULLIF(t,CAST(12 AS MONEY)) AS column_text, NULLIF(CAST(NULL AS MONEY),CAST(1 AS DECIMAL(30,5))) AS money FROM (SELECT CAST(NULL AS VARCHAR(20)) AS t) s"), &Scope::default()).unwrap();
        let literal = fields[0].info.as_ref().unwrap();
        assert_eq!(literal.system_type_id, Some(167));
        assert_eq!(literal.max_length, Some(3));
        let column = fields[1].info.as_ref().unwrap();
        assert_eq!(column.system_type_id, Some(167));
        assert_eq!(column.max_length, Some(20));
        assert_eq!(fields[2].info.as_ref().unwrap().system_type_id, Some(60));
    }
    #[test]
    fn recursive_anchors_bind_self_and_survive_native_lowering() {
        let mut catalog = catalog();
        catalog.tables.insert(
            "r".into(),
            vec![Field {
                collation: None,
                name: "wrong".into(),
                info: None,
                properties: Default::default(),
                json_fragment: false,
            }],
        );
        let mut query = query(
            "WITH r(s,n) AS (SELECT CAST('a' AS VARCHAR(5)),1 UNION ALL SELECT CAST(s+'b' AS VARCHAR(5)),n+1 FROM r WHERE n<3), next_cte(label) AS (SELECT s FROM r) SELECT * FROM next_cte",
        );
        let before = query.clone();
        let bound = scopes(&catalog, &query, &Scope::default());
        assert_eq!(bound.definitions[0]["r"][0].name, "s");
        assert_eq!(
            bound.definitions[0]["r"][0]
                .info
                .as_ref()
                .unwrap()
                .max_length,
            Some(5)
        );
        let fields = query_fields(&catalog, &query, &Scope::default()).unwrap();
        assert_eq!(fields[0].name, "label");
        assert_eq!(fields[0].info.as_ref().unwrap().system_type_id, Some(167));
        assert_eq!(query, before);
        crate::recursive_lower::lower(&mut query, &catalog).unwrap();
        let after = query_fields(&catalog, &query, &Scope::default()).unwrap();
        assert_eq!(after.len(), fields.len());
        assert_eq!(after[0].name, fields[0].name);
        assert_eq!(after[0].info, fields[0].info);
        let inner = &query.with.as_ref().unwrap().cte_tables[0].query;
        assert!(inner.with.as_ref().unwrap().recursive);
        assert_eq!(
            scopes(&catalog, inner, &Scope::default()).definitions[0]["r"][0]
                .info
                .as_ref()
                .unwrap()
                .max_length,
            Some(5)
        );
    }

    #[test]
    fn ctes_derived_sources_and_expression_rules_bind_without_a_backend() {
        let catalog = catalog();
        let query = query(
            "WITH a AS (SELECT CAST(NULL AS TIME(2)) AS t,CAST(N'ab' AS NVARCHAR(12)) AS n), b(clock,label) AS (SELECT t,n FROM a) SELECT ISNULL(clock,CAST(NULL AS TIME(5))) AS clock,UPPER(label) AS label,COALESCE(clock,CAST(NULL AS TIME(4))) AS wider FROM b",
        );
        let fields = query_fields(&catalog, &query, &Scope::default()).unwrap();
        assert_eq!(
            fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
            ["clock", "label", "wider"]
        );
        assert_eq!(fields[0].info.as_ref().unwrap().time_scale(), Some(2));
        assert_eq!(fields[1].info.as_ref().unwrap().max_length, Some(24));
        assert_eq!(fields[2].info.as_ref().unwrap().time_scale(), Some(4));
        let json = query_fields(
            &catalog,
            &self::query("SELECT j.* FROM OPENJSON(N'{}') WITH (n NVARCHAR, t TIME(3)) j"),
            &Scope::default(),
        )
        .unwrap();
        assert_eq!(json[0].info.as_ref().unwrap().max_length, Some(2));
        assert_eq!(json[1].info.as_ref().unwrap().time_scale(), Some(3));
        let values = query_fields(
            &catalog,
            &self::query(
                "SELECT d.* FROM (VALUES (CAST(NULL AS TIME(2))),(CAST(NULL AS TIME(5)))) d(t)",
            ),
            &Scope::default(),
        )
        .unwrap();
        assert_eq!(values[0].info.as_ref().unwrap().time_scale(), Some(5));
    }
    #[test]
    fn outer_metadata_never_overrides_local_unknown_names_or_cte_boundaries() {
        let catalog = catalog();
        let parent = query("SELECT * FROM (SELECT CAST(1 AS MONEY) AS price) p");
        let scope = scopes(&catalog, &parent, &Scope::default()).body;
        let fields = query_fields(&catalog, &query("SELECT p.price"), &scope).unwrap();
        assert_eq!(fields[0].info.as_ref().unwrap().system_type_id, Some(60));
        for sql in [
            "SELECT price FROM (SELECT 1 AS price) q",
            "SELECT p.price FROM (SELECT 1 AS other) p",
            "SELECT price FROM (SELECT 1 AS price) q CROSS JOIN (SELECT 2 AS price) r",
            "WITH c AS (SELECT p.price AS v) SELECT * FROM c",
        ] {
            assert!(
                query_fields(&catalog, &query(sql), &scope).unwrap()[0]
                    .info
                    .is_none(),
                "{sql}"
            );
        }
        assert!(query_fields(&catalog, &query("SELECT * FROM unknown_table"), &scope).is_none());
    }
}

#[cfg(test)]
mod fragment_tests {
    use super::*;
    fn fields(sql: &str, scope: &Scope) -> Vec<Field> {
        let Statement::Query(query) =
            sqlparser::parser::Parser::parse_sql(&crate::dialect::ServerDialect, sql)
                .unwrap()
                .remove(0)
        else {
            unreachable!()
        };
        query_fields(&CatalogSnapshot::default(), &query, scope).unwrap()
    }
    #[test]
    fn fragments_follow_columns_without_promoting_text_or_losing_shadowing() {
        let empty = Scope::default();
        let result = fields(
            "WITH a(j,t) AS (SELECT JSON_QUERY(N'{}'),N'{}'),b(fragment,text) AS (SELECT * FROM a) SELECT d.* FROM (SELECT * FROM b) d",
            &empty,
        );
        assert_eq!(
            result.iter().map(|f| f.json_fragment).collect::<Vec<_>>(),
            [true, false]
        );
        let result = fields(
            "SELECT (SELECT 1 AS n FOR JSON PATH) AS a,(SELECT 1 AS n FOR JSON PATH,WITHOUT_ARRAY_WRAPPER) AS b,JSON_QUERY((SELECT 1 AS n FOR JSON PATH,WITHOUT_ARRAY_WRAPPER)) AS c",
            &empty,
        );
        assert_eq!(
            result.iter().map(|f| f.json_fragment).collect::<Vec<_>>(),
            [true, false, true]
        );
        let mut outer = Scope::default();
        outer.rows.push(Some(vec![Source {
            qualifiers: vec!["p".into()],
            fields: fields("SELECT JSON_QUERY(N'{}') AS payload", &empty),
        }]));
        assert!(fields("SELECT p.payload", &outer)[0].json_fragment);
        assert!(
            !fields("SELECT payload FROM (SELECT N'{}' AS payload) q", &outer)[0].json_fragment
        );
        assert!(
            !fields("SELECT p.payload FROM (SELECT N'{}' AS other) p", &outer)[0].json_fragment
        );
        assert!(!fields("SELECT payload FROM (SELECT JSON_QUERY(N'{}') AS payload) a CROSS JOIN (SELECT JSON_QUERY(N'{}') AS payload) b", &outer)[0].json_fragment);
        outer.rows.push(None);
        assert!(!fields("SELECT p.payload", &outer)[0].json_fragment);
    }
}

#[cfg(test)]
mod qualified_star_tests {
    use super::*;
    fn query(sql: &str) -> Box<Query> {
        let Statement::Query(query) =
            sqlparser::parser::Parser::parse_sql(&crate::dialect::ServerDialect, sql)
                .unwrap()
                .remove(0)
        else {
            unreachable!()
        };
        query
    }
    #[test]
    fn qualified_stars_follow_nearest_scope_and_expand_without_losing_qualifiers() {
        let catalog = CatalogSnapshot::default();
        let parent =
            query("SELECT * FROM (SELECT 1 AS [space name],JSON_QUERY(N'{}') AS fragment) p");
        let outer = scopes(&catalog, &parent, &Scope::default()).body;
        let mut child = query("SELECT p.*,9 AS tail");
        let fields = query_fields(&catalog, &child, &outer).unwrap();
        assert_eq!(
            fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
            ["space name", "fragment", "tail"]
        );
        assert!(fields[1].json_fragment);
        expand_qualified_stars(&catalog, &mut child, &outer).unwrap();
        assert_eq!(
            child.to_string(),
            "SELECT p.\"space name\", p.\"fragment\", 9 AS tail"
        );
        let local = query("SELECT p.* FROM (SELECT 2 AS local_name) p");
        assert_eq!(
            query_fields(&catalog, &local, &outer).unwrap()[0].name,
            "local_name"
        );
        let ambiguous = query("SELECT p.* FROM (SELECT 1 AS a) p CROSS JOIN (SELECT 2 AS b) p");
        assert!(query_fields(&catalog, &ambiguous, &outer).is_none());
        let mut unknown = outer.clone();
        unknown.rows.push(None);
        let mut child = query("SELECT p.*");
        let unchanged = child.clone();
        assert!(query_fields(&catalog, &child, &unknown).is_none());
        assert!(expand_qualified_stars(&catalog, &mut child, &unknown).is_none());
        assert_eq!(child, unchanged);
        let cte = query("WITH c AS (SELECT p.*) SELECT * FROM c");
        assert!(query_fields(&catalog, &cte, &outer).is_none());
    }
}

#[cfg(test)]
mod cte_visibility_tests {
    use super::*;
    fn query(sql: &str) -> Box<Query> {
        let Statement::Query(query) =
            sqlparser::parser::Parser::parse_sql(&crate::dialect::ServerDialect, sql)
                .unwrap()
                .remove(0)
        else {
            unreachable!()
        };
        query
    }
    #[test]
    fn projection_and_visitor_scopes_agree_on_unresolved_cte_shadowing() {
        let mut catalog = CatalogSnapshot::default();
        for name in ["future", "self_cte"] {
            catalog.tables.insert(
                name.into(),
                vec![Field {
                    collation: None,
                    name: "wrong_base_column".into(),
                    info: None,
                    properties: Default::default(),
                    json_fragment: false,
                }],
            );
        }
        for sql in [
            "WITH early AS (SELECT * FROM future),future AS (SELECT 1 AS right_column) SELECT * FROM early",
            "WITH self_cte AS (SELECT * FROM self_cte) SELECT * FROM self_cte",
        ] {
            let query = query(sql);
            assert!(
                query_fields(&catalog, &query, &Scope::default()).is_none(),
                "{sql}"
            );
            let bound = scopes(&catalog, &query, &Scope::default());
            assert!(bound.body.rows.last().unwrap().is_none(), "{sql}");
            let mut outer = Scope::default();
            outer.insert("future".into(), catalog.tables["future"].clone());
            outer.insert("self_cte".into(), catalog.tables["self_cte"].clone());
            assert!(query_fields(&catalog, &query, &outer).is_none(), "{sql}");
        }
        catalog
            .tables
            .insert("dbo.future".into(), catalog.tables["future"].clone());
        let explicit_base = query(
            "WITH early AS (SELECT * FROM dbo.future),future AS (SELECT 1 AS right_column) SELECT * FROM early",
        );
        assert_eq!(
            query_fields(&catalog, &explicit_base, &Scope::default()).unwrap()[0].name,
            "wrong_base_column"
        );
        let chain = query(
            "WITH a(j) AS (SELECT JSON_QUERY(N'{}')),b(payload) AS (SELECT j FROM a) SELECT * FROM b",
        );
        let fields = query_fields(&catalog, &chain, &Scope::default()).unwrap();
        assert_eq!(fields[0].name, "payload");
        assert!(fields[0].json_fragment);
        let bound = scopes(&catalog, &chain, &Scope::default());
        assert!(bound.body["b"][0].json_fragment);
    }
}
