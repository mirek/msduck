//! Nonexecuting preparation descriptions over explicit catalog declarations.
use crate::{engine::Session, parameter::Parameter, tds};
use anyhow::{Result, bail, ensure};
use msduck_core::{catalog::TypeMetadata, types::Type as SqlType, value::Value};
use msduck_sql::{binding_scope::Scope, catalog_snapshot::CatalogSnapshot, projection};
use sqlparser::ast::{
    DataType, ExactNumberInfo, Expr, FunctionArg, FunctionArgExpr, FunctionArguments, Query,
    Statement, VisitMut, VisitorMut,
};
use std::collections::HashMap;
use std::ops::ControlFlow;

// Every contributing branch must have an explicit DATETIME2 declaration.
// In particular, a known branch cannot conceal an unknown or higher-precedence
// operand. ISNULL/NULLIF use the first declaration through conditional::values.
fn datetime2_declaration(expression: &Expr, parameters: &HashMap<String, Parameter>) -> Option<u8> {
    use msduck_sql::expression_metadata::conditional;
    if conditional::candidate(expression) {
        let scales = conditional::values(expression)
            .into_iter()
            .filter(|value| !conditional::literal_null(value))
            .map(|value| datetime2_declaration(value, parameters))
            .collect::<Option<Vec<_>>>()?;
        return scales.into_iter().max();
    }
    match expression {
        Expr::Nested(value) => datetime2_declaration(value, parameters),
        Expr::Cast { data_type, .. }
        | Expr::Convert {
            data_type: Some(data_type),
            ..
        } => msduck_sql::datetime2_cast::scale(data_type).ok().flatten(),
        Expr::Identifier(id) if id.value.starts_with('@') => {
            msduck_sql::datetime2_cast::scale(&parameters.get(&id.value.to_lowercase())?.ast_type())
                .ok()
                .flatten()
        }
        _ => None,
    }
}

// Some(true) is a proven variant, Some(false) a supported integral/BIT
// operand. Unknown and other families cannot establish variant precedence.
fn variant_declaration(
    expression: &Expr,
    scope: &Scope,
    parameters: &HashMap<String, Parameter>,
) -> Option<bool> {
    use msduck_sql::expression_metadata::conditional;
    if sqlparser::ast::visit_expressions(expression, |node| {
        if matches!(node, Expr::Function(f) if matches!(f.name.to_string().to_ascii_lowercase().as_str(), "count" | "count_big")) {
            ControlFlow::Break(())
        } else { ControlFlow::Continue(()) }
    }).is_break() {
        return None;
    }
    if conditional::candidate(expression) {
        let values = conditional::values(expression)
            .into_iter()
            .filter(|value| !conditional::literal_null(value))
            .map(|value| variant_declaration(value, scope, parameters))
            .collect::<Option<Vec<_>>>()?;
        return (!values.is_empty()).then(|| values.into_iter().any(|variant| variant));
    }
    match expression {
        Expr::Nested(expr) => return variant_declaration(expr, scope, parameters),
        Expr::Cast { data_type, .. }
        | Expr::Convert {
            data_type: Some(data_type),
            ..
        } if msduck_sql::variant_pack::is_variant(data_type) => return Some(true),
        Expr::Function(f)
            if matches!(
                f.name.to_string().to_ascii_lowercase().as_str(),
                "min" | "max"
            ) && f.over.is_none()
                && msduck_sql::aggregate::validate(f).is_ok() =>
        {
            let FunctionArguments::List(args) = &f.args else {
                return None;
            };
            let [FunctionArg::Unnamed(FunctionArgExpr::Expr(value))] = args.args.as_slice() else {
                return None;
            };
            return variant_declaration(value, scope, parameters);
        }
        _ => {}
    }
    if msduck_sql::case_types::integer_rank(expression, parameters).is_some()
        || msduck_sql::case_types::is_bit(expression, parameters)
    {
        return Some(false);
    }
    let ids = match expression {
        Expr::Identifier(id) if id.value.starts_with('@') => {
            return scope
                .parameters
                .get(&id.value.to_lowercase())
                .and_then(|info| info.system_type_id)
                .and_then(|id| match id {
                    98 => Some(true),
                    48 | 52 | 56 | 127 | 104 => Some(false),
                    _ => None,
                });
        }
        Expr::Identifier(id) => vec![id],
        Expr::CompoundIdentifier(ids) => ids.iter().collect(),
        _ => return None,
    };
    msduck_sql::binding_scope::resolve(&ids, &[], &scope.rows)
        .and_then(|field| field.info.as_ref())
        .and_then(|info| info.system_type_id)
        .and_then(|id| match id {
            98 => Some(true),
            48 | 52 | 56 | 127 | 104 => Some(false),
            _ => None,
        })
}

// A captured ROW_NUMBER over declared variant conditional keys has a BIGINT
// declaration. Preserve the original key binding proof instead of assigning a
// type to every function name or erasing ORDER BY columns in a metadata clone.
fn variant_row_number(
    expression: &Expr,
    scope: &Scope,
    parameters: &HashMap<String, Parameter>,
) -> bool {
    let Expr::Function(function) = expression else {
        return false;
    };
    if !function.name.to_string().eq_ignore_ascii_case("ROW_NUMBER")
        || function.parameters != FunctionArguments::None
        || function.filter.is_some()
        || function.null_treatment.is_some()
        || !function.within_group.is_empty()
        || function.uses_odbc_syntax
        || !matches!(&function.args, FunctionArguments::List(args) if args.args.is_empty() && args.clauses.is_empty() && args.duplicate_treatment.is_none())
    {
        return false;
    }
    let Some(sqlparser::ast::WindowType::WindowSpec(window)) = &function.over else {
        return false;
    };
    if window.window_name.is_some() || window.window_frame.is_some() || window.order_by.is_empty() {
        return false;
    }
    let declared = |expr: &Expr| variant_declaration(expr, scope, parameters).is_some();
    let has_column = |expr: &Expr| {
        sqlparser::ast::visit_expressions(expr, |node| {
            let ids = match node {
                Expr::Identifier(id) if !id.value.starts_with('@') => vec![id],
                Expr::CompoundIdentifier(ids) => ids.iter().collect(),
                _ => return ControlFlow::Continue(()),
            };
            if msduck_sql::binding_scope::resolve(&ids, &[], &scope.rows)
                .and_then(|field| field.info.as_ref())
                .and_then(|info| info.system_type_id)
                .is_some()
            {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        })
        .is_break()
    };
    window.partition_by.iter().all(declared)
        && window.order_by.iter().all(|key| {
            key.options.nulls_first.is_none()
                && key.with_fill.is_none()
                && declared(&key.expr)
                && has_column(&key.expr)
        })
        && window
            .partition_by
            .iter()
            .chain(window.order_by.iter().map(|key| &key.expr))
            .any(|key| {
                msduck_sql::expression_metadata::conditional::candidate(key)
                    && variant_declaration(key, scope, parameters) == Some(true)
            })
}

// This clone is used only for declaration inference, never execution. Native
// validation has already checked the original seed expression and function.
fn expression_declarations(
    query: &Query,
    parameters: &HashMap<String, Parameter>,
    catalog: &CatalogSnapshot,
    outer: &Scope,
) -> Query {
    struct Declare<'a> {
        parameters: &'a HashMap<String, Parameter>,
        catalog: &'a CatalogSnapshot,
        outer: &'a Scope,
        scopes: Vec<Scope>,
    }
    impl VisitorMut for Declare<'_> {
        type Break = ();
        fn pre_visit_query(&mut self, query: &mut Query) -> ControlFlow<()> {
            let inherited = self.scopes.last().unwrap_or(self.outer);
            let scope = projection::scopes(self.catalog, query, inherited).body;
            self.scopes.push(scope);
            ControlFlow::Continue(())
        }
        fn post_visit_query(&mut self, _: &mut Query) -> ControlFlow<()> {
            self.scopes.pop();
            ControlFlow::Continue(())
        }
        fn pre_visit_expr(&mut self, expression: &mut Expr) -> ControlFlow<()> {
            // COUNT's name alone cannot prove operand acceptance. Preserve its
            // declaration barriers even when enclosed in CASE/arithmetic. The
            // original projection already supplies proven COUNT declarations.
            if sqlparser::ast::visit_expressions(expression, |node| {
                if matches!(node, Expr::Function(f) if matches!(f.name.to_string().to_ascii_lowercase().as_str(), "count" | "count_big")) {
                    ControlFlow::Break(())
                } else { ControlFlow::Continue(()) }
            }).is_break() {
                return ControlFlow::Continue(());
            }
            if msduck_sql::expression_metadata::conditional::literal_null(expression) {
                return ControlFlow::Continue(());
            }
            if msduck_sql::expression_metadata::conditional::candidate(expression)
                && variant_declaration(
                    expression,
                    self.scopes.last().unwrap_or(self.outer),
                    self.parameters,
                ) == Some(true)
            {
                *expression = Expr::Convert {
                    is_try: false,
                    expr: Box::new(Expr::Value(sqlparser::ast::Value::Null.into())),
                    data_type: Some(DataType::Custom(
                        sqlparser::ast::ObjectName::from(vec![sqlparser::ast::Ident::new(
                            "SQL_VARIANT",
                        )]),
                        vec![],
                    )),
                    charset: None,
                    target_before_value: true,
                    styles: vec![],
                };
                return ControlFlow::Continue(());
            }
            let mut numeric = expression.clone();
            msduck_sql::case_types::lower(&mut numeric, self.parameters);
            let kind = msduck_sql::case_types::integer_rank(expression, self.parameters)
                .or_else(|| {
                    let Expr::BinaryOp { left, op, right } = expression else {
                        return None;
                    };
                    if !matches!(
                        op,
                        sqlparser::ast::BinaryOperator::BitwiseAnd
                            | sqlparser::ast::BinaryOperator::BitwiseOr
                            | sqlparser::ast::BinaryOperator::BitwiseXor
                    ) {
                        return None;
                    }
                    // SQL Server permits one BIT operand; the other integer
                    // operand determines the declared result width.
                    if msduck_sql::case_types::is_bit(left, self.parameters) {
                        msduck_sql::case_types::integer_rank(right, self.parameters)
                    } else if msduck_sql::case_types::is_bit(right, self.parameters) {
                        msduck_sql::case_types::integer_rank(left, self.parameters)
                    } else {
                        None
                    }
                })
                .map(|rank| match rank {
                    0 => DataType::TinyInt(None),
                    1 => DataType::SmallInt(None),
                    2 => DataType::Int(None),
                    _ => DataType::BigInt(None),
                })
                .or_else(|| {
                    msduck_sql::case_types::is_bit(expression, self.parameters)
                        .then_some(DataType::Bit(None))
                })
                .or_else(|| {
                    datetime2_declaration(expression, self.parameters).and_then(|scale| {
                        Some(msduck_sql::sql_type::ast(SqlType::DateTime2(
                            msduck_core::types::Scale::new(scale).ok()?,
                        )))
                    })
                })
                .or_else(|| {
                    msduck_sql::expression_metadata::storage::kind(
                        &numeric,
                        self.parameters,
                        &|_| None,
                    )
                })
                .or_else(
                    || match msduck_sql::result_types::expression_type(expression)? {
                        msduck_sql::result_types::ResultType::Character { family, length } => {
                            let kind =
                                msduck_core::character::CharacterType::new(family, length).ok()?;
                            Some(msduck_sql::sql_type::ast(SqlType::Character(kind)))
                        }
                        msduck_sql::result_types::ResultType::Time(scale) => {
                            Some(msduck_sql::sql_type::ast(SqlType::Time(
                                msduck_core::types::Scale::new(scale).ok()?,
                            )))
                        }
                        msduck_sql::result_types::ResultType::Money(kind) => {
                            Some(msduck_sql::sql_type::ast(match kind {
                                msduck_core::money::MoneyType::Money => SqlType::Money,
                                msduck_core::money::MoneyType::SmallMoney => SqlType::SmallMoney,
                            }))
                        }
                    },
                );
            if let Some(kind) = kind {
                *expression = Expr::Convert {
                    is_try: false,
                    expr: Box::new(Expr::Value(sqlparser::ast::Value::Null.into())),
                    data_type: Some(kind),
                    charset: None,
                    target_before_value: true,
                    styles: vec![],
                };
                return ControlFlow::Continue(());
            }
            if let Expr::Function(f) = expression {
                let name = f.name.to_string().to_ascii_lowercase();
                let fixed = if name == "newid"
                    && matches!(&f.args, FunctionArguments::List(a) if a.args.is_empty())
                {
                    Some(DataType::Uuid)
                } else if matches!(name.as_str(), "stdev" | "stdevp" | "var" | "varp")
                    && msduck_sql::aggregate::validate(f).is_ok()
                {
                    Some(DataType::Double(ExactNumberInfo::None))
                } else if matches!(&f.args, FunctionArguments::List(a) if a.clauses.is_empty() && a.duplicate_treatment.is_none() && a.args.iter().all(|arg| matches!(arg, FunctionArg::Unnamed(FunctionArgExpr::Expr(_)))))
                    && f.over.is_none()
                    && f.filter.is_none()
                    && f.within_group.is_empty()
                    && f.null_treatment.is_none()
                    && matches!(f.parameters, FunctionArguments::None)
                    && !f.uses_odbc_syntax
                {
                    // Fixed return declarations, checked without evaluating
                    // catalog lookups, dates, identity state or JSON operands.
                    match name.as_str() {
                        "eomonth" | "datefromparts" => Some(DataType::Date),
                        "object_id" | "schema_id" | "type_id" | "columnproperty"
                        | "json_path_exists" => Some(DataType::Int(None)),
                        "schema_name" | "type_name" | "col_name" | "object_name"
                        | "object_schema_name" => {
                            Some(msduck_sql::sql_type::ast(SqlType::Character(
                                msduck_core::character::CharacterType::new(
                                    msduck_core::character::Family::Nvarchar,
                                    msduck_core::character::Length::Bounded(128),
                                )
                                .expect("sysname declaration"),
                            )))
                        }
                        "sql_variant_property" => Some(DataType::Custom(
                            sqlparser::ast::ObjectName::from(vec![sqlparser::ast::Ident::new(
                                "SQL_VARIANT",
                            )]),
                            vec![],
                        )),
                        "ident_current" | "ident_seed" | "ident_incr" => {
                            Some(DataType::Numeric(ExactNumberInfo::PrecisionAndScale(38, 0)))
                        }
                        _ => None,
                    }
                } else {
                    None
                };
                if let Some(kind) = fixed {
                    *expression = Expr::Convert {
                        is_try: false,
                        expr: Box::new(Expr::Value(sqlparser::ast::Value::Null.into())),
                        data_type: Some(kind),
                        charset: None,
                        target_before_value: true,
                        styles: vec![],
                    };
                    return ControlFlow::Continue(());
                }
            }
            if let Expr::Function(f) = expression
                && f.name.to_string().eq_ignore_ascii_case("rand")
                && matches!(&f.args, FunctionArguments::List(a) if matches!(a.args.as_slice(), [] | [FunctionArg::Unnamed(FunctionArgExpr::Expr(_))]) && a.clauses.is_empty() && a.duplicate_treatment.is_none())
                && f.over.is_none()
                && f.filter.is_none()
                && f.within_group.is_empty()
                && f.null_treatment.is_none()
                && matches!(f.parameters, FunctionArguments::None)
                && !f.uses_odbc_syntax
            {
                *expression = Expr::Convert {
                    is_try: false,
                    expr: Box::new(Expr::Value(sqlparser::ast::Value::Null.into())),
                    data_type: Some(DataType::Double(ExactNumberInfo::None)),
                    charset: None,
                    target_before_value: true,
                    styles: vec![],
                };
            }
            ControlFlow::Continue(())
        }
    }
    let mut declared = query.clone();
    let _ = declared.visit(&mut Declare {
        parameters,
        catalog,
        outer,
        scopes: vec![],
    });
    // A bare projected NULL has an INT declaration. Do not type NULL while
    // traversing function arguments: COUNT(NULL) must keep its compile barrier.
    if let sqlparser::ast::SetExpr::Select(select) = declared.body.as_mut() {
        for item in &mut select.projection {
            let expression = match item {
                sqlparser::ast::SelectItem::UnnamedExpr(expression)
                | sqlparser::ast::SelectItem::ExprWithAlias {
                    expr: expression, ..
                } => expression,
                _ => continue,
            };
            if msduck_sql::expression_metadata::conditional::literal_null(expression) {
                *expression = Expr::Convert {
                    is_try: false,
                    expr: Box::new(expression.clone()),
                    data_type: Some(DataType::Int(None)),
                    charset: None,
                    target_before_value: true,
                    styles: vec![],
                };
            }
        }
    }
    declared
}

pub(super) struct Description {
    pub prefix: Vec<u8>,
    pub status: i32,
    pub accepted: bool,
}

fn wire(info: &TypeMetadata) -> Option<tds::Type> {
    use tds::Type;
    let width = || u16::try_from(info.max_length?).ok();
    let unicode = || {
        let bytes = width()?;
        (bytes.is_multiple_of(2) && bytes > 0 && bytes <= 8000).then_some(bytes / 2)
    };
    Some(match info.system_type_id? {
        48 => Type::Int(1),
        52 => Type::Int(2),
        56 => Type::Int(4),
        127 => Type::Int(8),
        104 => Type::Bit,
        59 => Type::Float(4),
        62 => Type::Float(8),
        106 | 108 => {
            let declaration =
                msduck_core::types::DecimalType::new(info.precision?, info.scale?).ok()?;
            Type::Decimal(declaration.precision(), declaration.scale())
        }
        60 => Type::Money(8),
        122 => Type::Money(4),
        36 => Type::Guid,
        40 => Type::Date,
        41 => Type::Time(msduck_core::types::Scale::new(info.scale?).ok()?.get()),
        58 => Type::LegacyDateTime(4),
        61 => Type::DateTime,
        42 => Type::DateTime2(msduck_core::types::Scale::new(info.scale?).ok()?.get()),
        43 => Type::DateTimeOffset(msduck_core::types::Scale::new(info.scale?).ok()?.get()),
        98 => Type::Variant,
        167 if info.max_length == Some(-1) => Type::Varchar(u16::MAX),
        167 => Type::Varchar(width().filter(|n| (1..=8000).contains(n))?),
        175 => Type::Char(width().filter(|n| (1..=8000).contains(n))?),
        231 if info.max_length == Some(-1) => Type::Text,
        231 => Type::Nvarchar(unicode()?),
        239 => Type::Nchar(unicode()?),
        165 if info.max_length == Some(-1) => Type::Binary,
        165 => Type::Varbinary(width().filter(|n| (1..=8000).contains(n))?),
        173 => Type::FixedBinary(width().filter(|n| (1..=8000).contains(n))?),
        _ => return None,
    })
}

// Additional captured ORDER shapes use the already-proven output width, while
// resolving keys against original explicit row scopes. Unknowns remain barriers.
fn prepared_order(
    catalog: &msduck_sql::catalog_snapshot::CatalogSnapshot,
    query: &Query,
    outer: &Scope,
    fields: &[msduck_sql::binding_scope::Field],
    parameters: &HashMap<String, Parameter>,
) -> projection::order::Plan {
    use sqlparser::ast::*;
    let original = projection::order::infer(catalog, query, outer);
    if !matches!(original, projection::order::Plan::Unknown(_)) {
        return original;
    }
    let infer = || -> Option<Vec<u16>> {
        let order = query.order_by.as_ref()?;
        let OrderByKind::Expressions(keys) = &order.kind else {
            return None;
        };
        if keys.is_empty()
            || keys.len() > usize::from(u16::MAX) / 2
            || query.for_clause.is_some()
            || order.interpolate.is_some()
            || keys.iter().any(|key| {
                key.with_fill.is_some()
                    || key.options.nulls_first.is_some()
                    || matches!(key.options.sort, Some(OrderBySort::Using(_)))
            })
            || fields.is_empty()
            || fields.len() >= usize::from(u16::MAX)
            || fields.iter().any(|field| field.info.is_none())
        {
            return None;
        }
        let mut expanded = query.clone();
        projection::expand_stars(catalog, &mut expanded, outer)?;
        let SetExpr::Select(select) = expanded.body.as_ref() else {
            return None;
        };
        let scope = projection::scopes(catalog, &expanded, outer).body;
        let expressions = select
            .projection
            .iter()
            .map(|item| match item {
                SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => {
                    Some(expr)
                }
                _ => None,
            })
            .collect::<Option<Vec<_>>>()?;
        if expressions.len() != fields.len() {
            return None;
        }
        fn column<'a>(
            expr: &Expr,
            scope: &'a Scope,
        ) -> Option<&'a msduck_sql::binding_scope::Field> {
            let ids = match expr {
                Expr::Nested(expr) => return column(expr, scope),
                Expr::Identifier(id) if !id.value.starts_with('@') => vec![id],
                Expr::CompoundIdentifier(ids) => ids.iter().collect(),
                _ => return None,
            };
            let field = msduck_sql::binding_scope::resolve(&ids, &[], &scope.rows)?;
            field.info.as_ref()?;
            Some(field)
        }
        let projected_key = |expr: &Expr| {
            if column(expr, &scope).is_some() {
                return true;
            }
            if msduck_sql::expression_metadata::conditional::candidate(expr)
                && variant_declaration(expr, &scope, parameters) == Some(true)
            {
                return true;
            }
            match expr {
                Expr::Cast {
                    expr, data_type, ..
                }
                | Expr::Convert {
                    expr,
                    data_type: Some(data_type),
                    ..
                } if catalog.cast_info(data_type).is_some_and(|info| {
                    matches!(info.system_type_id, Some(48 | 52 | 56 | 127))
                }) && column(expr, &scope).is_some_and(|field| {
                    field.info.as_ref().and_then(|info| info.system_type_id) == Some(98)
                }) =>
                {
                    true
                }
                Expr::Cast {
                    expr, data_type, ..
                }
                | Expr::Convert {
                    expr,
                    data_type: Some(data_type),
                    ..
                } if msduck_sql::variant_pack::is_variant(data_type) => column(expr, &scope)
                    .is_some_and(|field| {
                        matches!(
                            field.info.as_ref().and_then(|i| i.system_type_id),
                            Some(48 | 52 | 56 | 127 | 104)
                        )
                    }),
                Expr::Function(f)
                    if f.name.to_string().eq_ignore_ascii_case("SUM")
                        && msduck_sql::aggregate::validate(f).is_ok()
                        && (f.over.is_some()
                            || matches!(&select.group_by, GroupByExpr::Expressions(keys,_) if !keys.is_empty())) =>
                {
                    let FunctionArguments::List(args) = &f.args else {
                        return false;
                    };
                    matches!(args.args.as_slice(), [FunctionArg::Unnamed(FunctionArgExpr::Expr(expr))]
                        if column(expr,&scope).is_some_and(|field| matches!(field.info.as_ref().and_then(|i|i.system_type_id),Some(48|52|56|127|59|62|106|108|60|122))))
                }
                _ => false,
            }
        };
        let mut ordinals = Vec::with_capacity(keys.len());
        for key in keys {
            let ordinal = match &key.expr {
                Expr::Value(value) => match &value.value {
                    Value::Number(number, false) if number.bytes().all(|b| b.is_ascii_digit()) => {
                        let index = number.parse::<usize>().ok()?;
                        if index == 0 || index > fields.len() {
                            return None;
                        }
                        Some(index - 1)
                    }
                    _ => return None,
                },
                Expr::Identifier(name) => {
                    let matching = fields
                        .iter()
                        .enumerate()
                        .filter(|(_, field)| field.name.eq_ignore_ascii_case(&name.value))
                        .map(|(index, _)| index)
                        .collect::<Vec<_>>();
                    if matching.len() > 1 {
                        return None;
                    }
                    matching.first().copied()
                }
                _ => None,
            };
            if let Some(index) = ordinal {
                if !projected_key(expressions[index]) {
                    return None;
                }
                ordinals.push(u16::try_from(index + 1).ok()?);
            } else {
                let field = column(&key.expr, &scope)?;
                let projected = expressions.iter().position(|expr| {
                    column(expr, &scope).is_some_and(|other| std::ptr::eq(field, other))
                });
                if projected.is_none() && select.distinct.is_some() {
                    return None;
                }
                ordinals.push(projected.map_or(Some(0), |index| u16::try_from(index + 1).ok())?);
            }
        }
        Some(ordinals)
    };
    infer().map_or(original, projection::order::Plan::Token)
}

fn query_description(
    session: &Session,
    query: &Query,
    parameters: &HashMap<String, Parameter>,
    command: u16,
) -> Result<Vec<u8>> {
    let catalog = crate::query_catalog::snapshot(&session.db, query)?;
    let mut scope = Scope::default();
    for (name, parameter) in parameters {
        if let Some(info) = catalog.cast_info(&parameter.ast_type()) {
            scope.parameters.insert(name.to_lowercase(), info);
        }
    }
    let mut fields = projection::query_fields(&catalog, query, &scope)
        .ok_or_else(|| anyhow::anyhow!("unsupported prepared result declarations"))?;
    // Preserve original explicit VALUES declarations before operand annotation
    // lowers temporal sources into native helper calls. The annotated candidate
    // additionally supplies catalog-bound aggregate/operand declarations.
    let original_declarations = expression_declarations(query, parameters, &catalog, &scope);
    let mut declaration_query = query.clone();
    crate::aggregate_columns::annotate(&session.db, &mut declaration_query, parameters)
        .map_err(anyhow::Error::msg)?;
    for candidate in [
        original_declarations,
        expression_declarations(&declaration_query, parameters, &catalog, &scope),
    ] {
        if let Some(declared) = projection::query_fields(&catalog, &candidate, &scope) {
            ensure!(
                declared.len() == fields.len(),
                "prepared declaration alignment changed"
            );
            for (field, declaration) in fields.iter_mut().zip(declared) {
                if field.info.is_none() && declaration.info.is_some() {
                    field.info = declaration.info;
                    if field.properties.origin == msduck_core::result::Origin::Unknown {
                        field.properties = declaration.properties;
                    }
                    field.collation = declaration.collation;
                }
            }
        }
    }
    // Preparation retains fComputed for the captured CAST of an INT column
    // to SQL_VARIANT. Ordinary execution projection rules omit it for this
    // family, so derive this preparation property from the original AST and
    // explicit source declaration, never from returned payloads.
    let mut expanded = query.clone();
    if projection::expand_stars(&catalog, &mut expanded, &scope).is_some()
        && let sqlparser::ast::SetExpr::Select(select) = expanded.body.as_ref()
        && select.projection.len() == fields.len()
    {
        let source_scope = projection::scopes(&catalog, &expanded, &scope).body;
        for (field, item) in fields.iter_mut().zip(&select.projection) {
            let expression = match item {
                sqlparser::ast::SelectItem::UnnamedExpr(expr)
                | sqlparser::ast::SelectItem::ExprWithAlias { expr, .. } => expr,
                _ => continue,
            };
            if field.info.is_none() && variant_row_number(expression, &source_scope, parameters) {
                field.info = catalog.cast_info(&DataType::BigInt(None));
                if field.info.is_some() {
                    field.properties = msduck_core::result::Properties {
                        nullable: Some(true),
                        origin: msduck_core::result::Origin::Derived,
                    };
                }
            }
            if msduck_sql::expression_metadata::conditional::candidate(expression)
                && variant_declaration(expression, &source_scope, parameters) == Some(true)
                && field.info.as_ref().and_then(|info| info.system_type_id) == Some(98)
            {
                field.properties =
                    msduck_sql::result_properties::expression(expression, &[], &source_scope.rows);
            }
            if field.info.is_none()
                && variant_declaration(expression, &source_scope, parameters) == Some(true)
            {
                field.info = catalog.cast_info(&DataType::Custom(
                    sqlparser::ast::ObjectName::from(vec![sqlparser::ast::Ident::new(
                        "SQL_VARIANT",
                    )]),
                    vec![],
                ));
                if field.info.is_some()
                    && field.properties.origin == msduck_core::result::Origin::Unknown
                {
                    field.properties = msduck_sql::result_properties::expression(
                        expression,
                        &[],
                        &source_scope.rows,
                    );
                }
            }
            if let Expr::Function(function) = expression
                && (msduck_sql::expression_metadata::conditional::isnull_args(function)
                    .ok()
                    .flatten()
                    .is_some()
                    || msduck_sql::expression_metadata::conditional::coalesce_args(function)
                        .ok()
                        .flatten()
                        .is_some())
                && datetime2_declaration(expression, parameters).is_some()
                && field.info.as_ref().and_then(|info| info.system_type_id) == Some(42)
            {
                // Unlike ordinary temporal casts, the captured prepared
                // ISNULL/COALESCE retain fComputed and original NULL provenance.
                field.properties =
                    msduck_sql::result_properties::expression(expression, &[], &source_scope.rows);
            }
            if matches!(expression, Expr::Function(function) if function.name.to_string().eq_ignore_ascii_case("SQL_VARIANT_PROPERTY"))
                && field.info.as_ref().and_then(|info| info.system_type_id) == Some(98)
            {
                // The property name and runtime payload do not choose the
                // result declaration: both strings and integers are variants.
                field.properties = msduck_core::result::Properties::expression(true);
            }
            if let Expr::Cast {
                expr, data_type, ..
            } = expression
                && msduck_sql::variant_pack::is_variant(data_type)
                && field.info.as_ref().and_then(|info| info.system_type_id) == Some(98)
            {
                let ids = match expr.as_ref() {
                    Expr::Identifier(id) if !id.value.starts_with('@') => vec![id],
                    Expr::CompoundIdentifier(ids) => ids.iter().collect(),
                    _ => continue,
                };
                if msduck_sql::binding_scope::resolve(&ids, &[], &source_scope.rows)
                    .and_then(|source| source.info.as_ref())
                    .and_then(|info| info.system_type_id)
                    == Some(56)
                {
                    field.properties = msduck_core::result::Properties::expression(true);
                }
            }
        }
    }
    ensure!(
        !fields.is_empty(),
        "unsupported empty prepared result declarations"
    );
    let aligned = crate::result_metadata::Aligned::new(fields.len(), &fields, &[]);
    let columns = fields
        .iter()
        .enumerate()
        .map(|(index, field)| {
            let kind = field.info.as_ref().and_then(wire).ok_or_else(|| {
                anyhow::anyhow!("unsupported prepared result type for {}", field.name)
            })?;
            Ok(aligned.column(index, field.name.clone(), kind))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut out = Vec::new();
    // The shared decimal codec emits DECIMALN. NUMERICN has identical value
    // framing but a distinct TYPE_INFO ID, retained by SQL Server for these
    // declarations. Encode each bounded column with the shared codec, then
    // select that declaration's ID at its fixed TYPE_INFO position.
    ensure!(
        columns.len() < usize::from(u16::MAX),
        "too many prepared columns"
    );
    out.push(0x81);
    out.extend(u16::try_from(columns.len())?.to_le_bytes());
    for (column, field) in columns.iter().zip(&fields) {
        let mut encoded = Vec::new();
        tds::metadata(&mut encoded, std::slice::from_ref(column))?;
        if field
            .info
            .as_ref()
            .is_some_and(|info| info.system_type_id == Some(108))
        {
            // COLMETADATA(1) + count(2) + USERTYPE(4) + FLAGS(2).
            ensure!(
                encoded.get(9) == Some(&0x6a),
                "invalid numeric metadata codec"
            );
            encoded[9] = 0x6c;
        }
        out.extend_from_slice(&encoded[3..]);
    }
    match prepared_order(&catalog, query, &scope, &fields, parameters) {
        projection::order::Plan::Token(ordinals) => tds::order::encode(&mut out, &ordinals)?,
        projection::order::Plan::NoToken => {}
        projection::order::Plan::Unknown(_) => bail!("unsupported prepared ORDER declaration"),
    }
    tds::done(&mut out, 0xff, 17, command, 0);
    ensure!(
        out.len() <= tds::MAX_MESSAGE - 1024,
        "prepared metadata exceeds message bound"
    );
    Ok(out)
}

fn typeless_count(expr: &Expr) -> Option<String> {
    fn null(expr: &Expr) -> bool {
        match expr {
            Expr::Nested(expr)
            | Expr::UnaryOp {
                op: sqlparser::ast::UnaryOperator::Plus,
                expr,
            } => null(expr),
            _ => msduck_sql::expression_metadata::conditional::literal_null(expr),
        }
    }
    let Expr::Function(function) = expr else {
        return None;
    };
    let name = function.name.to_string().to_ascii_lowercase();
    if !matches!(name.as_str(), "count" | "count_big")
        || msduck_sql::aggregate::validate(function).is_err()
    {
        return None;
    }
    let FunctionArguments::List(args) = &function.args else {
        return None;
    };
    let [FunctionArg::Unnamed(FunctionArgExpr::Expr(value))] = args.args.as_slice() else {
        return None;
    };
    null(value).then_some(name)
}

// Binding-only probe for the captured SUM(COUNT(NULL)) precedence. Keep the
// outer aggregation and every source/column reference, replacing only its
// direct non-window typeless COUNT input. Never execute or cache this clone.
pub(super) fn nested_count_binding_sql(sql: &str) -> Result<Option<String>> {
    let mut statements = msduck_sql::batch::parse(sql)?;
    let mut changed = false;
    let _ = sqlparser::ast::visit_expressions_mut(&mut statements, |expr| {
        let Expr::Function(outer) = expr else {
            return ControlFlow::<()>::Continue(());
        };
        if !matches!(
            outer.name.to_string().to_ascii_lowercase().as_str(),
            "sum"
                | "avg"
                | "min"
                | "max"
                | "count"
                | "count_big"
                | "stdev"
                | "stdevp"
                | "var"
                | "varp"
        ) || outer.over.is_some()
            || outer.parameters != FunctionArguments::None
            || outer.filter.is_some()
            || outer.null_treatment.is_some()
            || !outer.within_group.is_empty()
            || outer.uses_odbc_syntax
        {
            return ControlFlow::Continue(());
        }
        let FunctionArguments::List(args) = &mut outer.args else {
            return ControlFlow::Continue(());
        };
        if args.duplicate_treatment.is_some() || !args.clauses.is_empty() {
            return ControlFlow::Continue(());
        }
        let [FunctionArg::Unnamed(FunctionArgExpr::Expr(value))] = args.args.as_mut_slice() else {
            return ControlFlow::Continue(());
        };
        let Some(name) = typeless_count(value) else {
            return ControlFlow::Continue(());
        };
        if !matches!(value, Expr::Function(inner) if inner.over.is_none()) {
            return ControlFlow::Continue(());
        }
        *value = Expr::Convert {
            is_try: false,
            expr: Box::new(Expr::Value(sqlparser::ast::Value::Null.into())),
            data_type: Some(if name == "count_big" {
                DataType::BigInt(None)
            } else {
                DataType::Int(None)
            }),
            charset: None,
            target_before_value: true,
            styles: vec![],
        };
        changed = true;
        ControlFlow::Continue(())
    });
    if !changed {
        return Ok(None);
    }
    let probe = statements
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ");
    ensure!(
        probe.len() <= tds::MAX_MESSAGE,
        "prepared binding probe exceeds message bound"
    );
    Ok(Some(probe))
}

pub(super) fn describe(
    session: &Session,
    sql: &str,
    declarations: &[(String, SqlType)],
) -> Result<Description> {
    let statements = msduck_sql::batch::parse(sql)?;
    let parameters = declarations
        .iter()
        .map(|(name, kind)| {
            (
                name.clone(),
                Parameter {
                    data_type: *kind,
                    value: Value::Null,
                },
            )
        })
        .collect();
    // Native source/column binding has already succeeded. Detect typeless
    // COUNT operands only afterwards, preserving captured binding precedence.
    if let ControlFlow::Break(name) = sqlparser::ast::visit_expressions(&statements, |expr| {
        typeless_count(expr).map_or(ControlFlow::Continue(()), ControlFlow::Break)
    }) {
        let mut prefix = Vec::new();
        tds::sql_error(
            &mut prefix,
            &msduck_core::diagnostic::SqlError::new(
                8117,
                1,
                format!("Operand data type NULL is invalid for {name} operator."),
            ),
        );
        tds::sql_error(
            &mut prefix,
            &msduck_core::diagnostic::SqlError::new(8180, 1, "Statement(s) could not be prepared."),
        );
        return Ok(Description {
            prefix,
            status: 8180,
            accepted: false,
        });
    }
    // Declaration collection never evaluates initializers or parameter values.
    let parameters = msduck_sql::preflight::variables(&statements, &parameters)?;
    if statements.len() > 1 && statements.iter().all(|s| matches!(s, Statement::Query(_))) {
        return Ok(Description {
            prefix: vec![],
            status: 8182,
            accepted: true,
        });
    }
    let prefix = match statements.as_slice() {
        [Statement::Query(query)]
            if matches!(query.body.as_ref(), sqlparser::ast::SetExpr::Delete(statement)
                if matches!(statement, Statement::Delete(delete)
                    if delete.output.is_none() && delete.returning.is_none())) =>
        {
            let mut out = Vec::new();
            tds::done(&mut out, 0xff, 17, 0xc4, 0);
            out
        }
        [Statement::Query(query)] => query_description(session, query, &parameters, 0xc1)?,
        [Statement::Insert(insert)]
            if insert.returning.is_none()
                && (insert.output.is_none()
                    || matches!(
                        &insert.output,
                        Some(sqlparser::ast::OutputClause::Output {
                            into_table: Some(_),
                            ..
                        })
                    )) =>
        {
            let mut out = Vec::new();
            tds::done(&mut out, 0xff, 17, 0xc3, 0);
            out
        }
        [Statement::Insert(insert)] if insert.returning.is_none() => {
            // Reuse the deterministic logical OUTPUT projection. The clone's
            // lowered INSERT is never executed; only the explicit target
            // catalog declarations are acquired for the projection.
            let mut statement = Statement::Insert(insert.clone());
            msduck_sql::output::validate(&statement)?;
            let plan = msduck_sql::output::lower_native(&mut statement)?.ok_or_else(|| {
                anyhow::anyhow!("unsupported prepared INSERT result declarations")
            })?;
            ensure!(
                plan.sink.is_none(),
                "unexpected prepared OUTPUT destination"
            );
            query_description(session, &plan.projection, &parameters, 0xc3)?
        }
        [Statement::Insert(_)] => bail!("unsupported prepared INSERT result declarations"),
        // Existing non-result/control-flow preparation stays available. These
        // shapes have no proven description here and are documented gaps.
        _ => vec![],
    };
    Ok(Description {
        prefix,
        status: 0,
        accepted: true,
    })
}

pub(super) fn handle(out: &mut Vec<u8>, name: &str, value: Option<i32>) -> Result<()> {
    use tds::return_value::{Declaration, Parameter as Output, Status, Value as OutputValue};
    let units: Vec<_> = name.trim_start_matches('@').encode_utf16().collect();
    tds::return_value::encode(
        out,
        &Output {
            ordinal: 0,
            name: &units,
            status: Status::Output,
            user_type: 0,
            flags: 0,
            declaration: Declaration::Int(4),
            value: value.map_or(OutputValue::Null, |n| OutputValue::Int(i64::from(n))),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declaration_enrichment_preserves_count_null_barriers_in_outer_expressions() {
        for sql in [
            "SELECT COUNT(NULL)",
            "SELECT COUNT_BIG(NULL)",
            "SELECT COUNT(NULL)+1",
            "SELECT CASE WHEN 1=1 THEN COUNT(NULL) ELSE 0 END",
        ] {
            let Statement::Query(query) = msduck_sql::batch::parse(sql).unwrap().remove(0) else {
                panic!("query")
            };
            let declared = expression_declarations(
                &query,
                &HashMap::new(),
                &CatalogSnapshot::default(),
                &Scope::default(),
            );
            assert!(declared.to_string().contains("COUNT"), "{sql}");
            assert!(declared.to_string().contains("NULL"), "{sql}");
        }
    }

    #[test]
    fn variant_window_declarations_follow_scopes_and_preserve_unknowns() {
        use msduck_sql::binding_scope::Field;
        let mut catalog = CatalogSnapshot::default();
        for (name, id) in [("sql_variant", 98), ("int", 56), ("float", 62)] {
            catalog.types.insert(
                name.into(),
                TypeMetadata {
                    system_type_id: Some(id),
                    user_type_id: Some(i32::from(id)),
                    ..Default::default()
                },
            );
        }
        for (name, id) in [("variants", 98), ("floating", 62)] {
            catalog.tables.insert(
                name.into(),
                vec![Field {
                    name: "v".into(),
                    info: Some(TypeMetadata {
                        system_type_id: Some(id),
                        user_type_id: Some(i32::from(id)),
                        ..Default::default()
                    }),
                    collation: None,
                    json_fragment: false,
                    properties: msduck_core::result::Properties::expression(true),
                }],
            );
        }
        let parameters = HashMap::new();
        let outer = Scope::default();
        let parse = |sql: &str| {
            let Statement::Query(query) = msduck_sql::batch::parse(sql).unwrap().remove(0) else {
                panic!("query")
            };
            query
        };
        let query = parse(
            "SELECT COUNT(*) OVER(PARTITION BY COALESCE(v,CAST(NULL AS SQL_VARIANT))) FROM variants",
        );
        let declared = expression_declarations(&query, &parameters, &catalog, &outer);
        assert!(
            projection::query_fields(&catalog, &query, &outer).unwrap()[0]
                .info
                .is_none()
        );
        assert_eq!(
            projection::query_fields(&catalog, &declared, &outer).unwrap()[0]
                .info
                .as_ref()
                .unwrap()
                .system_type_id,
            Some(56)
        );
        assert!(query.to_string().contains("COALESCE"));
        let unknown = parse(
            "SELECT COUNT(*) OVER(PARTITION BY COALESCE(v,CAST(NULL AS SQL_VARIANT))) FROM floating",
        );
        let declared = expression_declarations(&unknown, &parameters, &catalog, &outer);
        assert!(declared.to_string().contains("COALESCE"));
        let nested = parse(
            "SELECT (SELECT COALESCE(v,CAST(NULL AS SQL_VARIANT)) FROM floating) FROM variants",
        );
        let declared = expression_declarations(&nested, &parameters, &catalog, &outer);
        assert!(
            declared.to_string().contains("COALESCE"),
            "inner FLOAT must shadow outer variant"
        );
        for sql in [
            "SELECT ROW_NUMBER() OVER(ORDER BY COALESCE(v,CAST(NULL AS SQL_VARIANT))) FROM variants",
            "SELECT ROW_NUMBER() OVER(PARTITION BY COALESCE(v,CAST(NULL AS SQL_VARIANT)) ORDER BY v) FROM variants",
        ] {
            let query = parse(sql);
            let source_scope = projection::scopes(&catalog, &query, &outer).body;
            let sqlparser::ast::SetExpr::Select(select) = query.body.as_ref() else {
                panic!("select")
            };
            let sqlparser::ast::SelectItem::UnnamedExpr(expr) = &select.projection[0] else {
                panic!("expression")
            };
            assert!(variant_row_number(expr, &source_scope, &parameters));
        }
        for sql in [
            "SELECT ROW_NUMBER() OVER(ORDER BY COALESCE(CAST(NULL AS SQL_VARIANT),CAST(NULL AS SQL_VARIANT))) FROM variants",
            "SELECT ROW_NUMBER() OVER(ORDER BY COALESCE(v,CAST(NULL AS SQL_VARIANT))) FROM floating",
            "SELECT ROW_NUMBER() OVER(ORDER BY COALESCE(v,CAST(NULL AS SQL_VARIANT)) ROWS UNBOUNDED PRECEDING) FROM variants",
        ] {
            let query = parse(sql);
            let source_scope = projection::scopes(&catalog, &query, &outer).body;
            let sqlparser::ast::SetExpr::Select(select) = query.body.as_ref() else {
                panic!("select")
            };
            let sqlparser::ast::SelectItem::UnnamedExpr(expr) = &select.projection[0] else {
                panic!("expression")
            };
            assert!(
                !variant_row_number(expr, &source_scope, &parameters),
                "{sql}"
            );
        }
    }

    #[test]
    fn datetime2_conditionals_require_all_result_declarations() {
        fn declaration(sql: &str) -> Option<u8> {
            let Statement::Query(query) = msduck_sql::batch::parse(sql).unwrap().remove(0) else {
                panic!("query")
            };
            let sqlparser::ast::SetExpr::Select(select) = query.body.as_ref() else {
                panic!("select")
            };
            let sqlparser::ast::SelectItem::UnnamedExpr(expr) = &select.projection[0] else {
                panic!("expression")
            };
            datetime2_declaration(expr, &HashMap::new())
        }
        assert_eq!(
            declaration("SELECT ISNULL(CAST(NULL AS DATETIME2(2)),CAST(NULL AS DATETIME2(7)))"),
            Some(2)
        );
        assert_eq!(
            declaration("SELECT COALESCE(CAST(NULL AS DATETIME2(2)),CAST(NULL AS DATETIME2(7)))"),
            Some(7)
        );
        assert_eq!(
            declaration(
                "SELECT CASE WHEN 1=1 THEN CAST(NULL AS DATETIME2(2)) ELSE CAST(NULL AS DATETIME2(7)) END"
            ),
            Some(7)
        );
        for sql in [
            "SELECT COALESCE(CAST(NULL AS DATETIME2(2)),@unknown)",
            "SELECT COALESCE(CAST(NULL AS DATETIME2(2)),CAST(NULL AS DATETIMEOFFSET(7)))",
            "SELECT COALESCE(CAST(NULL AS DATETIME2(2)),a)",
        ] {
            assert_eq!(declaration(sql), None, "{sql}");
        }
    }

    #[test]
    fn variant_declarations_require_every_contributing_family() {
        fn declaration(sql: &str) -> Option<bool> {
            let Statement::Query(query) = msduck_sql::batch::parse(sql).unwrap().remove(0) else {
                panic!("query")
            };
            let sqlparser::ast::SetExpr::Select(select) = query.body.as_ref() else {
                panic!("select")
            };
            let sqlparser::ast::SelectItem::UnnamedExpr(expr) = &select.projection[0] else {
                panic!("expression")
            };
            variant_declaration(expr, &Scope::default(), &HashMap::new())
        }
        assert_eq!(
            declaration("SELECT COALESCE(CAST(NULL AS SQL_VARIANT),CAST(4 AS BIGINT))"),
            Some(true)
        );
        assert_eq!(
            declaration("SELECT MIN(CAST(1 AS SQL_VARIANT))"),
            Some(true)
        );
        for sql in [
            "SELECT COALESCE(CAST(NULL AS SQL_VARIANT),@unknown)",
            "SELECT COALESCE(CAST(NULL AS SQL_VARIANT),CAST(NULL AS FLOAT))",
            "SELECT MIN(COUNT(NULL))",
            "SELECT MAX(CAST(COUNT(NULL) AS SQL_VARIANT))",
        ] {
            assert_eq!(declaration(sql), None, "{sql}");
        }
    }

    #[test]
    fn character_metadata_requires_valid_declared_byte_capacity() {
        for id in [167, 175, 231, 239, 165, 173] {
            for width in [None, Some(-2), Some(0), Some(8001)] {
                assert!(
                    wire(&TypeMetadata {
                        system_type_id: Some(id),
                        max_length: width,
                        ..Default::default()
                    })
                    .is_none()
                );
            }
        }
        for id in [231, 239] {
            assert!(
                wire(&TypeMetadata {
                    system_type_id: Some(id),
                    max_length: Some(3),
                    ..Default::default()
                })
                .is_none()
            );
        }
        assert!(matches!(
            wire(&TypeMetadata {
                system_type_id: Some(231),
                max_length: Some(16),
                ..Default::default()
            }),
            Some(tds::Type::Nvarchar(8))
        ));
        assert!(matches!(
            wire(&TypeMetadata {
                system_type_id: Some(231),
                max_length: Some(-1),
                ..Default::default()
            }),
            Some(tds::Type::Text)
        ));
        assert!(wire(&TypeMetadata::default()).is_none());
    }

    #[test]
    fn handle_output_is_nullable_and_never_truncates_names() {
        let mut out = Vec::new();
        handle(&mut out, "@handle", None).unwrap();
        assert_eq!(out[0], 0xac);
        assert_eq!(out[3], 6);
        assert_eq!(&out[out.len() - 4..], &[0, 0x26, 4, 0]);
        assert!(handle(&mut Vec::new(), &"h".repeat(256), Some(1)).is_err());
    }
}
