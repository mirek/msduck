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

// Captured bitwise declarations depend on operand types, not NULL values
// or payloads. Alias, approximate and unresolved sources remain barriers.
fn bitwise_declaration(
    expression: &Expr,
    parameters: &HashMap<String, Parameter>,
    scope: &Scope,
) -> Option<DataType> {
    let column = |expr: &Expr| {
        let ids = match expr {
            Expr::Identifier(id) if !id.value.starts_with('@') => vec![id],
            Expr::CompoundIdentifier(ids) => ids.iter().collect(),
            _ => return None,
        };
        let info = msduck_sql::binding_scope::resolve(&ids, &[], &scope.rows)?
            .info
            .as_ref()?;
        let id = info.system_type_id?;
        if info.user_type_id.is_some_and(|user| user != i32::from(id)) {
            return None;
        }
        Some(match id {
            104 => DataType::Bit(None),
            48 => DataType::TinyInt(None),
            52 => DataType::SmallInt(None),
            56 => DataType::Int(None),
            127 => DataType::BigInt(None),
            _ => return None,
        })
    };
    let operand = |expr: &Expr| {
        if msduck_sql::expression_metadata::conditional::literal_null(expr) {
            return None;
        }
        msduck_sql::expression_metadata::storage::kind(expr, parameters, &column)
            .or_else(|| bitwise_declaration(expr, parameters, scope))
            .filter(|kind| {
                matches!(kind, DataType::Bit(_)) || msduck_sql::sql_type::integral_type(kind)
            })
    };
    match expression {
        Expr::Nested(expr) => bitwise_declaration(expr, parameters, scope),
        Expr::UnaryOp {
            op: sqlparser::ast::UnaryOperator::BitwiseNot,
            expr,
        } => operand(expr),
        Expr::BinaryOp {
            left,
            op:
                sqlparser::ast::BinaryOperator::BitwiseAnd
                | sqlparser::ast::BinaryOperator::BitwiseOr
                | sqlparser::ast::BinaryOperator::BitwiseXor,
            right,
        } => {
            let (left, right) = (operand(left)?, operand(right)?);
            if matches!(left, DataType::Bit(_)) {
                Some(right)
            } else if matches!(right, DataType::Bit(_)) {
                Some(left)
            } else {
                None
            }
        }
        _ => None,
    }
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

// Describe only validated scalar JSON functions over explicit character inputs.
// The source is retained in a metadata-only CAST so input collation survives;
// this query is never submitted to DuckDB or used as cached execution SQL.
fn json_declaration(expression: &Expr, catalog: &CatalogSnapshot, scope: &Scope) -> Option<Expr> {
    let Expr::Function(function) = expression else {
        return None;
    };
    let name = function.name.to_string().to_ascii_lowercase();
    if !matches!(name.as_str(), "json_query" | "json_value")
        || function.over.is_some()
        || function.filter.is_some()
        || function.null_treatment.is_some()
        || !function.within_group.is_empty()
        || function.parameters != FunctionArguments::None
        || function.uses_odbc_syntax
    {
        return None;
    }
    let FunctionArguments::List(args) = &function.args else {
        return None;
    };
    if args.duplicate_treatment.is_some() || !args.clauses.is_empty() {
        return None;
    }
    let source = match (name.as_str(), args.args.as_slice()) {
        ("json_query", [FunctionArg::Unnamed(FunctionArgExpr::Expr(source))])
        | (
            "json_query" | "json_value",
            [
                FunctionArg::Unnamed(FunctionArgExpr::Expr(source)),
                FunctionArg::Unnamed(FunctionArgExpr::Expr(_)),
            ],
        ) => source,
        _ => return None,
    };
    let Statement::Query(mut probe) = msduck_sql::batch::parse("SELECT 0").ok()?.remove(0) else {
        return None;
    };
    let sqlparser::ast::SetExpr::Select(select) = probe.body.as_mut() else {
        return None;
    };
    select.projection = vec![sqlparser::ast::SelectItem::UnnamedExpr(source.clone())];
    let fields = projection::query_fields(catalog, &probe, scope)?;
    let [field] = fields.as_slice() else {
        return None;
    };
    let info = field.info.as_ref()?;
    if !matches!(info.system_type_id, Some(167 | 175 | 231 | 239))
        || info.user_type_id != info.system_type_id.map(i32::from)
        || field.collation.as_ref()?.as_ref().ok()?.name().is_none()
    {
        return None;
    }
    let length = match info.max_length? {
        -1 if name == "json_query" => msduck_core::character::Length::Max,
        -1 | 1.. => msduck_core::character::Length::Bounded(4000),
        _ => return None,
    };
    Some(Expr::Convert {
        is_try: false,
        expr: Box::new(source.clone()),
        data_type: Some(msduck_sql::sql_type::ast(SqlType::Character(
            msduck_core::character::CharacterType::new(
                msduck_core::character::Family::Nvarchar,
                length,
            )
            .ok()?,
        ))),
        charset: None,
        target_before_value: true,
        styles: vec![],
    })
}

fn json_declarations(query: &Query, catalog: &CatalogSnapshot, outer: &Scope) -> Query {
    struct Declare<'a> {
        catalog: &'a CatalogSnapshot,
        outer: &'a Scope,
        scopes: Vec<Scope>,
    }
    impl VisitorMut for Declare<'_> {
        type Break = ();
        fn pre_visit_query(&mut self, query: &mut Query) -> ControlFlow<()> {
            self.scopes.push(
                projection::scopes(
                    self.catalog,
                    query,
                    self.scopes.last().unwrap_or(self.outer),
                )
                .body,
            );
            ControlFlow::Continue(())
        }
        fn post_visit_query(&mut self, _: &mut Query) -> ControlFlow<()> {
            self.scopes.pop();
            ControlFlow::Continue(())
        }
        fn post_visit_expr(&mut self, expression: &mut Expr) -> ControlFlow<()> {
            if let Some(declaration) = json_declaration(
                expression,
                self.catalog,
                self.scopes.last().unwrap_or(self.outer),
            ) {
                *expression = declaration;
            }
            ControlFlow::Continue(())
        }
    }
    let mut declared = query.clone();
    let _ = declared.visit(&mut Declare {
        catalog,
        outer,
        scopes: Vec::new(),
    });
    declared
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
                    bitwise_declaration(
                        expression,
                        self.parameters,
                        self.scopes.last().unwrap_or(self.outer),
                    )
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
                } else if matches!(name.as_str(), "percent_rank" | "cume_dist")
                    && msduck_sql::ranking::validate(f).is_ok()
                    && !f.uses_odbc_syntax
                {
                    // Original native binding checked the window and names.
                    // The result declaration is fixed even for empty input.
                    Some(DataType::Double(ExactNumberInfo::None))
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

// SQL Server compiles literal JSON extraction even inside a dead CASE arm or
// empty input. Only literal document/path operands are inspected here; bound
// parameters, casts, columns and volatile expressions remain unevaluated.
fn literal_json_error(statements: &[Statement]) -> Option<msduck_core::diagnostic::SqlError> {
    fn text(expr: &Expr) -> Option<&str> {
        match expr {
            Expr::Nested(inner) => text(inner),
            Expr::Value(value) => match &value.value {
                sqlparser::ast::Value::NationalStringLiteral(text) => Some(text),
                // Non-ASCII ANSI literal conversion depends on the database
                // code page; preserve that barrier rather than inspecting the
                // unconverted parser spelling.
                sqlparser::ast::Value::SingleQuotedString(text) if text.is_ascii() => Some(text),
                _ => None,
            },
            _ => None,
        }
    }
    for statement in statements {
        let result = sqlparser::ast::visit_expressions(statement, |expr| {
            let Expr::Function(function) = expr else {
                return ControlFlow::Continue(());
            };
            let name = function.name.to_string().to_ascii_lowercase();
            if !matches!(name.as_str(), "json_query" | "json_value")
                || function.parameters != FunctionArguments::None
                || function.over.is_some()
                || function.filter.is_some()
                || function.null_treatment.is_some()
                || !function.within_group.is_empty()
                || function.uses_odbc_syntax
            {
                return ControlFlow::Continue(());
            }
            let FunctionArguments::List(args) = &function.args else {
                return ControlFlow::Continue(());
            };
            if args.duplicate_treatment.is_some() || !args.clauses.is_empty() {
                return ControlFlow::Continue(());
            }
            let (document, path) = match args.args.as_slice() {
                [
                    FunctionArg::Unnamed(FunctionArgExpr::Expr(document)),
                    FunctionArg::Unnamed(FunctionArgExpr::Expr(path)),
                ] => (text(document), text(path)),
                _ => return ControlFlow::Continue(()),
            };
            if let (Some(document), Some(path)) = (document, path)
                && let Err(error) =
                    msduck_core::json_path::extract_detailed(document, path, name == "json_query")
                && let Some(error) = msduck_core::json_path::diagnostic(&error.backend_message())
            {
                return ControlFlow::Break(error);
            }
            ControlFlow::Continue(())
        });
        if let ControlFlow::Break(error) = result {
            return Some(error);
        }
    }
    None
}

pub(super) struct Description {
    pub prefix: Vec<u8>,
    pub status: i32,
    pub accepted: bool,
    pub complete_error: Option<msduck_core::diagnostic::SqlError>,
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
            // Captured one-column variant UNION/UNION ALL resolves its output
            // alias or position, without borrowing an operand's row scope.
            if matches!(
                expanded.body.as_ref(),
                SetExpr::SetOperation {
                    op: SetOperator::Union,
                    ..
                }
            ) && fields.len() == 1
                && fields[0].info.as_ref().is_some_and(|info| {
                    info.system_type_id == Some(98) && info.user_type_id == Some(98)
                })
                && keys.len() == 1
                && match &keys[0].expr {
                    Expr::Identifier(id) => id.value.eq_ignore_ascii_case(&fields[0].name),
                    Expr::Value(value) => {
                        matches!(&value.value, Value::Number(n, false) if n == "1")
                    }
                    _ => false,
                }
            {
                return Some(vec![1]);
            }
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
            let expr = msduck_sql::variant_cast::source(expr).unwrap_or(expr);
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
        fn integer_key<'a>(
            expr: &Expr,
            scope: &'a Scope,
        ) -> Option<(&'a msduck_sql::binding_scope::Field, i32)> {
            fn literal(expr: &Expr) -> Option<i32> {
                match expr {
                    Expr::Nested(expr) => literal(expr),
                    Expr::UnaryOp {
                        op: UnaryOperator::Plus,
                        expr,
                    } => literal(expr),
                    Expr::UnaryOp {
                        op: UnaryOperator::Minus,
                        expr,
                    } => literal(expr)?.checked_neg(),
                    Expr::Value(value) => match &value.value {
                        Value::Number(n, false) => n.parse().ok(),
                        _ => None,
                    },
                    _ => None,
                }
            }
            if let Expr::Nested(expr) = expr {
                return integer_key(expr, scope);
            }
            let Expr::BinaryOp {
                left,
                op: BinaryOperator::Plus,
                right,
            } = expr
            else {
                return None;
            };
            let source = column(left, scope)?;
            (source.info.as_ref()?.system_type_id == Some(56)).then_some((source, literal(right)?))
        }
        let projected_key = |expr: &Expr| {
            if column(expr, &scope).is_some() || integer_key(expr, &scope).is_some() {
                return true;
            }
            if msduck_sql::expression_metadata::conditional::candidate(expr)
                && variant_declaration(expr, &scope, parameters).is_some()
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
                    if f.name.to_string().eq_ignore_ascii_case("GROUPING")
                        && f.parameters == FunctionArguments::None
                        && f.over.is_none()
                        && f.filter.is_none()
                        && f.null_treatment.is_none()
                        && f.within_group.is_empty()
                        && !f.uses_odbc_syntax =>
                {
                    let FunctionArguments::List(args) = &f.args else {
                        return false;
                    };
                    args.duplicate_treatment.is_none()
                        && args.clauses.is_empty()
                        && matches!(args.args.as_slice(), [FunctionArg::Unnamed(FunctionArgExpr::Expr(expr))]
                            if column(expr, &scope).is_some_and(|field|
                                field.info.as_ref().and_then(|info| info.system_type_id) == Some(56))
                                || (msduck_sql::expression_metadata::conditional::candidate(expr)
                                    && variant_declaration(expr, &scope, parameters) == Some(false)
                                    && projection::order::expression_identity(expr, expr, &[], &scope) == Some(true)))
                }
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
                // Captured integral/variant conditional keys retain projected
                // identity across qualification, parentheses and parameter case.
                // A pure shape comparison does not authorize an unknown family.
                if projected_key(&key.expr)
                    && let Some(index) = expressions.iter().position(|expr| {
                        projection::order::expression_identity(expr, &key.expr, &[], &scope)
                            == Some(true)
                    })
                {
                    ordinals.push(u16::try_from(index + 1).ok()?);
                    continue;
                }
                // Captured hidden INT-column-plus-integer keys use ordinal
                // zero. The original native binding validates arithmetic;
                // this declaration check neither evaluates nor rewrites it.
                if let Some((source, value)) = integer_key(&key.expr, &scope) {
                    let projected = expressions.iter().position(|expr| {
                        integer_key(expr, &scope).is_some_and(|(other, literal)| {
                            std::ptr::eq(source, other) && value == literal
                        })
                    });
                    if projected.is_none() && select.distinct.is_some() {
                        return None;
                    }
                    ordinals
                        .push(projected.map_or(Some(0), |index| u16::try_from(index + 1).ok())?);
                    continue;
                }
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

// The shared numeric/character set merger leaves variants unresolved.
// Preserve only an identical catalog variant declaration on both branches;
// mixed families and aliases remain unknown. Properties still come from the
// shared operator-specific inference over the original query.
fn prepared_fields(
    catalog: &CatalogSnapshot,
    query: &Query,
    scope: &Scope,
) -> Option<Vec<msduck_sql::binding_scope::Field>> {
    let mut fields = projection::query_fields(catalog, query, scope)?;
    let sqlparser::ast::SetExpr::SetOperation {
        left,
        right,
        op: sqlparser::ast::SetOperator::Union,
        ..
    } = query.body.as_ref()
    else {
        return Some(fields);
    };
    let mut branch = query.clone();
    branch.body = left.clone();
    let left = prepared_fields(catalog, &branch, scope)?;
    branch.body = right.clone();
    let right = prepared_fields(catalog, &branch, scope)?;
    if fields.len() != left.len() || left.len() != right.len() {
        return None;
    }
    for ((field, left), right) in fields.iter_mut().zip(left).zip(right) {
        if field.info.is_none()
            && let (Some(a), Some(b)) = (left.info, right.info)
            && a.system_type_id == Some(98)
            && b.system_type_id == Some(98)
            && a.user_type_id == Some(98)
            && b.user_type_id == Some(98)
            && a == b
        {
            field.info = Some(a);
        }
    }
    Some(fields)
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
    let mut fields = prepared_fields(&catalog, query, &scope)
        .ok_or_else(|| anyhow::anyhow!("unsupported prepared result declarations"))?;
    // Preserve original explicit VALUES declarations before operand annotation
    // lowers temporal sources into native helper calls. The annotated candidate
    // additionally supplies catalog-bound aggregate/operand declarations.
    let original_declarations = expression_declarations(query, parameters, &catalog, &scope);
    let mut declaration_query = query.clone();
    crate::aggregate_columns::annotate(&session.db, &mut declaration_query, parameters)
        .map_err(anyhow::Error::msg)?;
    let json_query = json_declarations(query, &catalog, &scope);
    for candidate in [
        original_declarations,
        json_query.clone(),
        expression_declarations(&json_query, parameters, &catalog, &scope),
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
    let hidden_order = matches!(prepared_order(&catalog, query, &scope, &fields, parameters), projection::order::Plan::Token(keys) if keys.contains(&0));
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
        let json_fields = projection::query_fields(
            &catalog,
            &json_declarations(&expanded, &catalog, &scope),
            &scope,
        );
        let field_count = fields.len();
        for (index, (field, item)) in fields.iter_mut().zip(&select.projection).enumerate() {
            let expression = match item {
                sqlparser::ast::SelectItem::UnnamedExpr(expr)
                | sqlparser::ast::SelectItem::ExprWithAlias { expr, .. } => expr,
                _ => continue,
            };
            // JSON functions have computed, nullable results independently of
            // the source's nullability. Conditional parents retain their own
            // proven nullability; the metadata clone supplies source collation.
            let mut has_json = false;
            let _ = sqlparser::ast::visit_expressions(expression, |node| {
                if json_declaration(node, &catalog, &source_scope).is_some() {
                    has_json = true;
                }
                ControlFlow::<()>::Continue(())
            });
            if has_json
                && let Some(declared) = &json_fields
                && declared.len() == field_count
            {
                let declaration = &declared[index];
                if field.info.is_some() {
                    field.collation = declaration.collation.clone();
                    field.properties = msduck_sql::result_properties::expression_with(
                        expression,
                        &[],
                        &source_scope.rows,
                        &|node| {
                            json_declaration(node, &catalog, &source_scope)
                                .map(|_| msduck_core::result::Properties::expression(true))
                        },
                    );
                }
            }
            // Captured hidden sorting exposes a direct identity variant as a
            // stored catalog projection, unlike the unsorted computed profile.
            if hidden_order
                && field.info.as_ref().is_some_and(|info| {
                    info.system_type_id == Some(98) && info.user_type_id == Some(98)
                })
                && select.from.len() == 1
                && select.from[0].joins.is_empty()
                && matches!(&select.from[0].relation, sqlparser::ast::TableFactor::Table {name,args:None,..}
                    if name.to_string().eq_ignore_ascii_case("sys.identity_columns"))
                && (matches!(expression, Expr::Identifier(id) if !id.value.starts_with('@'))
                    || matches!(expression, Expr::CompoundIdentifier(_)))
            {
                field.properties = msduck_core::result::Properties {
                    nullable: Some(true),
                    origin: msduck_core::result::Origin::Stored,
                };
            }
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
            }
            | Expr::Convert {
                expr,
                data_type: Some(data_type),
                ..
            } = expression
                && catalog
                    .cast_info(data_type)
                    .is_some_and(|info| matches!(info.system_type_id, Some(48 | 52 | 56 | 127)))
            {
                let source = msduck_sql::variant_cast::source(expr).unwrap_or(expr);
                if variant_declaration(source, &source_scope, parameters) == Some(true) {
                    // Captured preparation keeps computed provenance for an
                    // explicit integer conversion of a declared variant.
                    field.properties.origin = msduck_core::result::Origin::Expression;
                }
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
                    field.properties = msduck_core::result::Properties {
                        nullable: Some(true),
                        origin: if hidden_order {
                            msduck_core::result::Origin::Derived
                        } else {
                            msduck_core::result::Origin::Expression
                        },
                    };
                }
            }
        }
    }
    let order = prepared_order(&catalog, query, &scope, &fields, parameters);
    fields_description(&fields, order, command)
}

// Root wire adapter shared by SELECT and independently bound DML descriptions.
// Every field remains an explicit logical declaration; physical row values do
// not supply missing metadata, and an unknown ORDER plan remains an error.
fn fields_description(
    fields: &[msduck_sql::binding_scope::Field],
    order: projection::order::Plan,
    command: u16,
) -> Result<Vec<u8>> {
    ensure!(
        !fields.is_empty(),
        "unsupported empty prepared result declarations"
    );
    let aligned = crate::result_metadata::Aligned::new(fields.len(), fields, &[]);
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
    for (column, field) in columns.iter().zip(fields) {
        let mut encoded = Vec::new();
        tds::metadata(&mut encoded, std::slice::from_ref(column))?;
        if field
            .info
            .as_ref()
            .is_some_and(|info| info.system_type_id == Some(231) && info.user_type_id == Some(256))
        {
            // Built-in sysname retains its alias USERTYPE in captured catalog
            // metadata. Scalar functions and conversions return base NVARCHAR.
            ensure!(encoded.len() >= 9, "truncated sysname metadata codec");
            encoded[3..7].copy_from_slice(&256u32.to_le_bytes());
        }
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
    match order {
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
            complete_error: None,
        });
    }
    if let Some(error) = literal_json_error(&statements) {
        return Ok(Description {
            prefix: vec![],
            status: 0,
            accepted: false,
            complete_error: Some(error),
        });
    }
    // Declaration collection never evaluates initializers or parameter values.
    let parameters = msduck_sql::preflight::variables(&statements, &parameters)?;
    if statements.len() > 1 && statements.iter().all(|s| matches!(s, Statement::Query(_))) {
        return Ok(Description {
            prefix: vec![],
            status: 8182,
            accepted: true,
            complete_error: None,
        });
    }
    if let [statement] = statements.as_slice() {
        if let Some((update, with)) = msduck_sql::output::joined_update(statement)
            && let Some(sqlparser::ast::OutputClause::Output {
                select_items,
                into_table: None,
                ..
            }) = &update.output
        {
            // Native binding already validated the original statement. This
            // existing catalog adapter acquires explicit target/source fields;
            // it neither captures images nor executes assignments or OUTPUT.
            let binding =
                crate::output_join::bind(&session.db, update, with.cloned(), &parameters)?;
            let fields = binding.output_fields(select_items)?;
            return Ok(Description {
                prefix: fields_description(&fields, projection::order::Plan::NoToken, 0xc5)?,
                status: 0,
                accepted: true,
                complete_error: None,
            });
        }
        let mut logical = statement.clone();
        if msduck_sql::output::joined_update(statement).is_none()
            && let Some(plan) = msduck_sql::output::lower_native(&mut logical)?
            && plan.sink.is_none()
        {
            let command = match plan.operation {
                msduck_sql::output::Operation::Insert => 0xc3,
                msduck_sql::output::Operation::Update => 0xc5,
                msduck_sql::output::Operation::Delete => 0xc4,
            };
            return Ok(Description {
                prefix: query_description(session, &plan.projection, &parameters, command)?,
                status: 0,
                accepted: true,
                complete_error: None,
            });
        }
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
        complete_error: None,
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
    fn literal_json_compilation_preserves_errors_and_dynamic_barriers() {
        for (sql, number, state) in [
            ("SELECT JSON_QUERY(N'bad','$.a')", 13609, 1),
            (
                "SELECT CASE WHEN 1=0 THEN JSON_QUERY(N'bad','$.a') ELSE N'{}' END",
                13609,
                1,
            ),
            ("SELECT JSON_QUERY(N'bad','$.a') WHERE 1=0", 13609, 1),
            ("SELECT JSON_QUERY(N'{}','strict $.a')", 13608, 1),
            ("SELECT JSON_QUERY(N'bad','$.[')", 13607, 14),
            ("SELECT JSON_VALUE(N'bad','$.a')", 13609, 1),
        ] {
            let statements = msduck_sql::batch::parse(sql).unwrap();
            let before = statements.clone();
            let error = literal_json_error(&statements).unwrap();
            assert_eq!((error.number, error.state), (number, state), "{sql}");
            assert_eq!(statements, before);
        }
        for sql in [
            "SELECT JSON_QUERY(@j,'$.a')",
            "SELECT JSON_QUERY(CAST(@p AS NVARCHAR(MAX)),'$.a')",
            "SELECT JSON_QUERY(CAST(RAND() AS NVARCHAR(MAX)),'$.a')",
            "SELECT JSON_QUERY(N'bad',@path)",
            "SELECT JSON_QUERY(N'{}','$')",
            "SELECT JSON_QUERY('ébad','$.a')",
            "SELECT JSON_QUERY(DISTINCT N'bad','$.a')",
        ] {
            assert!(
                literal_json_error(&msduck_sql::batch::parse(sql).unwrap()).is_none(),
                "{sql}"
            );
        }
    }

    #[test]
    fn json_declarations_retain_input_collation_without_evaluation() {
        let mut catalog = CatalogSnapshot {
            default_collation: Some("SQL_Latin1_General_CP1_CI_AS".into()),
            ..Default::default()
        };
        for (name, id, length) in [("int", 56, 4), ("nvarchar", 231, 8000)] {
            catalog.types.insert(
                name.into(),
                TypeMetadata {
                    system_type_id: Some(id),
                    user_type_id: Some(i32::from(id)),
                    max_length: Some(length),
                    ..Default::default()
                },
            );
        }
        for (sql, length, collation) in [
            (
                "SELECT JSON_QUERY(CAST(@p AS NVARCHAR(MAX)), 'strict $.a')",
                -1,
                "SQL_Latin1_General_CP1_CI_AS",
            ),
            (
                "SELECT JSON_QUERY(CAST(@p AS NVARCHAR(8)), '$.a')",
                8000,
                "SQL_Latin1_General_CP1_CI_AS",
            ),
            (
                "SELECT JSON_VALUE(CAST(@p AS NVARCHAR(MAX)) COLLATE Latin1_General_100_BIN2, '$.a')",
                8000,
                "Latin1_General_100_BIN2",
            ),
            (
                "SELECT ISNULL(JSON_QUERY(CAST(@p AS NVARCHAR(MAX)), '$.a'), N'{}')",
                -1,
                "SQL_Latin1_General_CP1_CI_AS",
            ),
        ] {
            let Statement::Query(query) = msduck_sql::batch::parse(sql).unwrap().remove(0) else {
                panic!("query")
            };
            let original = query.clone();
            let mut scope = Scope::default();
            scope.parameters.insert(
                "@p".into(),
                catalog.cast_info(&DataType::Int(None)).unwrap(),
            );
            let described = json_declarations(&query, &catalog, &scope);
            let fields = projection::query_fields(&catalog, &described, &scope).unwrap();
            assert_eq!(
                fields[0].info.as_ref().unwrap().max_length,
                Some(length),
                "{sql}"
            );
            assert_eq!(
                fields[0]
                    .collation
                    .as_ref()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .name(),
                Some(collation),
                "{sql}"
            );
            assert_eq!(
                *query, *original,
                "metadata must not change cached execution AST"
            );
        }
        let Statement::Query(query) = msduck_sql::batch::parse("SELECT CASE WHEN @p>0 THEN JSON_QUERY(CAST(@p AS NVARCHAR(MAX)), '$.a') ELSE N'{}' END").unwrap().remove(0) else { panic!("query") };
        let mut scope = Scope::default();
        scope.parameters.insert(
            "@p".into(),
            catalog.cast_info(&DataType::Int(None)).unwrap(),
        );
        let declared_json = json_declarations(&query, &catalog, &scope);
        let described = expression_declarations(&declared_json, &HashMap::new(), &catalog, &scope);
        let fields = projection::query_fields(&catalog, &described, &scope).unwrap();
        assert_eq!(fields[0].info.as_ref().unwrap().max_length, Some(-1));
        assert_eq!(
            projection::query_fields(&catalog, &declared_json, &scope).unwrap()[0]
                .collation
                .as_ref()
                .unwrap()
                .as_ref()
                .unwrap()
                .name(),
            Some("SQL_Latin1_General_CP1_CI_AS")
        );
        for sql in [
            "SELECT JSON_QUERY(missing, '$.a')",
            "SELECT JSON_QUERY(CAST(NULL AS INT), '$.a')",
            "SELECT JSON_VALUE(CAST(NULL AS NVARCHAR(MAX)))",
            "SELECT JSON_QUERY(DISTINCT CAST(NULL AS NVARCHAR(MAX)), '$.a')",
        ] {
            let Statement::Query(query) = msduck_sql::batch::parse(sql).unwrap().remove(0) else {
                panic!("query")
            };
            assert_eq!(
                json_declarations(&query, &catalog, &Scope::default()),
                *query,
                "{sql}"
            );
        }
    }

    #[test]
    fn bitwise_declarations_use_catalog_types_and_keep_unknown_barriers() {
        use msduck_sql::binding_scope::Field;
        let mut catalog = CatalogSnapshot::default();
        catalog.tables.insert(
            "bits".into(),
            [
                ("b", 104, 104),
                ("s", 52, 52),
                ("f", 62, 62),
                ("alias", 104, 900),
            ]
            .into_iter()
            .map(|(name, id, user)| Field {
                name: name.into(),
                info: Some(TypeMetadata {
                    system_type_id: Some(id),
                    user_type_id: Some(user),
                    ..Default::default()
                }),
                properties: msduck_core::result::Properties::expression(true),
                collation: None,
                json_fragment: false,
            })
            .collect(),
        );
        for (expression, expected) in [
            ("b&b", Some(DataType::Bit(None))),
            ("b|b", Some(DataType::Bit(None))),
            ("b^b", Some(DataType::Bit(None))),
            ("b&s", Some(DataType::SmallInt(None))),
            ("s|b", Some(DataType::SmallInt(None))),
            ("~b", Some(DataType::Bit(None))),
            ("(b&b)^b", Some(DataType::Bit(None))),
            ("b&CAST(NULL AS BIT)", Some(DataType::Bit(None))),
            ("b&f", None),
            ("b&alias", None),
            ("b&missing", None),
            ("b&NULL", None),
        ] {
            let Statement::Query(query) =
                msduck_sql::batch::parse(&format!("SELECT {expression} FROM bits"))
                    .unwrap()
                    .remove(0)
            else {
                panic!("query")
            };
            let sqlparser::ast::SetExpr::Select(select) = query.body.as_ref() else {
                panic!("select")
            };
            let sqlparser::ast::SelectItem::UnnamedExpr(expr) = &select.projection[0] else {
                panic!("expression")
            };
            let scope = projection::scopes(&catalog, &query, &Scope::default()).body;
            assert_eq!(
                bitwise_declaration(expr, &HashMap::new(), &scope),
                expected,
                "{expression}"
            );
        }
    }

    #[test]
    fn prepared_order_uses_original_cast_sources_and_proven_variant_sets() {
        use msduck_sql::binding_scope::Field;
        let mut catalog = CatalogSnapshot::default();
        for (name, id) in [
            ("int", 56),
            ("bigint", 127),
            ("sql_variant", 98),
            ("float", 62),
        ] {
            catalog.types.insert(
                name.into(),
                TypeMetadata {
                    system_type_id: Some(id),
                    user_type_id: Some(i32::from(id)),
                    ..Default::default()
                },
            );
        }
        catalog.tables.insert(
            "dbo.prepare_heap".into(),
            ["a", "b"]
                .into_iter()
                .map(|name| Field {
                    name: name.into(),
                    info: catalog.cast_info(&DataType::Int(None)),
                    properties: msduck_core::result::Properties {
                        nullable: Some(true),
                        origin: msduck_core::result::Origin::Stored,
                    },
                    collation: None,
                    json_fragment: false,
                })
                .collect(),
        );
        let scope = Scope::default();
        for (sql, expected) in [
            (
                "SELECT CAST(v AS INT) AS n FROM (SELECT CAST(a AS SQL_VARIANT) AS v FROM dbo.prepare_heap) q ORDER BY n",
                vec![1],
            ),
            (
                "SELECT CAST(v AS BIGINT) AS n FROM (SELECT CAST(a AS SQL_VARIANT) AS v FROM dbo.prepare_heap) q ORDER BY 1 DESC",
                vec![1],
            ),
            (
                "SELECT CAST(b AS SQL_VARIANT) AS v FROM dbo.prepare_heap ORDER BY v,a+1",
                vec![1, 0],
            ),
            (
                "SELECT CAST(a AS SQL_VARIANT) AS v FROM dbo.prepare_heap UNION ALL SELECT CAST(b AS SQL_VARIANT) FROM dbo.prepare_heap ORDER BY 1",
                vec![1],
            ),
            (
                "SELECT CAST(a AS SQL_VARIANT) AS v FROM dbo.prepare_heap UNION SELECT CAST(b AS SQL_VARIANT) FROM dbo.prepare_heap ORDER BY v",
                vec![1],
            ),
        ] {
            let Statement::Query(query) = msduck_sql::batch::parse(sql).unwrap().remove(0) else {
                panic!("query")
            };
            let fields = prepared_fields(&catalog, &query, &scope).unwrap();
            assert_eq!(
                prepared_order(&catalog, &query, &scope, &fields, &HashMap::new()),
                projection::order::Plan::Token(expected),
                "{sql}"
            );
        }
        let sql =
            "SELECT NTILE(@p) OVER(ORDER BY a) AS r,a+1 AS n FROM dbo.prepare_heap ORDER BY a+1";
        let Statement::Query(query) = msduck_sql::batch::parse(sql).unwrap().remove(0) else {
            panic!("query")
        };
        let mut fields = prepared_fields(&catalog, &query, &scope).unwrap();
        fields[0].info = catalog.cast_info(&DataType::BigInt(None));
        assert_eq!(
            prepared_order(&catalog, &query, &scope, &fields, &HashMap::new()),
            projection::order::Plan::Token(vec![2])
        );
        for sql in [
            "SELECT NTILE(@p) OVER(ORDER BY t.a) AS r,t.a+1 AS n FROM dbo.prepare_heap t ORDER BY a+1",
            "SELECT NTILE(@p) OVER(ORDER BY t.a) AS r,(t.a+01) AS n FROM dbo.prepare_heap t ORDER BY (a+(+1))",
        ] {
            let Statement::Query(query) = msduck_sql::batch::parse(sql).unwrap().remove(0) else {
                panic!("query")
            };
            let mut fields = prepared_fields(&catalog, &query, &scope).unwrap();
            fields[0].info = catalog.cast_info(&DataType::BigInt(None));
            assert_eq!(
                prepared_order(&catalog, &query, &scope, &fields, &HashMap::new()),
                projection::order::Plan::Token(vec![2]),
                "{sql}"
            );
        }
        for sql in [
            "SELECT CAST(a AS SQL_VARIANT) AS v FROM dbo.prepare_heap UNION ALL SELECT b FROM dbo.prepare_heap",
            "SELECT CAST(a AS SQL_VARIANT) AS v FROM dbo.prepare_heap UNION ALL SELECT CAST(b AS FLOAT) FROM dbo.prepare_heap",
        ] {
            let Statement::Query(query) = msduck_sql::batch::parse(sql).unwrap().remove(0) else {
                panic!("query")
            };
            assert!(
                prepared_fields(&catalog, &query, &scope).unwrap()[0]
                    .info
                    .is_none(),
                "{sql}"
            );
        }
    }

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
