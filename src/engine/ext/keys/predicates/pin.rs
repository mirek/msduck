//! Carrier columns among the alternatives of ISNULL, COALESCE, IIF, CASE
//! and set operations.
//!
//! DuckDB gives such an expression one type, and cannot unify a carrier
//! STRUCT with VARCHAR text (a literal, parameter or VARCHAR column). When
//! the alternatives mix a carrier column with other values, each carrier
//! column becomes `CAST(column AS <its declaration>)`, which keeps the
//! logical type (and so the result metadata) while the backend conversion
//! produces text ([`super::lower`]). Proved character-only alternatives with
//! direct OPENJSON sources instead preserve all branches as Unicode carriers,
//! using their common declaration without changing non-character precedence.
use super::catalog::Catalog;
use msduck_sql::parameter::Parameter;
use sqlparser::ast::*;
use std::collections::HashMap;
use std::ops::ControlFlow;

fn null(expr: &Expr) -> bool {
    match expr {
        Expr::Nested(inner) => null(inner),
        Expr::Value(value) => matches!(value.value, Value::Null),
        _ => false,
    }
}

/// Whether `values` mix a carrier column with other (non-NULL) values.
fn mixed(catalog: &Catalog, values: &[&Expr]) -> bool {
    let carriers: Vec<bool> = values
        .iter()
        .map(|v| catalog.carrier(v).is_some())
        .collect();
    carriers.iter().any(|c| *c) && values.iter().zip(&carriers).any(|(v, c)| !c && !null(v))
}

/// OPENJSON carrier provenance survives the explicit wrappers this pass adds.
/// Other producers, casts to ANSI and query outputs remain unresolved.
fn openjson_carrier(catalog: &Catalog, value: &Expr) -> bool {
    fn at(catalog: &Catalog, value: &Expr, remaining: usize) -> bool {
        if remaining == 0 {
            return false;
        }
        if catalog.carrier(value).is_some_and(|column| column.openjson) {
            return true;
        }
        match value {
            Expr::Nested(inner) => at(catalog, inner, remaining - 1),
            Expr::Cast {
                expr, data_type, ..
            } if matches!(msduck_sql::sql_type::declaration(data_type),
                Ok(msduck_core::types::Type::Character(character)) if matches!(character.family(), msduck_core::character::Family::Nchar | msduck_core::character::Family::Nvarchar)) =>
            {
                at(catalog, expr, remaining - 1)
            }
            Expr::Function(f)
                if f.name
                    .to_string()
                    .eq_ignore_ascii_case("__msduck_carrier_input") =>
            {
                let FunctionArguments::List(args) = &f.args else {
                    return false;
                };
                matches!(args.args.as_slice(), [FunctionArg::Unnamed(FunctionArgExpr::Expr(inner))] if at(catalog, inner, remaining - 1))
            }
            value if msduck_sql::expression_metadata::conditional::candidate(value) => {
                msduck_sql::expression_metadata::conditional::values(value)
                    .into_iter()
                    .any(|inner| at(catalog, inner, remaining - 1))
            }
            _ => false,
        }
    }
    at(catalog, value, 64)
}

fn expression_kind(
    catalog: &Catalog,
    value: &Expr,
    parameters: &HashMap<String, Parameter>,
) -> Option<DataType> {
    if let Expr::Nested(inner) | Expr::Collate { expr: inner, .. } = value {
        return expression_kind(catalog, inner, parameters);
    }
    if let Expr::Value(value) = value
        && let Value::SingleQuotedString(text) = &value.value
    {
        // The supported non-SC CP1252 collations convert each UTF-16 unit
        // independently. A supplementary ANSI literal therefore occupies two
        // bytes ("??"), before promotion to Unicode.
        let units = text.encode_utf16().count();
        return Some(DataType::Varchar(Some(if units > 8000 {
            CharacterLength::Max
        } else {
            CharacterLength::IntegerLength {
                length: units.max(1) as u64,
                unit: None,
            }
        })));
    }
    msduck_sql::expression_metadata::storage::kind(value, parameters, &|expr| {
        catalog.declaration(expr)
    })
}

/// Prove a character-only common result from declarations, never values.
/// Other type precedence and unresolved alternatives retain the legacy path.
fn openjson_character_result(
    catalog: &Catalog,
    values: &[&Expr],
    parameters: &HashMap<String, Parameter>,
) -> Option<DataType> {
    if !values.iter().any(|value| openjson_carrier(catalog, value)) {
        return None;
    }
    let kinds = values
        .iter()
        .filter(|value| !null(value))
        .map(|value| expression_kind(catalog, value, parameters))
        .collect::<Option<Vec<_>>>()?;
    common_unicode(kinds, parameters)
}

fn common_unicode(
    kinds: Vec<DataType>,
    parameters: &HashMap<String, Parameter>,
) -> Option<DataType> {
    let mut promoted = Vec::new();
    for kind in kinds {
        let msduck_core::types::Type::Character(mut character) =
            msduck_sql::sql_type::declaration(&kind).ok()?
        else {
            return None;
        };
        use msduck_core::character::{CharacterType, Family, Length};
        if matches!(character.family(), Family::Char | Family::Varchar) {
            character = CharacterType::new(
                if character.family() == Family::Char {
                    Family::Nchar
                } else {
                    Family::Nvarchar
                },
                match character.length() {
                    Length::Bounded(n) => Length::Bounded(n.min(4000)),
                    Length::Max => Length::Max,
                },
            )
            .ok()?;
        }
        promoted.push(msduck_sql::sql_type::ast(
            msduck_core::types::Type::Character(character),
        ));
    }
    let mut common = msduck_sql::expr::binary_function(
        "COALESCE",
        Expr::Value(Value::Null.into()),
        Expr::Value(Value::Null.into()),
    );
    if let Expr::Function(f) = &mut common
        && let FunctionArguments::List(args) = &mut f.args
    {
        args.args = promoted
            .into_iter()
            .map(|data_type| {
                FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Cast {
                    kind: CastKind::Cast,
                    expr: Box::new(Expr::Value(Value::Null.into())),
                    data_type,
                    format: None,
                }))
            })
            .collect();
    }
    let kind = msduck_sql::expression_metadata::storage::kind(&common, parameters, &|_| None)?;
    matches!(msduck_sql::sql_type::declaration(&kind).ok()?,
        msduck_core::types::Type::Character(character) if matches!(character.family(), msduck_core::character::Family::Nvarchar | msduck_core::character::Family::Nchar))
    .then_some(kind)
}

/// Pin the carrier columns among `values` when they mix with other values;
/// a dry run only reports whether any would be.
fn pin(
    catalog: &Catalog,
    values: Vec<&mut Expr>,
    apply: bool,
    parameters: &HashMap<String, Parameter>,
    preserve_character: bool,
) -> bool {
    let is_mixed = mixed(catalog, &values.iter().map(|v| &**v).collect::<Vec<_>>());
    let openjson =
        preserve_character && values.iter().any(|value| openjson_carrier(catalog, value));
    if !is_mixed && !openjson {
        return false;
    }
    if !apply {
        return true;
    }
    let common = preserve_character
        .then(|| {
            openjson_character_result(
                catalog,
                &values.iter().map(|value| &**value).collect::<Vec<_>>(),
                parameters,
            )
        })
        .flatten();
    if common.is_none() && !is_mixed {
        return false;
    }
    for value in values {
        if let Some(data_type) = &common {
            // Both carrier and text branches need the same backend family.
            // Each original branch occurs once; the conditional stays lazy.
            let source_kind = expression_kind(catalog, value, parameters);
            let mut original = std::mem::replace(value, Expr::Value(Value::Null.into()));
            if let Some(ansi) = source_kind.filter(|kind| matches!(msduck_sql::sql_type::declaration(kind),
                Ok(msduck_core::types::Type::Character(character)) if matches!(character.family(), msduck_core::character::Family::Char | msduck_core::character::Family::Varchar))) {
                original = Expr::Cast { kind: CastKind::Cast, expr: Box::new(original), data_type: ansi, format: None };
            }
            *value = Expr::Cast {
                kind: CastKind::Cast,
                expr: Box::new(msduck_sql::expr::unary_function(
                    "__msduck_carrier_input",
                    original,
                )),
                data_type: data_type.clone(),
                format: None,
            };
        } else if let Some(data_type) = catalog.carrier(value).and_then(|c| c.declared.clone()) {
            let column = std::mem::replace(value, Expr::Value(Value::Null.into()));
            *value = Expr::Cast {
                kind: CastKind::Cast,
                expr: Box::new(column),
                data_type,
                format: None,
            };
        }
    }
    true
}

fn arguments(function: &mut Function) -> Vec<&mut Expr> {
    let FunctionArguments::List(list) = &mut function.args else {
        return Vec::new();
    };
    list.args
        .iter_mut()
        .filter_map(|arg| match arg {
            FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => Some(e),
            _ => None,
        })
        .collect()
}

/// Whether `statement` has an expression this module may change.
pub(super) fn sites<T: Visit>(node: &T) -> bool {
    struct Find;
    impl Visitor for Find {
        type Break = ();
        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
            match expr {
                Expr::Case { .. } => ControlFlow::Break(()),
                Expr::Function(f)
                    if matches!(
                        f.name.to_string().to_ascii_uppercase().as_str(),
                        "ISNULL" | "COALESCE" | "IIF"
                    ) =>
                {
                    ControlFlow::Break(())
                }
                _ => ControlFlow::Continue(()),
            }
        }
        fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<()> {
            if matches!(query.body.as_ref(), SetExpr::SetOperation { .. }) {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        }
    }
    node.visit(&mut Find).is_break()
}

/// The projections of a set operation's branches.
fn projections(body: &SetExpr) -> Vec<&Select> {
    match body {
        SetExpr::Select(select) => vec![select],
        SetExpr::SetOperation { left, right, .. } => {
            let mut all = projections(left);
            all.extend(projections(right));
            all
        }
        SetExpr::Query(query) => projections(&query.body),
        _ => Vec::new(),
    }
}

fn scoped_projections<'a>(catalog: &Catalog, body: &'a SetExpr) -> Vec<(&'a Select, Catalog)> {
    match body {
        SetExpr::Select(select) => vec![(select, catalog.select_scope(select))],
        SetExpr::SetOperation { left, right, .. } => {
            let mut all = scoped_projections(catalog, left);
            all.extend(scoped_projections(catalog, right));
            all
        }
        SetExpr::Query(query) => scoped_projections(&catalog.query_scope(query), &query.body),
        _ => Vec::new(),
    }
}

fn character_sets(
    catalog: &Catalog,
    body: &SetExpr,
    parameters: &HashMap<String, Parameter>,
) -> Vec<(usize, DataType)> {
    let branches = scoped_projections(catalog, body);
    let width = branches
        .first()
        .map_or(0, |(select, _)| select.projection.len());
    if branches.len() < 2
        || branches
            .iter()
            .any(|(select, _)| select.projection.len() != width)
    {
        return Vec::new();
    }
    let migrated_kind = |expr: &Expr, scope: &Catalog| {
        if !msduck_sql::expression_metadata::conditional::candidate(expr)
            || matches!(expr, Expr::Function(f) if f.name.to_string().eq_ignore_ascii_case("ISNULL"))
        {
            return None;
        }
        openjson_character_result(
            scope,
            &msduck_sql::expression_metadata::conditional::values(expr),
            parameters,
        )
    };
    (0..width)
        .filter_map(|position| {
            let values = branches
                .iter()
                .map(|(select, scope)| {
                    item_expr(&select.projection[position]).map(|expr| (expr, scope))
                })
                .collect::<Option<Vec<_>>>()?;
            if !values
                .iter()
                .any(|(expr, scope)| migrated_kind(expr, scope).is_some())
            {
                return None;
            }
            let kinds = values
                .into_iter()
                .filter(|(expr, _)| !null(expr))
                .map(|(expr, scope)| {
                    migrated_kind(expr, scope).or_else(|| {
                        msduck_sql::expression_metadata::storage::kind(expr, parameters, &|value| {
                            scope.declaration(value)
                        })
                    })
                })
                .collect::<Option<Vec<_>>>()?;
            let common = common_unicode(kinds.clone(), parameters).or_else(|| {
                let mut numeric = kinds.iter().filter(|kind| {
                    !matches!(
                        msduck_sql::sql_type::declaration(kind),
                        Ok(msduck_core::types::Type::Character(_))
                    )
                });
                let first = numeric.next()?.clone();
                let first = msduck_sql::expression_metadata::arithmetic::set_type(&first, &first)?;
                numeric.try_fold(first, |left, right| {
                    msduck_sql::expression_metadata::arithmetic::set_type(&left, right)
                })
            })?;
            Some((position, common))
        })
        .collect()
}

fn align_character_sets(
    catalog: &Catalog,
    body: &mut SetExpr,
    plans: &[(usize, DataType)],
    parameters: &HashMap<String, Parameter>,
) {
    match body {
        SetExpr::Select(select) => {
            let scope = catalog.select_scope(select);
            for (position, kind) in plans {
                let Some(item) = select.projection.get_mut(*position) else {
                    continue;
                };
                let (original, alias) =
                    match std::mem::replace(item, SelectItem::Wildcard(Default::default())) {
                        SelectItem::UnnamedExpr(expr) => {
                            let alias = match &expr {
                                Expr::Identifier(name) => Some(name.clone()),
                                Expr::CompoundIdentifier(names) => names.last().cloned(),
                                _ => None,
                            };
                            (expr, alias)
                        }
                        SelectItem::ExprWithAlias { expr, alias } => (expr, Some(alias)),
                        other => {
                            *item = other;
                            continue;
                        }
                    };
                let character_target = matches!(
                    msduck_sql::sql_type::declaration(kind),
                    Ok(msduck_core::types::Type::Character(_))
                );
                let character_source =
                    msduck_sql::expression_metadata::storage::kind(
                        &original,
                        parameters,
                        &|value| scope.declaration(value),
                    )
                    .is_some_and(|kind| {
                        matches!(
                            msduck_sql::sql_type::declaration(&kind),
                            Ok(msduck_core::types::Type::Character(_))
                        )
                    }) || msduck_sql::expression_metadata::conditional::candidate(&original)
                        && openjson_character_result(
                            &scope,
                            &msduck_sql::expression_metadata::conditional::values(&original),
                            parameters,
                        )
                        .is_some();
                let source = if character_target || character_source {
                    let packed =
                        msduck_sql::expr::unary_function("__msduck_carrier_input", original);
                    if character_target {
                        packed
                    } else {
                        msduck_sql::expr::unary_function("__msduck_unicode_text", packed)
                    }
                } else {
                    original
                };
                let expr = Expr::Cast {
                    kind: CastKind::Cast,
                    expr: Box::new(source),
                    data_type: kind.clone(),
                    format: None,
                };
                *item = match alias {
                    Some(alias) => SelectItem::ExprWithAlias { expr, alias },
                    None => SelectItem::UnnamedExpr(expr),
                };
            }
        }
        SetExpr::SetOperation { left, right, .. } => {
            align_character_sets(catalog, left, plans, parameters);
            align_character_sets(catalog, right, plans, parameters);
        }
        SetExpr::Query(query) => align_character_sets(
            &catalog.query_scope(query),
            &mut query.body,
            plans,
            parameters,
        ),
        _ => {}
    }
}

fn item_expr(item: &SelectItem) -> Option<&Expr> {
    match item {
        SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => Some(expr),
        _ => None,
    }
}

/// Pin the carrier columns at `positions` of every branch, keeping the
/// output name of a bare column.
fn pin_branches(catalog: &Catalog, body: &mut SetExpr, positions: &[usize]) {
    match body {
        SetExpr::Select(select) => {
            let catalog = catalog.select_scope(select);
            for &position in positions {
                let Some(item) = select.projection.get_mut(position) else {
                    continue;
                };
                let Some(data_type) = item_expr(item)
                    .and_then(|e| catalog.carrier(e))
                    .and_then(|c| c.declared.clone())
                else {
                    continue;
                };
                let (expr, alias) =
                    match std::mem::replace(item, SelectItem::Wildcard(Default::default())) {
                        SelectItem::UnnamedExpr(expr) => {
                            let alias = match &expr {
                                Expr::Identifier(ident) => Some(ident.clone()),
                                Expr::CompoundIdentifier(parts) => parts.last().cloned(),
                                _ => None,
                            };
                            (expr, alias)
                        }
                        SelectItem::ExprWithAlias { expr, alias } => (expr, Some(alias)),
                        _ => unreachable!("a pinned item is an expression"),
                    };
                let expr = Expr::Cast {
                    kind: CastKind::Cast,
                    expr: Box::new(expr),
                    data_type,
                    format: None,
                };
                *item = match alias {
                    Some(alias) => SelectItem::ExprWithAlias { expr, alias },
                    None => SelectItem::UnnamedExpr(expr),
                };
            }
        }
        SetExpr::SetOperation { left, right, .. } => {
            pin_branches(catalog, left, positions);
            pin_branches(catalog, right, positions);
        }
        SetExpr::Query(query) => pin_branches(catalog, &mut query.body, positions),
        _ => {}
    }
}

fn character_equality(
    body: &mut SetExpr,
    names: &[String],
    equality: &[msduck_sql::variant_sets::Equality],
) {
    let SetExpr::SetOperation { left, right, .. } = body else {
        return;
    };
    character_equality(left, names, equality);
    character_equality(right, names, equality);
    let SetExpr::SetOperation {
        op, set_quantifier, ..
    } = body
    else {
        unreachable!()
    };
    if !matches!(
        set_quantifier,
        SetQuantifier::None | SetQuantifier::Distinct
    ) {
        return;
    }
    if *op == SetOperator::Union {
        *set_quantifier = SetQuantifier::All;
        msduck_sql::variant_sets::deduplicate(body, names, equality);
    } else {
        msduck_sql::variant_sets::membership(body, names, equality);
    }
}

/// Pin the mixed carrier columns of `node`, or with `apply` false only
/// report whether there are any (before their declarations are loaded).
pub(super) fn rewrite<T: VisitMut>(
    catalog: &Catalog,
    parameters: &HashMap<String, Parameter>,
    node: &mut T,
    apply: bool,
) -> bool {
    struct Pin<'a> {
        catalog: &'a Catalog,
        parameters: &'a HashMap<String, Parameter>,
        scopes: Vec<Catalog>,
        set_plans: Vec<Vec<(usize, DataType)>>,
        apply: bool,
        found: bool,
    }
    impl Pin<'_> {
        fn catalog(&self) -> &Catalog {
            self.scopes.last().unwrap_or(self.catalog)
        }
    }
    impl VisitorMut for Pin<'_> {
        type Break = ();
        fn pre_visit_query(&mut self, query: &mut Query) -> ControlFlow<()> {
            self.scopes.push(self.catalog().query_scope(query));
            self.set_plans.push(Vec::new());
            if !matches!(query.body.as_ref(), SetExpr::SetOperation { .. }) {
                return ControlFlow::Continue(());
            }
            let character_plans = character_sets(self.catalog(), &query.body, self.parameters);
            if !character_plans.is_empty() {
                self.found = true;
                if self.apply {
                    *self.set_plans.last_mut().unwrap() = character_plans.clone();
                    align_character_sets(
                        self.catalog(),
                        &mut query.body,
                        &character_plans,
                        self.parameters,
                    );
                }
            }
            let projections = projections(&query.body);
            let width = projections.first().map_or(0, |s| s.projection.len());
            if projections.iter().any(|s| s.projection.len() != width) {
                return ControlFlow::Continue(());
            }
            let positions: Vec<usize> = (0..width)
                .filter(|&position| {
                    let values: Option<Vec<(&Expr, bool)>> = projections
                        .iter()
                        .map(|select| {
                            let scope = self.catalog().select_scope(select);
                            item_expr(&select.projection[position])
                                .map(|value| (value, scope.carrier(value).is_some()))
                        })
                        .collect();
                    values.is_some_and(|values| {
                        values.iter().any(|(_, carrier)| *carrier)
                            && values
                                .iter()
                                .any(|(value, carrier)| !carrier && !null(value))
                    })
                })
                .collect();
            if !positions.is_empty() {
                self.found = true;
                if self.apply {
                    pin_branches(self.catalog(), &mut query.body, &positions);
                }
            }
            ControlFlow::Continue(())
        }
        fn post_visit_query(&mut self, query: &mut Query) -> ControlFlow<()> {
            let plans = self.set_plans.pop().unwrap();
            if self.apply
                && plans.iter().any(|(_, kind)| {
                    matches!(
                        msduck_sql::sql_type::declaration(kind),
                        Ok(msduck_core::types::Type::Character(_))
                    )
                })
            {
                let branches = projections(&query.body);
                if let Some(first) = branches.first() {
                    let names: Vec<String> = first
                        .projection
                        .iter()
                        .map(|item| match item {
                            SelectItem::ExprWithAlias { alias, .. } => alias.value.clone(),
                            SelectItem::UnnamedExpr(Expr::Identifier(name)) => name.value.clone(),
                            SelectItem::UnnamedExpr(Expr::CompoundIdentifier(names)) => names
                                .last()
                                .map(|name| name.value.clone())
                                .unwrap_or_default(),
                            _ => String::new(),
                        })
                        .collect();
                    let equality: Vec<_> = (0..names.len())
                        .map(|position| {
                            if plans.iter().any(|(p, kind)| {
                                *p == position
                                    && matches!(
                                        msduck_sql::sql_type::declaration(kind),
                                        Ok(msduck_core::types::Type::Character(_))
                                    )
                            }) {
                                msduck_sql::variant_sets::Equality::Unicode
                            } else {
                                msduck_sql::variant_sets::Equality::Native
                            }
                        })
                        .collect();
                    character_equality(&mut query.body, &names, &equality);
                }
            }
            self.scopes.pop();
            ControlFlow::Continue(())
        }
        fn pre_visit_select(&mut self, select: &mut Select) -> ControlFlow<()> {
            self.scopes.push(self.catalog().select_scope(select));
            ControlFlow::Continue(())
        }
        fn post_visit_select(&mut self, _: &mut Select) -> ControlFlow<()> {
            self.scopes.pop();
            ControlFlow::Continue(())
        }
        fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
            let found = match expr {
                Expr::Case {
                    conditions,
                    else_result,
                    ..
                } => {
                    let mut values: Vec<&mut Expr> =
                        conditions.iter_mut().map(|c| &mut c.result).collect();
                    if let Some(otherwise) = else_result {
                        values.push(otherwise);
                    }
                    pin(self.catalog(), values, self.apply, self.parameters, true)
                }
                Expr::Function(f) => match f.name.to_string().to_ascii_uppercase().as_str() {
                    "ISNULL" => {
                        let values = arguments(f);
                        // OPENJSON's ISNULL dispatch already packs replacements
                        // into the first carrier's type. Pinning it to text
                        // would lose unpaired units and change stored values.
                        if values.first().is_some_and(|first| {
                            self.catalog().carrier(first).is_some_and(|c| c.openjson)
                        }) {
                            false
                        } else {
                            pin(self.catalog(), values, self.apply, self.parameters, false)
                        }
                    }
                    "COALESCE" => pin(
                        self.catalog(),
                        arguments(f),
                        self.apply,
                        self.parameters,
                        true,
                    ),
                    "IIF" => pin(
                        self.catalog(),
                        arguments(f).into_iter().skip(1).collect(),
                        self.apply,
                        self.parameters,
                        true,
                    ),
                    _ => false,
                },
                _ => false,
            };
            self.found |= found;
            ControlFlow::Continue(())
        }
    }
    let mut pin = Pin {
        catalog,
        parameters,
        scopes: Vec::new(),
        set_plans: Vec::new(),
        apply,
        found: false,
    };
    let _ = node.visit(&mut pin);
    pin.found
}
