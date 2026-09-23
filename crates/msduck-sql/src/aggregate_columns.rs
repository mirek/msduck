//! Snapshot-based integer aggregate and DATETIME2 comparison annotations.
//! Resolve source columns within lexical query and DML scopes.
use sqlparser::ast::*;
use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    ops::ControlFlow,
};

#[derive(Default, Clone)]
struct Scope {
    columns: HashMap<Vec<String>, Option<DataType>>,
    unknown_source: bool,
    sources: Vec<(Vec<Vec<String>>, Columns)>,
}
impl Scope {
    fn validate_grouping(&self, select: &mut Select, outer: &[Scope]) -> Result<(), String> {
        struct Canonical<'a>(&'a Scope);
        impl VisitorMut for Canonical<'_> {
            type Break = ();
            fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
                if let Some((source, column)) = self.0.source_column(expr) {
                    *expr = Expr::Value(
                        Value::Placeholder(format!("group-column:{source}:{column}")).into(),
                    );
                } else if let Expr::Identifier(id) = expr {
                    id.value = id.value.to_lowercase();
                } else if let Expr::Nested(value) = expr {
                    *expr = *value.clone();
                } else if let Expr::Function(function) = expr {
                    for part in &mut function.name.0 {
                        if let ObjectNamePart::Identifier(id) = part {
                            id.value = id.value.to_lowercase();
                        }
                    }
                }
                ControlFlow::Continue(())
            }
        }
        let canonical = |value: &Expr| {
            let mut value = value.clone();
            let _ = VisitMut::visit(&mut value, &mut Canonical(self));
            value
        };
        crate::grouping::legacy(select, canonical)?;
        crate::grouping::sets(select)?;
        crate::grouping_syntax::columns(select, &|expr| !self.outer_reference(expr, outer))?;
        crate::grouping::columns(select, canonical)
    }
    fn outer_reference(&self, expr: &Expr, outer: &[Scope]) -> bool {
        let key = match expr {
            Expr::Identifier(id) => vec![id.value.to_lowercase()],
            Expr::CompoundIdentifier(ids) => ids.iter().map(|id| id.value.to_lowercase()).collect(),
            _ => return false,
        };
        let qualifier = &key[..key.len().saturating_sub(1)];
        // Local names, ambiguous bindings and unknown sources must be left to
        // the binder. A matching qualifier also shadows outer tables even if
        // the requested column is missing from that local table.
        let shadows = |scope: &Scope| {
            scope.unknown_source
                || scope.columns.contains_key(&key)
                || (!qualifier.is_empty()
                    && scope
                        .sources
                        .iter()
                        .any(|(names, _)| names.iter().any(|name| name == qualifier)))
        };
        if shadows(self) {
            return false;
        }
        for scope in outer.iter().rev() {
            if scope.unknown_source {
                return false;
            }
            if scope.source_column(expr).is_some() {
                return true;
            }
            if shadows(scope) {
                return false;
            }
        }
        false
    }
    fn source_column(&self, expr: &Expr) -> Option<(usize, usize)> {
        let ids = match expr {
            Expr::Identifier(id) if !id.value.starts_with('@') && !self.unknown_source => {
                vec![id.value.to_lowercase()]
            }
            Expr::CompoundIdentifier(ids) => ids.iter().map(|id| id.value.to_lowercase()).collect(),
            _ => return None,
        };
        let (name, qualifier) = ids.split_last()?;
        let mut found = None;
        for (source, (qualifiers, columns)) in self.sources.iter().enumerate() {
            if !qualifier.is_empty() && !qualifiers.iter().any(|q| q == qualifier) {
                continue;
            }
            for (column, (candidate, _)) in columns.iter().enumerate() {
                if candidate.eq_ignore_ascii_case(name) {
                    if found.is_some() {
                        return None;
                    }
                    found = Some((source, column));
                }
            }
        }
        found
    }
    fn projection_references(&self, select: &Select) -> Option<Vec<Expr>> {
        let mut result = vec![];
        for item in &select.projection {
            let sources = match item {
                SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => {
                    result.push(expr.clone());
                    continue;
                }
                SelectItem::Wildcard(options) if plain_star(options) && !self.unknown_source => {
                    self.sources.iter().collect::<Vec<_>>()
                }
                SelectItem::QualifiedWildcard(
                    SelectItemQualifiedWildcardKind::ObjectName(name),
                    options,
                ) if plain_star(options) => {
                    let qualifier = name
                        .0
                        .iter()
                        .map(|part| part.as_ident().map(|id| id.value.to_lowercase()))
                        .collect::<Option<Vec<_>>>()?;
                    let sources = self
                        .sources
                        .iter()
                        .filter(|(qualifiers, _)| qualifiers.contains(&qualifier))
                        .collect::<Vec<_>>();
                    if sources.len() != 1 {
                        return None;
                    }
                    sources
                }
                _ => return None,
            };
            for (qualifiers, columns) in sources {
                for (name, _) in columns {
                    let mut ids = qualifiers
                        .first()?
                        .iter()
                        .map(|name| Ident::with_quote('"', name))
                        .collect::<Vec<_>>();
                    ids.push(Ident::with_quote('"', name));
                    result.push(Expr::CompoundIdentifier(ids));
                }
            }
        }
        Some(result)
    }
    fn insert(&mut self, key: Vec<String>, kind: Option<DataType>) {
        self.columns
            .entry(key)
            .and_modify(|value| *value = None)
            .or_insert(kind);
    }
    fn column(&self, expr: &Expr) -> Option<DataType> {
        let key = match expr {
            Expr::Identifier(id) if !id.value.starts_with('@') && !self.unknown_source => {
                vec![id.value.to_lowercase()]
            }
            Expr::CompoundIdentifier(ids) => ids.iter().map(|id| id.value.to_lowercase()).collect(),
            _ => return None,
        };
        self.columns.get(&key)?.clone()
    }
}

mod alias_scope;
mod group_all;
mod variant_groups;

/// Owned declarations keyed by lowercase (schema, table); acquisition errors
/// are reported only when a base-table reference uses the entry (CTEs may shadow it).
pub type Snapshot = BTreeMap<(String, String), Result<Columns, String>>;

/// Explicit declarations for adapter-owned relations. Keys are lower-case
/// identifier components, preserving quoted dots and database qualification.
pub type Relations = BTreeMap<Vec<String>, Columns>;

/// Normalize fixed sets, then report whether operand binding needs a snapshot.
pub fn prepare<T: VisitMut>(value: &mut T) -> bool {
    crate::result_types::lower_fixed_sets(value);
    struct Find;
    impl VisitorMut for Find {
        type Break = ();
        fn pre_visit_query(&mut self, query: &mut Query) -> ControlFlow<()> {
            if matches!(
                query.body.as_ref(),
                SetExpr::SetOperation { .. } | SetExpr::Values(_)
            ) {
                return ControlFlow::Break(());
            }
            ControlFlow::Continue(())
        }
        fn pre_visit_select(&mut self, select: &mut Select) -> ControlFlow<()> {
            if matches!(select.distinct, Some(Distinct::Distinct))
                || matches!(&select.group_by, GroupByExpr::Expressions(groups, _) if !groups.is_empty())
            {
                return ControlFlow::Break(());
            }
            ControlFlow::Continue(())
        }
        fn pre_visit_order_by_expr(&mut self, _: &mut OrderByExpr) -> ControlFlow<()> {
            ControlFlow::Break(())
        }
        fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
            if matches!(expr, Expr::Function(f) if f.over.is_some())
                || matches!(
                    expr,
                    Expr::AnyOp { .. } | Expr::AllOp { .. } | Expr::Subquery(_)
                )
            {
                return ControlFlow::Break(());
            }
            if crate::money_format::candidate(expr)
                || arithmetic(expr)
                || crate::datetime2_compare::is_comparison(expr)
                || crate::expression_metadata::conditional::candidate(expr)
            {
                return ControlFlow::Break(());
            }
            if matches!(expr, Expr::Function(f) if matches!(f.name.to_string().to_ascii_uppercase().as_str(), "LEN" | "DATALENGTH" | "REPLICATE" | "LEFT" | "RIGHT" | "TODATETIMEOFFSET" | "SWITCHOFFSET" | "DATEADD" | "COUNT" | "COUNT_BIG" | "APPROX_COUNT_DISTINCT" | "SUM" | "AVG" | "MIN" | "MAX" | "STDEV" | "STDEVP" | "VAR" | "VARP" | "FIRST_VALUE" | "LAST_VALUE" | "LAG" | "LEAD" | "NTILE" | "PERCENTILE_DISC"))
            {
                return ControlFlow::Break(());
            }
            ControlFlow::Continue(())
        }
    }
    value.visit(&mut Find).is_break()
}

/// Binding consumes explicit declarations and performs no catalog reads.
/// Bind a prepared AST against explicit declarations and parameters.
/// Missing entries retain unknown types; this function performs no catalog access.
pub fn resolve<T: VisitMut>(
    catalog: &Snapshot,
    value: &mut T,
    parameters: &HashMap<String, crate::parameter::Parameter>,
) -> Result<(), String> {
    resolve_with_relations(catalog, &Relations::new(), value, parameters)
}

pub fn resolve_with_relations<T: VisitMut>(
    catalog: &Snapshot,
    relations: &Relations,
    value: &mut T,
    parameters: &HashMap<String, crate::parameter::Parameter>,
) -> Result<(), String> {
    let mut resolver = Resolver {
        catalog,
        relations,
        parameters,
        scopes: vec![],
        grouping_bases: vec![],
        expression_queries: vec![],
        query_ctes: vec![],
        derived_queries: vec![],
        query_outer_ends: vec![],
        from_frames: vec![],
        factor_depth: 0,
        factor_on: vec![],
        pending_on: None,
        on_frames: vec![],
        expr_depth: 0,
        syntax_unit_depth: None,
        order_depths: vec![],
        query_aliases: vec![],
        ctes: vec![],
    };
    match value.visit(&mut resolver) {
        ControlFlow::Continue(()) => Ok(()),
        ControlFlow::Break(error) => Err(error),
    }
}

fn arithmetic(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::UnaryOp {
            op: UnaryOperator::Plus | UnaryOperator::Minus,
            ..
        } | Expr::BinaryOp {
            op: BinaryOperator::Plus
                | BinaryOperator::Minus
                | BinaryOperator::Multiply
                | BinaryOperator::Divide
                | BinaryOperator::Modulo,
            ..
        }
    )
}

pub type Columns = Vec<(String, Option<DataType>)>;
struct Resolver<'a> {
    relations: &'a Relations,
    catalog: &'a Snapshot,
    parameters: &'a HashMap<String, crate::parameter::Parameter>,
    scopes: Vec<Scope>,
    grouping_bases: Vec<usize>,
    expression_queries: Vec<usize>,
    query_ctes: Vec<usize>,
    derived_queries: Vec<DerivedFrame>,
    query_outer_ends: Vec<usize>,
    from_frames: Vec<FromFrame>,
    factor_depth: usize,
    factor_on: Vec<Option<Scope>>,
    pending_on: Option<Vec<Scope>>,
    on_frames: Vec<OnFrame>,
    expr_depth: usize,
    syntax_unit_depth: Option<usize>,
    order_depths: Vec<Option<usize>>,
    query_aliases: Vec<(usize, HashSet<String>)>,
    ctes: Vec<HashMap<String, Option<Columns>>>,
}
#[derive(Default)]
struct FactorPlan {
    apply: Option<Scope>,
    on: Option<Scope>,
}
struct OnFrame {
    expr_depth: usize,
    query_depth: usize,
    scope_start: usize,
}
struct DerivedFrame {
    depth: usize,
    base: usize,
    restore: usize,
}
struct FromFrame {
    depth: usize,
    pending: VecDeque<FactorPlan>,
    apply_outer: Option<Vec<Scope>>,
}
fn renamed(mut columns: Columns, alias: Option<&TableAlias>) -> Option<Columns> {
    if let Some(alias) = alias
        && !alias.columns.is_empty()
    {
        if alias.columns.len() != columns.len() {
            return None;
        }
        for ((name, _), alias) in columns.iter_mut().zip(&alias.columns) {
            *name = alias.name.value.clone();
        }
    }
    // Positional outputs require names supplied by the surrounding alias.
    columns
        .iter()
        .all(|(name, _)| !name.is_empty())
        .then_some(columns)
}
// Source ranks include BIT below the four integer widths.
fn source_rank(
    expr: &Expr,
    parameters: &HashMap<String, crate::parameter::Parameter>,
) -> Option<u8> {
    crate::case_types::integer_rank(expr, parameters)
        .map(|rank| rank + 1)
        .or_else(|| crate::case_types::is_bit(expr, parameters).then_some(0))
}
fn rank_type(rank: u8) -> DataType {
    if rank == 0 {
        return DataType::Bit(None);
    }
    let rank = rank - 1;
    match rank {
        0 => DataType::UTinyInt,
        1 => DataType::SmallInt(None),
        2 => DataType::Int(None),
        _ => DataType::BigInt(None),
    }
}
pub fn datetimeoffset_type(scale: u8) -> DataType {
    DataType::Custom(
        ObjectName::from(vec![Ident::new("DATETIMEOFFSET")]),
        vec![scale.to_string()],
    )
}
pub fn datetime2_type(scale: u8) -> DataType {
    DataType::Custom(
        ObjectName::from(vec![Ident::new("DATETIME2")]),
        vec![scale.to_string()],
    )
}
fn scalar_currency_operand_type(kind: &DataType) -> bool {
    crate::money_cast::money_type(kind).is_some()
        || crate::character_storage::is_character(kind)
        || crate::sql_type::integral_type(kind)
        || matches!(
            kind,
            DataType::Bit(_)
                | DataType::Decimal(_)
                | DataType::Numeric(_)
                | DataType::Float(_)
                | DataType::Double(_)
                | DataType::Real
        )
}

fn source_type(
    expr: &Expr,
    parameters: &HashMap<String, crate::parameter::Parameter>,
) -> Option<DataType> {
    if matches!(expr, Expr::Subquery(query) if matches!(query.for_clause, Some(ForClause::Json { .. })))
    {
        return Some(DataType::Nvarchar(Some(CharacterLength::Max)));
    }
    if crate::variant_compare::known(expr) {
        return Some(crate::variant_compare::kind());
    }
    crate::expression_metadata::currency::kind(expr, parameters, &|_| None)
        .map(crate::expression_metadata::currency::declaration)
        .or_else(|| crate::datetimeoffset_compare::scale(expr, parameters).map(datetimeoffset_type))
        .or_else(|| crate::datetime2_compare::scale(expr, parameters).map(datetime2_type))
        .or_else(|| {
            crate::expression_metadata::temporal::time_scale(expr)
                .map(|s| DataType::Time(Some(u64::from(s)), TimezoneInfo::None))
        })
        .or_else(|| source_rank(expr, parameters).map(rank_type))
        .or_else(|| {
            crate::datalength::kind(expr, parameters, &|_| None).filter(|kind| {
                crate::character_storage::is_character(kind)
                    || crate::datalength::fixed_width(kind).is_some()
                    || matches!(kind, DataType::Binary(_) | DataType::Varbinary(_))
            })
        })
}
fn plain_star(options: &WildcardAdditionalOptions) -> bool {
    options.opt_ilike.is_none()
        && options.opt_exclude.is_none()
        && options.opt_except.is_none()
        && options.opt_replace.is_none()
        && options.opt_rename.is_none()
        && options.opt_alias.is_none()
}
impl Resolver<'_> {
    fn column_type(&self, value: &Expr) -> Option<DataType> {
        let key = match value {
            Expr::Identifier(id) => vec![id.value.to_lowercase()],
            Expr::CompoundIdentifier(ids) => ids.iter().map(|id| id.value.to_lowercase()).collect(),
            _ => return None,
        };
        let qualifier = &key[..key.len() - 1];
        for scope in self.scopes.iter().rev() {
            if let Some(kind) = scope.column(value) {
                return Some(kind);
            }
            if scope.unknown_source
                || scope.columns.contains_key(&key)
                || (!qualifier.is_empty()
                    && scope
                        .sources
                        .iter()
                        .any(|(names, _)| names.iter().any(|n| n == qualifier)))
            {
                break;
            }
        }
        None
    }
    fn distinct_outputs(&mut self, query: &mut Query) -> Result<(), String> {
        if !matches!(query.body.as_ref(), SetExpr::Select(select) if matches!(select.distinct, Some(Distinct::Distinct)))
        {
            return Ok(());
        }
        let Some(columns) = self.body_outputs(&query.body)? else {
            return Ok(());
        };
        let variants = columns
            .iter()
            .map(|c| crate::variant_sets::Equality::for_type(c.1.as_ref()))
            .collect::<Vec<_>>();
        if !variants.iter().any(|v| v.special()) {
            return Ok(());
        }
        let SetExpr::Select(select) = query.body.as_mut() else {
            unreachable!()
        };
        let scope = self.scope(select)?;
        let references = scope.projection_references(select);
        // Resolve source identity before moving the projection behind a wrapper.
        // Output aliases take precedence over unqualified source column names.
        if let Some(references) = references
            && let Some(OrderBy {
                kind: OrderByKind::Expressions(orders),
                ..
            }) = &mut query.order_by
        {
            for order in orders {
                if matches!(&order.expr, Expr::Identifier(id) if columns.iter().any(|c| c.0.eq_ignore_ascii_case(&id.value)))
                {
                    continue;
                }
                let source = scope.source_column(&order.expr);
                if let Some(index) = references.iter().position(|expr| {
                    expr == &order.expr
                        || source.is_some_and(|source| scope.source_column(expr) == Some(source))
                }) {
                    order.expr = Expr::Value(Value::Number((index + 1).to_string(), false).into());
                }
            }
        }
        select.distinct = None;
        crate::variant_order::wrap(
            query,
            &columns.iter().map(|c| c.0.clone()).collect::<Vec<_>>(),
            &[],
            vec![],
        );
        let SetExpr::Select(select) = query.body.as_mut() else {
            unreachable!()
        };
        select.distinct = Some(Distinct::On(
            variants
                .iter()
                .enumerate()
                .map(|(i, variant)| {
                    let value = crate::variant_order::column(i);
                    variant.key(value)
                })
                .collect(),
        ));
        Ok(())
    }
    fn order_outputs(&mut self, query: &mut Query) -> Result<(), String> {
        let Some(OrderBy {
            kind: OrderByKind::Expressions(orders),
            ..
        }) = &query.order_by
        else {
            return Ok(());
        };
        let Some(columns) = self.body_outputs(&query.body)? else {
            return Ok(());
        };
        let scope = match query.body.as_ref() {
            SetExpr::Select(select) => self.scope(select)?,
            _ => Scope::default(),
        };
        let mut keys = Vec::new();
        let mut extra = Vec::new();
        let mut has_variant_output = false;
        for order in orders {
            let position = match &order.expr {
                Expr::Value(value) => match &value.value {
                    Value::Number(n, _) => {
                        let Some(index) = n
                            .parse::<usize>()
                            .ok()
                            .and_then(|n| n.checked_sub(1))
                            .filter(|n| *n < columns.len())
                        else {
                            return Ok(());
                        };
                        Some(index)
                    }
                    _ => None,
                },
                Expr::Identifier(id) => {
                    let found = columns
                        .iter()
                        .enumerate()
                        .filter(|(_, (name, _))| name.eq_ignore_ascii_case(&id.value))
                        .map(|(i, _)| i)
                        .collect::<Vec<_>>();
                    if found.len() > 1 {
                        return Ok(());
                    }
                    found.first().copied()
                }
                _ => None,
            };
            if let Some(index) = position {
                let variant = columns[index]
                    .1
                    .as_ref()
                    .is_some_and(crate::variant_pack::is_variant);
                has_variant_output |= variant;
                keys.push((index, variant));
            } else {
                // Source expressions become hidden outputs, so sorting uses the
                // value already computed by the inner query.
                let mut expr = order.expr.clone();
                let _ = VisitMut::visit(
                    &mut expr,
                    &mut Annotate {
                        scope: &scope,
                        queries: 0,
                        datetime_only: true,
                        syntax: DatepartSyntax::default(),
                    },
                );
                let variant = crate::variant_compare::known(&expr);
                keys.push((columns.len() + extra.len(), variant));
                extra.push(expr);
            }
        }
        if !has_variant_output {
            return Ok(());
        }
        if !extra.is_empty()
            && !matches!(query.body.as_ref(), SetExpr::Select(select) if select.distinct.is_none())
        {
            return Ok(());
        }
        crate::variant_order::wrap(
            query,
            &columns.iter().map(|c| c.0.clone()).collect::<Vec<_>>(),
            &keys,
            extra,
        );
        Ok(())
    }
    fn push_ctes(&mut self, query: &Query) -> Result<(), String> {
        // Placeholders prevent self/forward references from falling through to
        // same-named catalog tables. Infer recursive outputs from their anchors.
        self.ctes.push(
            query
                .with
                .iter()
                .flat_map(|with| {
                    with.cte_tables
                        .iter()
                        .map(|cte| (cte.alias.name.value.to_lowercase(), None))
                })
                .collect(),
        );
        for cte in query.with.iter().flat_map(|with| &with.cte_tables) {
            let anchor = crate::cte_recursion::anchor(cte);
            let columns = self
                .outputs(anchor.as_ref().unwrap_or(&cte.query))?
                .and_then(|cols| renamed(cols, Some(&cte.alias)));
            self.ctes
                .last_mut()
                .unwrap()
                .insert(cte.alias.name.value.to_lowercase(), columns);
        }
        Ok(())
    }
    fn normalize_sets(&mut self, body: &mut SetExpr) -> Result<(), String> {
        match body {
            SetExpr::SetOperation {
                left,
                right,
                op,
                set_quantifier,
            } => {
                self.normalize_sets(left)?;
                self.normalize_sets(right)?;
                let (Some(a), Some(b)) = (self.body_outputs(left)?, self.body_outputs(right)?)
                else {
                    return Ok(());
                };
                if a.len() != b.len() {
                    return Ok(());
                }
                let numeric = a
                    .iter()
                    .zip(&b)
                    .map(|((_, a), (_, b))| {
                        let (a, b) = (a.as_ref()?, b.as_ref()?);
                        let kind = crate::expression_metadata::arithmetic::set_type(a, b)?;
                        (a != &kind || b != &kind).then_some(kind)
                    })
                    .collect::<Vec<_>>();
                if numeric.iter().any(Option::is_some) {
                    crate::set_coercion::wrap(
                        left,
                        &a.iter().map(|c| c.0.clone()).collect::<Vec<_>>(),
                        &numeric,
                    );
                    crate::set_coercion::wrap(
                        right,
                        &b.iter().map(|c| c.0.clone()).collect::<Vec<_>>(),
                        &numeric,
                    );
                }
                let variants = a
                    .iter()
                    .zip(&b)
                    .map(|((_, a), (_, b))| {
                        a.as_ref().is_some_and(crate::variant_pack::is_variant)
                            || b.as_ref().is_some_and(crate::variant_pack::is_variant)
                    })
                    .collect::<Vec<_>>();
                if crate::variant_sets::supported(*op, *set_quantifier)
                    && variants.iter().any(|v| *v)
                {
                    crate::variant_sets::wrap(
                        left,
                        &a.iter().map(|c| c.0.clone()).collect::<Vec<_>>(),
                        &variants,
                    );
                    crate::variant_sets::wrap(
                        right,
                        &b.iter().map(|c| c.0.clone()).collect::<Vec<_>>(),
                        &variants,
                    );
                }
                let offset_scales = a
                    .iter()
                    .zip(&b)
                    .map(|((_, a), (_, b))| {
                        [a.as_ref(), b.as_ref()]
                            .into_iter()
                            .flatten()
                            .filter_map(|kind| {
                                crate::datetimeoffset_cast::scale(kind).ok().flatten()
                            })
                            .max()
                    })
                    .collect::<Vec<_>>();
                let equality = variants
                    .iter()
                    .zip(&offset_scales)
                    .map(|(variant, offset)| {
                        if *variant {
                            crate::variant_sets::Equality::Variant
                        } else if offset.is_some() {
                            crate::variant_sets::Equality::DateTimeOffset
                        } else {
                            crate::variant_sets::Equality::Native
                        }
                    })
                    .collect::<Vec<_>>();
                let membership = matches!(*op, SetOperator::Intersect | SetOperator::Except)
                    && crate::variant_sets::supported(*op, *set_quantifier)
                    && equality.iter().any(|v| v.special());
                let deduplicate = *op == SetOperator::Union
                    && matches!(
                        *set_quantifier,
                        SetQuantifier::Distinct | SetQuantifier::None
                    )
                    && equality.iter().any(|v| v.special());
                if deduplicate {
                    *set_quantifier = SetQuantifier::All;
                }
                let scales = a
                    .iter()
                    .zip(&b)
                    .zip(&offset_scales)
                    .map(|(((_, a), (_, b)), offset)| {
                        if offset.is_some() {
                            return None;
                        }
                        [a.as_ref(), b.as_ref()]
                            .into_iter()
                            .flatten()
                            .filter_map(|kind| crate::datetime2_cast::scale(kind).ok().flatten())
                            .max()
                    })
                    .collect::<Vec<_>>();
                if scales.iter().any(Option::is_some) {
                    crate::datetime2_sets::wrap(
                        left,
                        &a.iter().map(|c| c.0.clone()).collect::<Vec<_>>(),
                        &scales,
                    );
                    crate::datetime2_sets::wrap(
                        right,
                        &b.iter().map(|c| c.0.clone()).collect::<Vec<_>>(),
                        &scales,
                    );
                }
                if offset_scales.iter().any(Option::is_some) {
                    crate::datetime2_sets::wrap_temporal(
                        left,
                        &a.iter().map(|c| c.0.clone()).collect::<Vec<_>>(),
                        &offset_scales,
                        true,
                    );
                    crate::datetime2_sets::wrap_temporal(
                        right,
                        &b.iter().map(|c| c.0.clone()).collect::<Vec<_>>(),
                        &offset_scales,
                        true,
                    );
                }
                if membership {
                    crate::variant_sets::membership(
                        body,
                        &a.iter().map(|c| c.0.clone()).collect::<Vec<_>>(),
                        &equality,
                    );
                } else if deduplicate {
                    crate::variant_sets::deduplicate(
                        body,
                        &a.iter().map(|c| c.0.clone()).collect::<Vec<_>>(),
                        &equality,
                    );
                }
            }
            SetExpr::Values(values) => {
                let width = values.rows.first().map_or(0, |row| row.len());
                if values.rows.iter().any(|r| r.len() != width) {
                    return Ok(());
                }
                for index in 0..width {
                    if values
                        .rows
                        .iter()
                        .any(|row| crate::variant_compare::known(&row[index]))
                    {
                        for row in &mut values.rows {
                            row[index] = crate::variant_pack::convert(row[index].clone());
                        }
                        continue;
                    }
                    if let Some(scale) = values
                        .rows
                        .iter()
                        .filter_map(|r| {
                            crate::datetimeoffset_compare::scale(&r[index], self.parameters)
                        })
                        .max()
                    {
                        for row in &mut values.rows {
                            row[index] =
                                crate::datetimeoffset_cast::convert(row[index].clone(), scale);
                        }
                        continue;
                    }
                    if let Some(scale) = values
                        .rows
                        .iter()
                        .filter_map(|r| crate::datetime2_compare::scale(&r[index], self.parameters))
                        .max()
                    {
                        for row in &mut values.rows {
                            row[index] = crate::datetime2_cast::convert(row[index].clone(), scale);
                        }
                    }
                }
            }
            _ => {} // Nested Query nodes were normalized by their own visitor.
        }
        Ok(())
    }
    fn outputs(&mut self, query: &Query) -> Result<Option<Columns>, String> {
        if matches!(query.for_clause, Some(ForClause::Json { .. })) {
            return Ok(Some(vec![(
                "JSON_F52E2B61-18A1-11d1-B105-00805F49916B".into(),
                Some(DataType::Nvarchar(Some(CharacterLength::Max))),
            )]));
        }
        self.push_ctes(query)?;
        let result = self.body_outputs(&query.body);
        self.ctes.pop();
        result
    }
    fn body_outputs(&mut self, body: &SetExpr) -> Result<Option<Columns>, String> {
        match body {
            SetExpr::Query(query) => self.outputs(query),
            SetExpr::Values(values) => {
                let Some(first) = values.rows.first() else {
                    return Ok(None);
                };
                if values.rows.iter().any(|row| row.len() != first.len()) {
                    return Ok(None);
                }
                let columns = (0..first.len())
                    .map(|index| {
                        let types = values
                            .rows
                            .iter()
                            .map(|r| source_type(&r[index], self.parameters))
                            .collect::<Vec<_>>();
                        if types.iter().flatten().any(crate::variant_pack::is_variant) {
                            return (String::new(), Some(crate::variant_compare::kind()));
                        }
                        if let Some(scale) = types
                            .iter()
                            .flatten()
                            .filter_map(|kind| {
                                crate::datetimeoffset_cast::scale(kind).ok().flatten()
                            })
                            .max()
                        {
                            return (String::new(), Some(datetimeoffset_type(scale)));
                        }
                        if let Some(scale) = types
                            .iter()
                            .flatten()
                            .filter_map(|kind| crate::datetime2_cast::scale(kind).ok().flatten())
                            .max()
                        {
                            return (String::new(), Some(datetime2_type(scale)));
                        }
                        if let Some(scale) = crate::expression_metadata::temporal::common_scale(
                            values.rows.iter().map(|row| &row[index]),
                        ) {
                            return (
                                String::new(),
                                Some(DataType::Time(Some(u64::from(scale)), TimezoneInfo::None)),
                            );
                        }
                        let character = values
                            .rows
                            .iter()
                            .filter(|row| {
                                !crate::expression_metadata::conditional::literal_null(&row[index])
                            })
                            .try_fold(None, |previous: Option<DataType>, row| {
                                let next = source_type(&row[index], self.parameters)?;
                                if !crate::character_storage::is_character(&next) {
                                    return None;
                                }
                                Some(Some(match previous {
                                    None => next,
                                    Some(previous) => {
                                        crate::expression_metadata::character::set_type(
                                            &previous, &next,
                                        )?
                                    }
                                }))
                            })
                            .flatten();
                        if let Some(kind) = character {
                            return (String::new(), Some(kind));
                        }
                        if types
                            .iter()
                            .flatten()
                            .any(|kind| matches!(kind, DataType::Decimal(_) | DataType::Numeric(_)))
                        {
                            let numeric = values
                                .rows
                                .iter()
                                .filter(|row| {
                                    !crate::expression_metadata::conditional::literal_null(
                                        &row[index],
                                    )
                                })
                                .try_fold(None, |previous: Option<DataType>, row| {
                                    let next = source_type(&row[index], self.parameters)?;
                                    Some(Some(match previous {
                                        None => next,
                                        Some(previous) => {
                                            crate::expression_metadata::arithmetic::set_type(
                                                &previous, &next,
                                            )?
                                        }
                                    }))
                                })
                                .flatten();
                            return (String::new(), numeric);
                        }
                        let mut rank = None;
                        for row in &values.rows {
                            let mut expr = &row[index];
                            while let Expr::Nested(inner) = expr {
                                expr = inner;
                            }
                            if matches!(expr, Expr::Value(value) if value.value == Value::Null) {
                                continue;
                            }
                            let Some(next) = source_rank(expr, self.parameters) else {
                                return (String::new(), None);
                            };
                            rank = Some(rank.map_or(next, |previous: u8| previous.max(next)));
                        }
                        // All-untyped-NULL and mixed noninteger columns remain unknown.
                        (String::new(), rank.map(rank_type))
                    })
                    .collect();
                Ok(Some(columns))
            }
            SetExpr::Select(select) => {
                let scope = self.scope(select)?;
                let mut columns = vec![];
                for item in &select.projection {
                    let (name, expr) = match item {
                        SelectItem::ExprWithAlias { expr, alias } => (alias.value.clone(), expr),
                        SelectItem::UnnamedExpr(expr) => {
                            let name = match expr {
                                Expr::Identifier(id) => id.value.clone(),
                                Expr::CompoundIdentifier(ids) => ids.last().unwrap().value.clone(),
                                // A surrounding CTE/derived column alias list can
                                // name this positional output before scope binding.
                                _ => String::new(),
                            };
                            (name, expr)
                        }
                        SelectItem::Wildcard(options)
                            if plain_star(options) && !scope.unknown_source =>
                        {
                            columns.extend(scope.sources.iter().flat_map(|(_, cols)| cols.clone()));
                            continue;
                        }
                        SelectItem::QualifiedWildcard(
                            SelectItemQualifiedWildcardKind::ObjectName(name),
                            options,
                        ) if plain_star(options) => {
                            let Some(names) = name
                                .0
                                .iter()
                                .map(|p| p.as_ident().map(|id| id.value.to_lowercase()))
                                .collect::<Option<Vec<_>>>()
                            else {
                                return Ok(None);
                            };
                            let matches = scope
                                .sources
                                .iter()
                                .filter(|(qualifiers, _)| qualifiers.contains(&names))
                                .collect::<Vec<_>>();
                            if matches.len() != 1 {
                                return Ok(None);
                            }
                            columns.extend(matches[0].1.clone());
                            continue;
                        }
                        _ => return Ok(None),
                    };
                    let mut typed = expr.clone();
                    let _ = VisitMut::visit(
                        &mut typed,
                        &mut Annotate {
                            scope: &scope,
                            queries: 0,
                            datetime_only: false,
                            syntax: DatepartSyntax::default(),
                        },
                    );
                    let kind = source_type(&typed, self.parameters);
                    columns.push((name, kind));
                }
                Ok(Some(columns))
            }
            SetExpr::SetOperation {
                left,
                right,
                op,
                set_quantifier,
            } => {
                let (Some(left), Some(right)) =
                    (self.body_outputs(left)?, self.body_outputs(right)?)
                else {
                    return Ok(None);
                };
                if left.len() != right.len() {
                    return Ok(None);
                }
                Ok(Some(
                    left.into_iter()
                        .zip(right)
                        .map(|((name, a), (_, b))| {
                            if crate::variant_sets::supported(*op, *set_quantifier)
                                && (a.as_ref().is_some_and(crate::variant_pack::is_variant)
                                    || b.as_ref().is_some_and(crate::variant_pack::is_variant))
                            {
                                return (name, Some(crate::variant_compare::kind()));
                            }
                            if let Some(scale) = [a.as_ref(), b.as_ref()]
                                .into_iter()
                                .flatten()
                                .filter_map(|kind| {
                                    crate::datetimeoffset_cast::scale(kind).ok().flatten()
                                })
                                .max()
                            {
                                return (name, Some(datetimeoffset_type(scale)));
                            }
                            if let Some(scale) = [a.as_ref(), b.as_ref()]
                                .into_iter()
                                .flatten()
                                .filter_map(|kind| {
                                    crate::datetime2_cast::scale(kind).ok().flatten()
                                })
                                .max()
                            {
                                return (name, Some(datetime2_type(scale)));
                            }
                            if let (
                                Some(DataType::Time(a, TimezoneInfo::None)),
                                Some(DataType::Time(b, TimezoneInfo::None)),
                            ) = (&a, &b)
                            {
                                return (
                                    name,
                                    Some(DataType::Time(
                                        Some(a.unwrap_or(7).max(b.unwrap_or(7))),
                                        TimezoneInfo::None,
                                    )),
                                );
                            }
                            if let Some(kind) = a.as_ref().zip(b.as_ref()).and_then(|(a, b)| {
                                crate::expression_metadata::character::set_type(a, b)
                            }) {
                                return (name, Some(kind));
                            }
                            if let Some(kind) = a.as_ref().zip(b.as_ref()).and_then(|(a, b)| {
                                crate::expression_metadata::arithmetic::set_type(a, b)
                            }) {
                                return (name, Some(kind));
                            }
                            let rank = |kind: DataType| {
                                source_rank(
                                    &Expr::Cast {
                                        kind: CastKind::Cast,
                                        expr: Box::new(Expr::Value(Value::Null.into())),
                                        data_type: kind,
                                        format: None,
                                    },
                                    self.parameters,
                                )
                            };
                            (
                                name,
                                a.and_then(rank)
                                    .zip(b.and_then(rank))
                                    .map(|(a, b)| rank_type(a.max(b))),
                            )
                        })
                        .collect(),
                ))
            }
            _ => Ok(None),
        }
    }
    fn build_from_frame(&mut self, sources: &[TableWithJoins]) -> Result<FromFrame, String> {
        let mut pending = VecDeque::new();
        for source in sources {
            pending.push_back(FactorPlan::default());
            let mut left = TableWithJoins {
                relation: source.relation.clone(),
                joins: vec![],
            };
            for join in &source.joins {
                let input = if matches!(
                    join.join_operator,
                    JoinOperator::CrossApply | JoinOperator::OuterApply
                ) {
                    Some(self.source_scope(std::slice::from_ref(&left))?)
                } else {
                    None
                };
                left.joins.push(join.clone());
                let on = matches!(
                    &join.join_operator,
                    JoinOperator::Join(JoinConstraint::On(_))
                        | JoinOperator::Inner(JoinConstraint::On(_))
                        | JoinOperator::Left(JoinConstraint::On(_))
                        | JoinOperator::LeftOuter(JoinConstraint::On(_))
                        | JoinOperator::Right(JoinConstraint::On(_))
                        | JoinOperator::RightOuter(JoinConstraint::On(_))
                        | JoinOperator::FullOuter(JoinConstraint::On(_))
                );
                pending.push_back(FactorPlan {
                    apply: input,
                    on: if on {
                        Some(self.source_scope(std::slice::from_ref(&left))?)
                    } else {
                        None
                    },
                });
            }
        }
        Ok(FromFrame {
            depth: self.factor_depth,
            pending,
            apply_outer: None,
        })
    }
    fn scope(&mut self, select: &Select) -> Result<Scope, String> {
        self.source_scope(&select.from)
    }
    fn source_scope(&mut self, sources: &[TableWithJoins]) -> Result<Scope, String> {
        let mut scope = Scope::default();
        for source in sources {
            for factor in std::iter::once(&source.relation)
                .chain(source.joins.iter().map(|join| &join.relation))
            {
                let (qualifiers, columns, alias) = match factor {
                    TableFactor::OpenJsonTable { columns, alias, .. } => (
                        vec![vec![
                            alias
                                .as_ref()
                                .map(|a| a.name.value.to_lowercase())
                                .unwrap_or_else(|| "openjson".into()),
                        ]],
                        Some(crate::expression_metadata::openjson::columns(columns)),
                        alias.as_ref(),
                    ),
                    TableFactor::Table { alias, .. }
                        if crate::generate_series::is_series(factor) =>
                    {
                        (
                            vec![vec![
                                alias
                                    .as_ref()
                                    .map(|a| a.name.value.to_lowercase())
                                    .unwrap_or_else(|| "generate_series".into()),
                            ]],
                            Some(vec![(
                                "value".into(),
                                crate::generate_series::result_type(factor, self.parameters),
                            )]),
                            alias.as_ref(),
                        )
                    }
                    TableFactor::NestedJoin {
                        table_with_joins,
                        alias: None,
                    } => {
                        let nested = self.source_scope(std::slice::from_ref(table_with_joins))?;
                        scope.unknown_source |= nested.unknown_source;
                        for (key, kind) in nested.columns {
                            scope.insert(key, kind);
                        }
                        scope.sources.extend(nested.sources);
                        continue;
                    }
                    TableFactor::Derived {
                        subquery,
                        alias: Some(alias),
                        ..
                    } => (
                        vec![vec![alias.name.value.to_lowercase()]],
                        self.outputs(subquery)?,
                        Some(alias),
                    ),
                    TableFactor::Table {
                        name,
                        alias,
                        args: None,
                        ..
                    } => {
                        let Some(names) = name
                            .0
                            .iter()
                            .map(|part| part.as_ident().map(|id| id.value.to_lowercase()))
                            .collect::<Option<Vec<_>>>()
                        else {
                            scope.unknown_source = true;
                            continue;
                        };
                        let cte = if names.len() == 1 {
                            self.ctes
                                .iter()
                                .rev()
                                .find_map(|frame| frame.get(&names[0]))
                                .cloned()
                        } else {
                            None
                        };
                        let (qualifiers, columns) = if let Some(columns) = cte {
                            (vec![names], columns)
                        } else if let Some(columns) = self.relations.get(&names) {
                            (
                                vec![names.clone(), vec![names.last().unwrap().clone()]],
                                Some(columns.clone()),
                            )
                        } else {
                            let (schema, table) = match names.as_slice() {
                                [table] => ("dbo".to_string(), table.clone()),
                                [schema, table] => (schema.clone(), table.clone()),
                                _ => {
                                    scope.unknown_source = true;
                                    continue;
                                }
                            };
                            let key = (schema.clone(), table.clone());
                            (
                                vec![vec![table.clone()], vec![schema, table]],
                                self.catalog.get(&key).cloned().transpose()?,
                            )
                        };
                        let qualifiers = alias.as_ref().map_or(qualifiers, |alias| {
                            vec![vec![alias.name.value.to_lowercase()]]
                        });
                        (qualifiers, columns, alias.as_ref())
                    }
                    _ => {
                        scope.unknown_source = true;
                        continue;
                    }
                };
                let Some(columns) = columns.and_then(|columns| renamed(columns, alias)) else {
                    scope.unknown_source = true;
                    continue;
                };
                if columns.is_empty() {
                    scope.unknown_source = true;
                }
                for (column, kind) in &columns {
                    scope.insert(vec![column.to_lowercase()], kind.clone());
                    for qualifier in &qualifiers {
                        let mut key = qualifier.clone();
                        key.push(column.to_lowercase());
                        scope.insert(key, kind.clone());
                    }
                }
                scope.sources.push((qualifiers, columns));
            }
        }
        Ok(scope)
    }
}
fn dml_sources(statement: &Statement) -> Option<Vec<TableWithJoins>> {
    match statement {
        Statement::Update(update) => {
            let mut sources = vec![update.table.clone()];
            if let Some(
                UpdateTableFromKind::BeforeSet(from) | UpdateTableFromKind::AfterSet(from),
            ) = &update.from
            {
                sources.extend(from.clone());
            }
            Some(sources)
        }
        Statement::Delete(delete) => {
            let (FromTable::WithFromKeyword(from) | FromTable::WithoutKeyword(from)) = &delete.from;
            let mut sources = from.clone();
            sources.extend(delete.using.clone().unwrap_or_default());
            Some(sources)
        }
        _ => None,
    }
}
impl VisitorMut for Resolver<'_> {
    type Break = String;
    fn pre_visit_order_by_expr(&mut self, order: &mut OrderByExpr) -> ControlFlow<String> {
        let alias = self.query_aliases.last().is_some_and(|(depth, aliases)| {
            *depth == self.expr_depth
                && matches!(&order.expr, Expr::Identifier(id) if aliases.contains(&id.value.to_lowercase()))
        });
        self.order_depths.push(alias.then_some(self.expr_depth + 1));
        if !alias && let Some(scope) = self.scopes.last() {
            let mut typed = order.expr.clone();
            let _ = VisitMut::visit(
                &mut typed,
                &mut Annotate {
                    scope,
                    queries: 0,
                    datetime_only: true,
                    syntax: DatepartSyntax::default(),
                },
            );
            if crate::variant_compare::known(&typed) {
                order.expr = crate::variant_compare::key(typed);
            } else if crate::datetimeoffset_compare::scale(&typed, self.parameters).is_some() {
                order.expr = crate::datetimeoffset_compare::key(typed);
            }
        }
        ControlFlow::Continue(())
    }

    fn post_visit_order_by_expr(&mut self, _: &mut OrderByExpr) -> ControlFlow<String> {
        self.order_depths.pop();
        ControlFlow::Continue(())
    }

    fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<String> {
        if matches!(
            expr,
            Expr::Subquery(_) | Expr::Exists { .. } | Expr::InSubquery { .. }
        ) {
            self.expression_queries.pop();
        }
        if let Expr::InSubquery {
            expr: value,
            subquery,
            ..
        } = expr
        {
            match self.outputs(subquery) {
                Ok(Some(columns)) if columns.len() == 1 => {
                    let right_datetime = columns[0].1.as_ref().is_some_and(|kind| {
                        crate::datetime2_cast::scale(kind).ok().flatten().is_some()
                    });
                    if columns[0]
                        .1
                        .as_ref()
                        .is_some_and(crate::variant_pack::is_variant)
                        || crate::variant_compare::known(value)
                    {
                        crate::variant_compare::subquery(value, subquery);
                    } else if columns[0].1.as_ref().is_some_and(|kind| {
                        crate::datetimeoffset_cast::scale(kind)
                            .ok()
                            .flatten()
                            .is_some()
                    }) || crate::datetimeoffset_compare::scale(value, self.parameters)
                        .is_some()
                    {
                        crate::datetimeoffset_compare::subquery(value, subquery);
                    } else if right_datetime
                        || crate::datetime2_compare::scale(value, self.parameters).is_some()
                    {
                        crate::datetime2_compare::subquery(value, subquery);
                    }
                }
                Err(error) => return ControlFlow::Break(error),
                _ => {}
            }
        }
        if let Expr::AnyOp { right, .. } | Expr::AllOp { right, .. } = expr
            && let Expr::Cast {
                expr: source,
                data_type,
                ..
            } = right.as_ref()
            && scalar_currency_operand_type(data_type)
            && matches!(source.as_ref(), Expr::Subquery(_))
        {
            **right = *source.clone();
        }
        let comparison = match expr {
            Expr::InSubquery { expr, subquery, .. } => Some((expr.as_mut(), subquery)),
            Expr::AnyOp { left, right, .. } | Expr::AllOp { left, right, .. } => {
                if let Expr::Subquery(query) = right.as_mut() {
                    Some((left.as_mut(), query))
                } else {
                    None
                }
            }
            _ => None,
        };
        if let Some((value, query)) = comparison {
            match self.outputs(query) {
                Ok(Some(columns)) if columns.len() == 1 => {
                    if let Some(kind) = columns[0].1.as_ref() {
                        crate::money_compare::subquery(value, query, kind, self.parameters, &|e| {
                            self.column_type(e)
                        });
                    }
                }
                Err(error) => return ControlFlow::Break(error),
                _ => {}
            }
        }
        // JSON fragments carry provenance in their expression shape. A character
        // cast would turn a nested fragment into quoted JSON text.
        let json_fragment = crate::for_json::fragment(expr);
        if let Expr::Subquery(query) = expr {
            match self.outputs(query) {
                Ok(Some(columns)) if columns.len() == 1 => {
                    if let Some(kind) = &columns[0].1
                        && (crate::datetime2_cast::scale(kind).ok().flatten().is_some()
                            || crate::datetimeoffset_cast::scale(kind)
                                .ok()
                                .flatten()
                                .is_some()
                            || crate::variant_pack::is_variant(kind)
                            || (!json_fragment && scalar_currency_operand_type(kind)))
                    {
                        *expr = Expr::Cast {
                            kind: CastKind::Cast,
                            expr: Box::new(expr.clone()),
                            data_type: kind.clone(),
                            format: None,
                        };
                    }
                }
                Err(error) => return ControlFlow::Break(error),
                _ => {}
            }
        }
        let column = |value: &Expr| self.column_type(value);
        if let Err(error) = crate::unary_operator::check(expr, self.parameters, &column) {
            return ControlFlow::Break(error.message);
        }
        if let Err(error) = crate::nullif::lower_currency(expr, self.parameters, &column) {
            return ControlFlow::Break(error);
        }
        crate::money_arithmetic::lower(expr, self.parameters, &column);
        crate::decimal_division::lower(expr, self.parameters, &column);
        if let Err(error) = crate::left_right::lower(expr, self.parameters, &column) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::replicate::lower(expr, self.parameters, &column) {
            return ControlFlow::Break(error);
        }
        crate::money_compare::lower(expr, self.parameters, &column);
        crate::money_results::lower(expr, self.parameters, &column);
        if let Err(error) = crate::money_format::lower(expr, self.parameters, &column) {
            return ControlFlow::Break(error);
        }
        if self
            .on_frames
            .last()
            .is_some_and(|frame| frame.expr_depth == self.expr_depth)
        {
            let frame = self.on_frames.pop().unwrap();
            self.scopes.truncate(frame.scope_start);
        }
        self.expr_depth -= 1;
        ControlFlow::Continue(())
    }
    fn pre_visit_statement(&mut self, statement: &mut Statement) -> ControlFlow<String> {
        let sources = dml_sources(statement);
        if let Some(sources) = sources {
            // Resolve the target alias on a copy for the whole-statement scope,
            // but keep original join trees for ON/APPLY visitation.
            let mut bound = statement.clone();
            if let Err(error) = crate::update::canonicalize(&mut bound)
                .and_then(|_| crate::delete::canonicalize(&mut bound))
            {
                return ControlFlow::Break(error.to_string());
            }
            match self.source_scope(&dml_sources(&bound).unwrap()) {
                Ok(scope) => self.scopes.push(scope),
                Err(error) => return ControlFlow::Break(error),
            }
            match self.build_from_frame(&sources) {
                Ok(frame) => self.from_frames.push(frame),
                Err(error) => return ControlFlow::Break(error),
            }
        }
        ControlFlow::Continue(())
    }
    fn post_visit_statement(&mut self, statement: &mut Statement) -> ControlFlow<String> {
        if matches!(statement, Statement::Update(_) | Statement::Delete(_)) {
            self.from_frames.pop();
            self.scopes.pop();
        }
        ControlFlow::Continue(())
    }
    fn pre_visit_query(&mut self, query: &mut Query) -> ControlFlow<String> {
        // Query visits WITH definitions before the body. Parenthesized set
        // branches inherit correlation, while CTEs and derived tables start a
        // new binding boundary. Expression subqueries within either can still
        // correlate with their own enclosing SELECT.
        let cte = self.query_ctes.last_mut().is_some_and(|remaining| {
            if *remaining == 0 {
                false
            } else {
                *remaining -= 1;
                true
            }
        });
        let depth = self.grouping_bases.len();
        let derived = self
            .derived_queries
            .last()
            .filter(|frame| frame.depth == depth && self.expression_queries.last() != Some(&depth));
        let base = if cte {
            self.scopes.len()
        } else if let Some(frame) = derived {
            frame.base
        } else if let Some(frame) = self
            .on_frames
            .last()
            .filter(|frame| frame.query_depth == depth)
        {
            frame.scope_start
        } else {
            *self.grouping_bases.last().unwrap_or(&0)
        };
        self.query_outer_ends.push(self.scopes.len());
        self.grouping_bases.push(base);
        if let Err(error) = crate::named_windows::query(query) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = self.push_ctes(query) {
            return ControlFlow::Break(error);
        }
        if let SetExpr::Select(select) = query.body.as_mut() {
            match self.scope(select) {
                Ok(mut scope) => {
                    if let Err(error) = alias_scope::validate(select, &scope) {
                        return ControlFlow::Break(error);
                    }
                    match group_all::lower(select, query.order_by.as_mut(), &scope) {
                        Ok(true) => match self.scope(select) {
                            Ok(updated) => scope = updated,
                            Err(error) => return ControlFlow::Break(error),
                        },
                        Ok(false) => {}
                        Err(error) => return ControlFlow::Break(error),
                    }
                    if let Err(error) = scope.validate_grouping(
                        select,
                        &self.scopes[*self.grouping_bases.last().unwrap_or(&0)..],
                    ) {
                        return ControlFlow::Break(error);
                    }
                    crate::grouping::expand_sets(select);
                    variant_groups::lower(select, query.order_by.as_mut(), &scope);
                }
                Err(error) => return ControlFlow::Break(error),
            }
        }
        if let Err(error) = self.distinct_outputs(query) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = self.order_outputs(query) {
            return ControlFlow::Break(error);
        }
        fn aliases(body: &SetExpr) -> HashSet<String> {
            match body {
                SetExpr::Select(select) => select
                    .projection
                    .iter()
                    .filter_map(|item| {
                        if let SelectItem::ExprWithAlias { alias, .. } = item {
                            Some(alias.value.to_lowercase())
                        } else {
                            None
                        }
                    })
                    .collect(),
                SetExpr::Query(query) => aliases(&query.body),
                SetExpr::SetOperation { left, .. } => aliases(left),
                _ => HashSet::new(),
            }
        }
        self.query_aliases
            .push((self.expr_depth, aliases(&query.body)));
        self.query_ctes
            .push(query.with.as_ref().map_or(0, |with| with.cte_tables.len()));
        // Keep this frame through ORDER BY, which is visited after SELECT.
        let scope = match query.body.as_ref() {
            SetExpr::Select(select) => self.scope(select),
            _ => Ok(Scope::default()),
        };
        match scope {
            Ok(scope) => self.scopes.push(scope),
            Err(e) => return ControlFlow::Break(e),
        }
        ControlFlow::Continue(())
    }
    fn post_visit_query(&mut self, query: &mut Query) -> ControlFlow<String> {
        if let Err(error) = self.normalize_sets(&mut query.body) {
            return ControlFlow::Break(error);
        }
        self.scopes.pop();
        self.ctes.pop();
        self.grouping_bases.pop();
        self.query_ctes.pop();
        self.query_aliases.pop();
        self.query_outer_ends.pop();
        ControlFlow::Continue(())
    }
    fn pre_visit_table_factor(&mut self, factor: &mut TableFactor) -> ControlFlow<String> {
        let (plan, inherited) = self
            .from_frames
            .last_mut()
            .filter(|frame| frame.depth == self.factor_depth)
            .map(|frame| {
                (
                    frame.pending.pop_front().unwrap_or_default(),
                    frame.apply_outer.clone(),
                )
            })
            .unwrap_or_default();
        self.factor_on.push(plan.on);
        let apply_outer = if let Some(left) = plan.apply {
            // A joined APPLY input shares the external dependency across all
            // its constituents. An inner APPLY additionally sees its own left.
            let mut outer = inherited.unwrap_or_else(|| {
                let base = *self.grouping_bases.last().unwrap_or(&0);
                let end = *self.query_outer_ends.last().unwrap_or(&0);
                self.scopes[base..end].to_vec()
            });
            outer.push(left);
            Some(outer)
        } else {
            inherited
        };
        self.factor_depth += 1;
        if let TableFactor::NestedJoin {
            table_with_joins, ..
        } = factor
        {
            match self.build_from_frame(std::slice::from_ref(table_with_joins)) {
                Ok(mut frame) => {
                    frame.apply_outer = apply_outer.clone();
                    self.from_frames.push(frame);
                }
                Err(error) => return ControlFlow::Break(error),
            }
        }
        if matches!(factor, TableFactor::Derived { .. }) {
            let restore = self.scopes.len();
            if let Some(outer) = apply_outer {
                // Exclude this query's output and later joins from the scope.
                self.scopes.extend(outer);
            }
            self.derived_queries.push(DerivedFrame {
                depth: self.grouping_bases.len(),
                base: restore,
                restore,
            });
        }
        ControlFlow::Continue(())
    }
    fn post_visit_table_factor(&mut self, factor: &mut TableFactor) -> ControlFlow<String> {
        if matches!(factor, TableFactor::NestedJoin { .. }) {
            self.from_frames.pop();
        }
        if matches!(factor, TableFactor::Derived { .. }) {
            let frame = self.derived_queries.pop().unwrap();
            self.scopes.truncate(frame.restore);
        }
        self.factor_depth -= 1;
        if let Some(on) = self.factor_on.pop().flatten() {
            let mut outer = self
                .from_frames
                .last()
                .and_then(|frame| frame.apply_outer.clone())
                .unwrap_or_else(|| {
                    let base = *self.grouping_bases.last().unwrap_or(&0);
                    let end = *self.query_outer_ends.last().unwrap_or(&0);
                    self.scopes[base..end].to_vec()
                });
            outer.push(on);
            self.pending_on = Some(outer);
        }
        ControlFlow::Continue(())
    }
    fn pre_visit_select(&mut self, select: &mut Select) -> ControlFlow<String> {
        if let Err(error) = crate::named_windows::select(select) {
            return ControlFlow::Break(error);
        }
        match self.scope(select) {
            Ok(mut scope) => {
                if let Err(error) = alias_scope::validate(select, &scope) {
                    return ControlFlow::Break(error);
                }
                match group_all::lower(select, None, &scope) {
                    Ok(true) => match self.scope(select) {
                        Ok(updated) => scope = updated,
                        Err(error) => return ControlFlow::Break(error),
                    },
                    Ok(false) => {}
                    Err(error) => return ControlFlow::Break(error),
                }
                if let Err(error) = scope.validate_grouping(
                    select,
                    &self.scopes[*self.grouping_bases.last().unwrap_or(&0)..],
                ) {
                    return ControlFlow::Break(error);
                }
                crate::grouping::expand_sets(select);
                variant_groups::lower(select, None, &scope);
                match self.build_from_frame(&select.from) {
                    Ok(frame) => self.from_frames.push(frame),
                    Err(error) => return ControlFlow::Break(error),
                }
                self.scopes.push(scope);
            }
            Err(e) => return ControlFlow::Break(e),
        }
        ControlFlow::Continue(())
    }
    fn post_visit_select(&mut self, _: &mut Select) -> ControlFlow<String> {
        self.from_frames.pop();
        self.scopes.pop();
        ControlFlow::Continue(())
    }
    fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<String> {
        self.expr_depth += 1;
        let syntax_unit = self.syntax_unit_depth.take() == Some(self.expr_depth);
        if let Some(scopes) = self.pending_on.take() {
            self.on_frames.push(OnFrame {
                expr_depth: self.expr_depth,
                query_depth: self.grouping_bases.len(),
                scope_start: self.scopes.len(),
            });
            self.scopes.extend(scopes);
        }
        if !syntax_unit
            && !self
                .order_depths
                .last()
                .is_some_and(|depth| depth.is_some_and(|start| self.expr_depth >= start))
            && let Expr::Identifier(id) = expr
            && !id.value.starts_with('@')
            && let Some(frame) = self.on_frames.last()
        {
            let key = vec![id.value.to_lowercase()];
            let base = frame
                .scope_start
                .max(*self.grouping_bases.last().unwrap_or(&0));
            for scope in self.scopes[base..].iter().rev() {
                if scope.unknown_source {
                    break;
                }
                if let Some((source, column)) = scope.source_column(expr) {
                    let (qualifiers, columns) = &scope.sources[source];
                    if let Some(qualifier) = qualifiers.first() {
                        let mut ids = qualifier
                            .iter()
                            .map(|name| Ident::with_quote('"', name))
                            .collect::<Vec<_>>();
                        ids.push(Ident::with_quote('"', &columns[column].0));
                        *expr = Expr::CompoundIdentifier(ids);
                    }
                    break;
                }
                if scope.columns.contains_key(&key) {
                    break; // Preserve an ambiguous local binding for the binder.
                }
            }
        }
        let column = |value: &Expr| self.column_type(value);
        if let Err(error) = crate::unary_operator::check(expr, self.parameters, &column) {
            return ControlFlow::Break(error.message);
        }
        if let Err(error) = crate::nullif::lower_currency(expr, self.parameters, &column) {
            return ControlFlow::Break(error);
        }
        crate::money_arithmetic::lower(expr, self.parameters, &column);
        crate::decimal_division::lower(expr, self.parameters, &column);
        if let Err(error) = crate::left_right::lower(expr, self.parameters, &column) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::replicate::lower(expr, self.parameters, &column) {
            return ControlFlow::Break(error);
        }
        crate::money_compare::lower(expr, self.parameters, &column);
        crate::money_results::lower(expr, self.parameters, &column);
        if let Err(error) = crate::money_format::lower(expr, self.parameters, &column)
            .and_then(|_| crate::datalength::lower(expr, self.parameters, &column))
        {
            return ControlFlow::Break(error);
        }
        // Datepart units are syntax. Skip only the immediate first argument,
        // allowing a same-named column in any later argument to bind normally.
        if let Expr::Function(function) = expr
            && let [ObjectNamePart::Identifier(name)] = function.name.0.as_slice()
            && matches!(
                name.value.to_ascii_lowercase().as_str(),
                "datepart" | "datename" | "dateadd" | "datediff" | "datediff_big"
            )
            && matches!(function.parameters, FunctionArguments::None)
            && let FunctionArguments::List(args) = &function.args
            && matches!(
                args.args.first(),
                Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(
                    Expr::Identifier(_)
                )))
            )
        {
            self.syntax_unit_depth = Some(self.expr_depth + 1);
        }
        if let Expr::CompoundIdentifier(ids) = expr
            && let Some(frame) = self.on_frames.last()
        {
            let key = ids
                .iter()
                .map(|id| id.value.to_lowercase())
                .collect::<Vec<_>>();
            let visible = &self.scopes[frame.scope_start..];
            if !visible
                .iter()
                .any(|scope| scope.unknown_source || scope.columns.contains_key(&key))
                && self.scopes[..frame.scope_start]
                    .iter()
                    .any(|scope| scope.columns.contains_key(&key))
            {
                let name = ids
                    .iter()
                    .map(|id| id.value.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
                return ControlFlow::Break(format!(
                    "The multi-part identifier \"{name}\" could not be bound."
                ));
            }
        }
        if matches!(
            expr,
            Expr::Subquery(_) | Expr::Exists { .. } | Expr::InSubquery { .. }
        ) {
            self.expression_queries.push(self.grouping_bases.len());
        }
        if let Expr::Function(function) = expr
            && let Some(WindowType::WindowSpec(spec)) = &mut function.over
            && let Some(scope) = self.scopes.last()
        {
            for value in &mut spec.partition_by {
                let mut typed = value.clone();
                let _ = VisitMut::visit(
                    &mut typed,
                    &mut Annotate {
                        scope,
                        queries: 0,
                        datetime_only: true,
                        syntax: DatepartSyntax::default(),
                    },
                );
                if crate::variant_compare::known(&typed) {
                    *value = crate::variant_compare::key(typed);
                } else if crate::datetimeoffset_compare::scale(&typed, self.parameters).is_some() {
                    *value = crate::datetimeoffset_compare::key(typed);
                }
            }
        }
        if let Expr::Function(function) = expr
            && function.name.to_string().eq_ignore_ascii_case("DATEADD")
            && let FunctionArguments::List(args) = &mut function.args
            && let Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(value))) = args.args.get_mut(2)
        {
            let base = self
                .grouping_bases
                .last()
                .copied()
                .unwrap_or(0)
                .max(self.on_frames.last().map_or(0, |frame| frame.scope_start));
            let _ = value.visit(&mut TemporalArguments {
                scopes: &self.scopes[base..],
                queries: 0,
                syntax: DatepartSyntax::default(),
            });
        }
        // Carry bound column types into the scalar translator, which chooses
        // numeric operators versus T-SQL character concatenation from types.
        // Annotation only copies column references, never their expressions.
        if arithmetic(expr)
            && let Some(scope) = self.scopes.last()
        {
            let _ = expr.visit(&mut Annotate {
                scope,
                queries: 0,
                datetime_only: false,
                syntax: DatepartSyntax::default(),
            });
        }
        if (crate::datetime2_compare::is_comparison(expr)
            || crate::expression_metadata::conditional::candidate(expr))
            && let Some(scope) = self.scopes.last()
        {
            let _ = expr.visit(&mut Annotate {
                scope,
                queries: 0,
                datetime_only: true,
                syntax: DatepartSyntax::default(),
            });
        }
        if let Expr::Function(function) = expr
            && function
                .name
                .to_string()
                .eq_ignore_ascii_case("percentile_disc")
            && let Some(scope) = self.scopes.last()
        {
            for order in &mut function.within_group {
                let _ = VisitMut::visit(
                    &mut order.expr,
                    &mut Annotate {
                        scope,
                        queries: 0,
                        datetime_only: false,
                        syntax: DatepartSyntax::default(),
                    },
                );
            }
        }
        if let Expr::Function(function) = expr
            && matches!(
                function.name.to_string().to_ascii_uppercase().as_str(),
                "COUNT"
                    | "COUNT_BIG"
                    | "APPROX_COUNT_DISTINCT"
                    | "SUM"
                    | "AVG"
                    | "MIN"
                    | "MAX"
                    | "STDEV"
                    | "STDEVP"
                    | "VAR"
                    | "VARP"
                    | "TODATETIMEOFFSET"
                    | "SWITCHOFFSET"
                    | "FIRST_VALUE"
                    | "LAST_VALUE"
                    | "LAG"
                    | "LEAD"
                    | "NTILE"
            )
            && let FunctionArguments::List(args) = &mut function.args
            && let Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(value))) = args.args.first_mut()
            && let Some(scope) = self.scopes.last()
        {
            if function.name.to_string().eq_ignore_ascii_case("NTILE") {
                struct LocalColumn<'a> {
                    scope: &'a Scope,
                    queries: usize,
                }
                impl Visitor for LocalColumn<'_> {
                    type Break = String;
                    fn pre_visit_query(&mut self, _: &Query) -> ControlFlow<String> {
                        self.queries += 1;
                        ControlFlow::Continue(())
                    }
                    fn post_visit_query(&mut self, _: &Query) -> ControlFlow<String> {
                        self.queries -= 1;
                        ControlFlow::Continue(())
                    }
                    fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<String> {
                        if self.queries == 0 && self.scope.column(expr).is_some() {
                            ControlFlow::Break(expr.to_string())
                        } else {
                            ControlFlow::Continue(())
                        }
                    }
                }
                if let ControlFlow::Break(name) =
                    Visit::visit(&*value, &mut LocalColumn { scope, queries: 0 })
                {
                    return ControlFlow::Break(format!(
                        "The reference to column '{name}' is not allowed in an argument to the NTILE function. Only references to columns at an outer scope or standalone expressions and subqueries are allowed here."
                    ));
                }
            }
            let _ = value.visit(&mut Annotate {
                scope,
                queries: 0,
                datetime_only: false,
                syntax: DatepartSyntax::default(),
            });
        }
        ControlFlow::Continue(())
    }
}
struct Annotate<'a> {
    scope: &'a Scope,
    queries: usize,
    datetime_only: bool,
    syntax: DatepartSyntax,
}
impl VisitorMut for Annotate<'_> {
    type Break = ();
    fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
        self.syntax.enter(expr);
        ControlFlow::Continue(())
    }
    fn pre_visit_query(&mut self, _: &mut Query) -> ControlFlow<()> {
        self.queries += 1;
        ControlFlow::Continue(())
    }
    fn post_visit_query(&mut self, _: &mut Query) -> ControlFlow<()> {
        self.queries -= 1;
        ControlFlow::Continue(())
    }
    fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
        let unit = self.syntax.leave();
        if self.queries == 0
            && !unit
            && let Some(data_type) = self.scope.column(expr)
            && (!self.datetime_only
                || matches!(data_type, DataType::Time(..))
                || crate::variant_pack::is_variant(&data_type)
                || crate::datetimeoffset_cast::scale(&data_type)
                    .ok()
                    .flatten()
                    .is_some()
                || crate::datetime2_cast::scale(&data_type)
                    .ok()
                    .flatten()
                    .is_some())
        {
            *expr = Expr::Cast {
                kind: CastKind::Cast,
                expr: Box::new(expr.clone()),
                data_type,
                format: None,
            };
        }
        ControlFlow::Continue(())
    }
}

// Resolve temporal arguments against the same visibility boundaries as grouping.
// Missing/ambiguous local bindings shadow outer names and stay with the binder.
struct TemporalArguments<'a> {
    scopes: &'a [Scope],
    queries: usize,
    syntax: DatepartSyntax,
}
impl VisitorMut for TemporalArguments<'_> {
    type Break = ();
    fn pre_visit_query(&mut self, _: &mut Query) -> ControlFlow<()> {
        self.queries += 1;
        ControlFlow::Continue(())
    }
    fn post_visit_query(&mut self, _: &mut Query) -> ControlFlow<()> {
        self.queries -= 1;
        ControlFlow::Continue(())
    }
    fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
        self.syntax.enter(expr);
        ControlFlow::Continue(())
    }
    fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
        let unit = self.syntax.leave();
        if self.queries != 0 || unit {
            return ControlFlow::Continue(());
        }
        let key: Vec<String> = match expr {
            Expr::Identifier(id) if !id.value.starts_with('@') => vec![id.value.to_lowercase()],
            Expr::CompoundIdentifier(ids) => ids.iter().map(|id| id.value.to_lowercase()).collect(),
            _ => return ControlFlow::Continue(()),
        };
        let qualifier = &key[..key.len() - 1];
        for scope in self.scopes.iter().rev() {
            if let Some(data_type) = scope.column(expr) {
                if crate::datetime2_cast::scale(&data_type)
                    .ok()
                    .flatten()
                    .is_some()
                    || crate::datetimeoffset_cast::scale(&data_type)
                        .ok()
                        .flatten()
                        .is_some()
                    || matches!(data_type, DataType::Time(_, TimezoneInfo::None))
                {
                    *expr = Expr::Cast {
                        kind: CastKind::Cast,
                        expr: Box::new(expr.clone()),
                        data_type,
                        format: None,
                    };
                }
                break;
            }
            if scope.unknown_source
                || scope.columns.contains_key(&key)
                || (!qualifier.is_empty()
                    && scope
                        .sources
                        .iter()
                        .any(|(names, _)| names.iter().any(|n| n == qualifier)))
            {
                break;
            }
        }
        ControlFlow::Continue(())
    }
}

// First datepart arguments are syntax even when a source column has that name.
// Track just that immediate leaf, allowing identically named value arguments to bind.
#[derive(Default)]
struct DatepartSyntax {
    depth: usize,
    unit_depth: Option<usize>,
}
impl DatepartSyntax {
    fn enter(&mut self, expr: &Expr) {
        self.depth += 1;
        if let Expr::Function(f) = expr
            && matches!(
                f.name.to_string().to_ascii_lowercase().as_str(),
                "dateadd" | "datepart" | "datename" | "datediff" | "datediff_big"
            )
            && let FunctionArguments::List(args) = &f.args
            && matches!(
                args.args.first(),
                Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(
                    Expr::Identifier(_)
                )))
            )
        {
            self.unit_depth = Some(self.depth + 1);
        }
    }
    fn leave(&mut self) -> bool {
        let unit = self.unit_depth == Some(self.depth);
        if unit {
            self.unit_depth = None;
        }
        self.depth -= 1;
        unit
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlparser::parser::Parser;
    fn parse(sql: &str) -> Statement {
        Parser::parse_sql(&crate::dialect::ServerDialect, sql)
            .unwrap()
            .remove(0)
    }
    #[test]
    fn explicit_relations_preserve_components_and_cte_shadowing() {
        let money = DataType::Custom(ObjectName::from(vec![Ident::new("money")]), vec![]);
        let relations = [
            (
                vec!["temp".into(), "main".into(), "a.b".into()],
                vec![("n".into(), Some(money.clone()))],
            ),
            (vec!["x".into()], vec![("n".into(), Some(money))]),
        ]
        .into_iter()
        .collect();
        for (sql, formatted) in [
            (
                "SELECT CAST(t.n AS NVARCHAR(20)) FROM temp.main.[a.b] t",
                true,
            ),
            (
                "SELECT CAST(t.n AS NVARCHAR(20)) FROM temp.main.a.b t",
                false,
            ),
            (
                "WITH x AS (SELECT CAST(1 AS INT) AS n) SELECT CAST(n AS NVARCHAR(20)) FROM x",
                false,
            ),
        ] {
            let mut statement = parse(sql);
            resolve_with_relations(
                &Snapshot::new(),
                &relations,
                &mut statement,
                &HashMap::new(),
            )
            .unwrap();
            assert_eq!(
                statement.to_string().contains("__msduck_money"),
                formatted,
                "{statement}"
            );
        }
    }
    #[test]
    fn snapshot_binding_preserves_shadowing_and_error_selection() {
        let mut catalog = Snapshot::new();
        catalog.insert(("dbo".into(), "t".into()), Err("unavailable t".into()));
        catalog.insert(("dbo".into(), "u".into()), Err("unavailable u".into()));
        for sql in [
            "WITH t AS (SELECT CAST(1 AS SMALLINT) AS n) SELECT SUM(n) FROM t",
            "WITH t AS (SELECT CAST(1 AS SMALLINT) AS n) SELECT SUM(x.n) FROM (SELECT n FROM t) x",
        ] {
            let mut statement = parse(sql);
            resolve(&catalog, &mut statement, &Default::default()).unwrap();
            assert!(statement.to_string().contains("SMALLINT"));
        }
        for (sql, expected) in [
            ("SELECT SUM(n) FROM dbo.t", "unavailable t"),
            ("SELECT SUM(t.n) FROM u CROSS JOIN t", "unavailable u"),
            (
                "WITH t AS (SELECT 1 AS n) SELECT SUM(n) FROM dbo.t",
                "unavailable t",
            ),
        ] {
            assert_eq!(
                resolve(&catalog, &mut parse(sql), &Default::default()),
                Err(expected.into())
            );
        }
        let mut unknown = parse("SELECT SUM(n) FROM missing");
        let original = unknown.clone();
        resolve(&catalog, &mut unknown, &Default::default()).unwrap();
        assert_eq!(unknown, original);
    }
}
