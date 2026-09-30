//! Logical ORDER metadata over explicit declarations. This does not validate a
//! complete SQL statement or inspect bound values, rows or backend plans.
use crate::{
    binding_scope::{Field, Scope, Source},
    catalog_snapshot::CatalogSnapshot,
};
use sqlparser::ast::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Plan {
    NoToken,
    Token(Vec<u16>),
    Unknown(Barrier),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Barrier {
    Shape,
    Projection,
    Source,
    AmbiguousName,
    Expression,
    Constant,
    Length,
}

fn inner(mut expr: &Expr) -> &Expr {
    while let Expr::Nested(value) = expr {
        expr = value;
    }
    expr
}
fn column<'a>(expr: &Expr, sources: &'a [Source], scope: &'a Scope) -> Option<&'a Field> {
    let ids = match inner(expr) {
        Expr::Identifier(id) if !id.value.starts_with('@') => vec![id],
        Expr::CompoundIdentifier(ids) => ids.iter().collect(),
        _ => return None,
    };
    let field = crate::binding_scope::resolve(&ids, sources, &scope.rows)?;
    field.info.as_ref()?;
    Some(field)
}
fn same(left: &Expr, right: &Expr, sources: &[Source], scope: &Scope) -> bool {
    // Canonicalize borrowed field identity through the whole expression tree,
    // including CAST, COLLATE and aggregate arguments. Never rewrite the input.
    struct Canonical<'a> {
        sources: &'a [Source],
        scope: &'a Scope,
        fields: Vec<&'a Field>,
    }
    impl VisitorMut for Canonical<'_> {
        type Break = ();
        fn pre_visit_expr(&mut self, expr: &mut Expr) -> std::ops::ControlFlow<()> {
            if matches!(expr, Expr::Nested(_)) {
                *expr = inner(expr).clone();
            }
            match expr {
                Expr::Function(function)
                    if ["COUNT", "SUM", "ROW_NUMBER"]
                        .iter()
                        .any(|name| function.name.to_string().eq_ignore_ascii_case(name)) =>
                {
                    for part in &mut function.name.0 {
                        if let ObjectNamePart::Identifier(name) = part {
                            name.value = name.value.to_uppercase();
                        }
                    }
                }
                Expr::Collate { collation, .. }
                    if collation
                        .to_string()
                        .eq_ignore_ascii_case("Latin1_General_100_BIN2") =>
                {
                    for part in &mut collation.0 {
                        if let ObjectNamePart::Identifier(name) = part {
                            name.value = name.value.to_lowercase();
                        }
                    }
                }
                Expr::Value(value) => {
                    if let Value::Number(number, false) = &mut value.value
                        && let Ok(value) = number.parse::<i32>()
                    {
                        *number = value.to_string();
                    }
                }
                _ => {}
            }
            if let Some(source) = crate::variant_cast::source(expr) {
                *expr = source.clone();
            }
            if let Some(field) = column(expr, self.sources, self.scope) {
                if let Some(index) = self
                    .fields
                    .iter()
                    .position(|candidate| std::ptr::eq(*candidate, field))
                {
                    *expr = Expr::Identifier(Ident::new(format!("__msduck_order_field_{index}")));
                }
            } else if let Expr::Identifier(name) = expr
                && name.value.starts_with('@')
                && self
                    .scope
                    .parameters
                    .contains_key(&name.value.to_lowercase())
            {
                name.value = name.value.to_lowercase();
            }
            std::ops::ControlFlow::Continue(())
        }
    }
    let fields = sources
        .iter()
        .chain(scope.rows.iter().filter_map(Option::as_deref).flatten())
        .flat_map(|source| &source.fields)
        .collect();
    let mut canonical = Canonical {
        sources,
        scope,
        fields,
    };
    let mut left = inner(left).clone();
    let mut right = inner(right).clone();
    let _ = VisitMut::visit(&mut left, &mut canonical);
    let _ = VisitMut::visit(&mut right, &mut canonical);
    left == right
}
fn integer_column(expr: &Expr, sources: &[Source], scope: &Scope) -> bool {
    column(expr, sources, scope).is_some_and(|field| {
        field
            .info
            .as_ref()
            .is_some_and(|info| info.system_type_id == Some(56))
    })
}
fn integer_operand(expr: &Expr, scope: &Scope) -> bool {
    match inner(expr) {
        Expr::Value(value) => {
            matches!(&value.value,Value::Number(number,false) if number.parse::<i32>().is_ok())
        }
        Expr::Identifier(name) if name.value.starts_with('@') => scope
            .parameters
            .get(&name.value.to_lowercase())
            .is_some_and(|info| info.system_type_id == Some(56)),
        _ => false,
    }
}
fn arithmetic(expr: &Expr, sources: &[Source], scope: &Scope) -> bool {
    matches!(inner(expr),Expr::BinaryOp {left,op:BinaryOperator::Plus|BinaryOperator::Multiply,right}
        if integer_column(left,sources,scope)&&integer_operand(right,scope))
}
fn sum_column(expr: &Expr, sources: &[Source], scope: &Scope) -> bool {
    let Expr::Function(f) = inner(expr) else {
        return false;
    };
    let FunctionArguments::List(args) = &f.args else {
        return false;
    };
    f.name.to_string().eq_ignore_ascii_case("SUM")
        && plain_function(f)
        && f.over.is_none()
        && args.clauses.is_empty()
        && args.duplicate_treatment.is_none()
        && matches!(args.args.as_slice(),[FunctionArg::Unnamed(FunctionArgExpr::Expr(value))] if integer_column(value,sources,scope))
}
fn ordered_expression(expr: &Expr, sources: &[Source], scope: &Scope) -> bool {
    if column(expr, sources, scope).is_some()
        || arithmetic(expr, sources, scope)
        || row_number(expr, sources, scope)
        || count_star(expr)
        || sum_column(expr, sources, scope)
    {
        return true;
    }
    match inner(expr) {
        Expr::Cast {
            expr,
            data_type: DataType::BigInt(None),
            kind: CastKind::Cast,
            format: None,
        } => integer_column(
            crate::variant_cast::source(expr).unwrap_or(expr),
            sources,
            scope,
        ),
        Expr::Collate { expr, collation } => {
            column(expr, sources, scope).is_some()
                && collation
                    .to_string()
                    .eq_ignore_ascii_case("Latin1_General_100_BIN2")
        }
        // This profile retains its ORDER ordinal, despite equal result branches.
        Expr::Case {
            operand: None,
            conditions,
            else_result: Some(other),
            ..
        } => {
            conditions.len() == 1
                && matches!(&conditions[0].condition,Expr::BinaryOp {left,op:BinaryOperator::Gt,right} if integer_column(left,sources,scope)&&matches!(inner(right),Expr::Value(v) if v.value==Value::Number("0".into(),false)))
                && matches!(inner(&conditions[0].result),Expr::Value(v) if v.value==Value::Number("1".into(),false))
                && matches!(inner(other),Expr::Value(v) if v.value==Value::Number("1".into(),false))
        }
        _ => false,
    }
}
fn folded(expr: &Expr) -> bool {
    typed_null(expr)
        || matches!(inner(expr),Expr::Value(value) if value.value==Value::Number("1".into(),false))
        || matches!(inner(expr),Expr::BinaryOp {left,op:BinaryOperator::Plus,right} if matches!(inner(left),Expr::Value(v) if v.value==Value::Number("1".into(),false))&&matches!(inner(right),Expr::Value(v) if v.value==Value::Number("2".into(),false)))
}
fn projected_expressions(select: &Select, sources: &[Source], scope: &Scope) -> Option<Vec<Expr>> {
    let mut out = Vec::new();
    let append = |out: &mut Vec<Expr>, source: &Source| -> Option<()> {
        let qualifier = source.qualifiers.first()?;
        for field in &source.fields {
            let mut ids = qualifier.split('.').map(Ident::new).collect::<Vec<_>>();
            ids.push(Ident::new(&field.name));
            out.push(Expr::CompoundIdentifier(ids));
        }
        Some(())
    };
    for item in &select.projection {
        match item {
            SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => {
                out.push(expr.clone())
            }
            SelectItem::Wildcard(options) if *options == WildcardAdditionalOptions::default() => {
                for source in sources {
                    append(&mut out, source)?;
                }
            }
            SelectItem::QualifiedWildcard(
                SelectItemQualifiedWildcardKind::ObjectName(name),
                options,
            ) if *options == WildcardAdditionalOptions::default() => {
                let qualifier = name
                    .0
                    .iter()
                    .map(|part| part.as_ident().map(|id| id.value.as_str()))
                    .collect::<Option<Vec<_>>>()?
                    .join(".");
                append(
                    &mut out,
                    crate::binding_scope::resolve_source(&qualifier, sources, &scope.rows)?,
                )?;
            }
            _ => return None,
        }
    }
    Some(out)
}
fn null_subquery(expr: &Expr) -> bool {
    let Expr::Subquery(query) = inner(expr) else {
        return false;
    };
    let Ok(expected) =
        sqlparser::parser::Parser::parse_sql(&crate::dialect::ServerDialect, "SELECT NULL")
    else {
        return false;
    };
    matches!(expected.as_slice(),[Statement::Query(expected)] if query == expected)
}
fn typed_null(expr: &Expr) -> bool {
    matches!(inner(expr),Expr::Cast {expr,data_type:DataType::Int(None),kind:CastKind::Cast,format:None}
        if matches!(inner(crate::variant_cast::source(expr).unwrap_or(expr)),Expr::Value(v) if v.value == Value::Null))
}
fn ordinal(expr: &Expr, width: usize) -> Option<usize> {
    let Expr::Value(value) = expr else {
        return None;
    };
    let Value::Number(number, false) = &value.value else {
        return None;
    };
    if !number.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    };
    let number = number.parse::<usize>().ok()?;
    (number > 0 && number <= width).then(|| number - 1)
}

fn plain_function(function: &Function) -> bool {
    function.parameters == FunctionArguments::None
        && function.filter.is_none()
        && function.null_treatment.is_none()
        && function.within_group.is_empty()
        && !function.uses_odbc_syntax
}
fn row_number(expr: &Expr, sources: &[Source], scope: &Scope) -> bool {
    let Expr::Function(f) = inner(expr) else {
        return false;
    };
    let FunctionArguments::List(args) = &f.args else {
        return false;
    };
    let Some(WindowType::WindowSpec(window)) = &f.over else {
        return false;
    };
    f.name.to_string().eq_ignore_ascii_case("ROW_NUMBER")
        && plain_function(f)
        && args.args.is_empty()
        && args.clauses.is_empty()
        && args.duplicate_treatment.is_none()
        && window.window_name.is_none()
        && window.window_frame.is_none()
        && !window.order_by.is_empty()
        && window
            .order_by
            .iter()
            .all(|key| column(&key.expr, sources, scope).is_some() || null_subquery(&key.expr))
        && window
            .partition_by
            .iter()
            .all(|key| column(key, sources, scope).is_some())
}
fn count_star(expr: &Expr) -> bool {
    let Expr::Function(f) = inner(expr) else {
        return false;
    };
    let FunctionArguments::List(args) = &f.args else {
        return false;
    };
    f.name.to_string().eq_ignore_ascii_case("COUNT")
        && plain_function(f)
        && f.over.is_none()
        && args.clauses.is_empty()
        && args.duplicate_treatment.is_none()
        && matches!(
            args.args.as_slice(),
            [FunctionArg::Unnamed(FunctionArgExpr::Wildcard)]
        )
}

/// Infer metadata for an original logical query before backend lowering. The
/// caller still binds and validates the query; Unknown must not become guessed
/// ORDER bytes. WHERE emptiness and parameter values never influence this plan.
pub fn infer(catalog: &CatalogSnapshot, query: &Query, outer: &Scope) -> Plan {
    let Some(order) = &query.order_by else {
        return Plan::NoToken;
    };
    let OrderByKind::Expressions(keys) = &order.kind else {
        return Plan::Unknown(Barrier::Shape);
    };
    if keys.is_empty()
        || query.for_clause.is_some()
        || order.interpolate.is_some()
        || keys.iter().any(|key| {
            key.with_fill.is_some()
                || key.options.nulls_first.is_some()
                || matches!(key.options.sort, Some(OrderBySort::Using(_)))
        })
    {
        return Plan::Unknown(Barrier::Shape);
    }
    if keys.len() > u16::MAX as usize / 2 {
        return Plan::Unknown(Barrier::Length);
    };
    let Some(mut fields) = super::member_fields(catalog, query, outer) else {
        return Plan::Unknown(Barrier::Projection);
    };
    if fields.is_empty() || fields.len() > u16::MAX as usize {
        return Plan::Unknown(Barrier::Projection);
    }
    let scope = super::declaration_scopes(catalog, query, outer).body;
    let (select, sources, expressions) = match query.body.as_ref() {
        SetExpr::Select(select) => {
            let Some(sources) = super::sources(catalog, select, &scope) else {
                return Plan::Unknown(Barrier::Source);
            };
            let Some(expressions) = projected_expressions(select, &sources, &scope) else {
                return Plan::Unknown(Barrier::Projection);
            };
            if expressions.len() != fields.len() {
                return Plan::Unknown(Barrier::Projection);
            };
            if fields.iter().zip(&expressions).any(|(field, expr)| {
                field.info.is_none()
                    && !row_number(expr, &sources, &scope)
                    && !count_star(expr)
                    && !sum_column(expr, &sources, &scope)
            }) {
                return Plan::Unknown(Barrier::Projection);
            }
            (Some(select.as_ref()), sources, expressions)
        }
        SetExpr::SetOperation {
            op: SetOperator::Union,
            left,
            right,
            ..
        } => {
            let mut branch = query.clone();
            branch.order_by = None;
            branch.body = left.clone();
            let Some(left) = super::member_fields(catalog, &branch, outer) else {
                return Plan::Unknown(Barrier::Projection);
            };
            branch.body = right.clone();
            let Some(right) = super::member_fields(catalog, &branch, outer) else {
                return Plan::Unknown(Barrier::Projection);
            };
            if left.len() != fields.len()
                || right.len() != fields.len()
                || left.iter().chain(&right).any(|field| field.info.is_none())
            {
                return Plan::Unknown(Barrier::Projection);
            }
            fields = left;
            (None, Vec::new(), Vec::new())
        }
        _ => return Plan::Unknown(Barrier::Shape),
    };
    let mut result = Vec::with_capacity(keys.len());
    for key in keys {
        let target = if let Some(index) = ordinal(&key.expr, fields.len()) {
            Some(index)
        } else if let Expr::Identifier(name) = inner(&key.expr) {
            let matches = fields
                .iter()
                .enumerate()
                .filter(|(_, field)| field.name.eq_ignore_ascii_case(&name.value))
                .map(|(i, _)| i)
                .collect::<Vec<_>>();
            if matches.len() > 1 {
                return Plan::Unknown(Barrier::AmbiguousName);
            };
            matches.first().copied()
        } else {
            None
        };
        if let Some(index) = target {
            if select.is_some() {
                let expression = &expressions[index];
                if folded(expression) {
                    continue;
                }
                if !ordered_expression(expression, &sources, &scope) {
                    return Plan::Unknown(Barrier::Constant);
                }
            }
            result.push((index + 1) as u16);
            continue;
        }
        if select.is_none() {
            return Plan::Unknown(Barrier::Expression);
        };
        if null_subquery(&key.expr) {
            result.push(0);
            continue;
        };
        if !ordered_expression(&key.expr, &sources, &scope) {
            return Plan::Unknown(Barrier::Expression);
        }
        let target = expressions
            .iter()
            .position(|expr| same(expr, &key.expr, &sources, &scope));
        result.push(target.map_or(0, |index| (index + 1) as u16));
    }
    if result.is_empty() {
        Plan::NoToken
    } else {
        Plan::Token(result)
    }
}
