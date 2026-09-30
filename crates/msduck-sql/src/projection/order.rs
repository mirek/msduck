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
    let (left, right) = (inner(left), inner(right));
    if let (Some(left), Some(right)) = (column(left, sources, scope), column(right, sources, scope))
    {
        // Both references resolve within the same explicit source snapshot.
        // Identity distinguishes equally named columns from different sources.
        return std::ptr::eq(left, right);
    }
    match (left, right) {
        (
            Expr::BinaryOp {
                left: a,
                op: x,
                right: b,
            },
            Expr::BinaryOp {
                left: c,
                op: y,
                right: d,
            },
        ) => x == y && same(a, c, sources, scope) && same(b, d, sources, scope),
        (Expr::Value(a), Expr::Value(b)) => a == b,
        _ => left == right,
    }
}
fn plus_column(expr: &Expr, sources: &[Source], scope: &Scope) -> bool {
    let Expr::BinaryOp {
        left,
        op: BinaryOperator::Plus,
        right,
    } = inner(expr)
    else {
        return false;
    };
    column(left, sources, scope).is_some_and(|field| {
        field
            .info
            .as_ref()
            .is_some_and(|info| info.system_type_id == Some(56))
    }) && matches!(inner(right),Expr::Value(v) if matches!(&v.value, Value::Number(n,false) if n == "1"))
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
            .all(|key| column(&key.expr, sources, scope).is_some())
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
            let expressions = select
                .projection
                .iter()
                .map(|item| match item {
                    SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => {
                        Some(expr)
                    }
                    _ => None,
                })
                .collect::<Option<Vec<_>>>();
            let Some(expressions) = expressions else {
                return Plan::Unknown(Barrier::Projection);
            };
            if expressions.len() != fields.len() {
                return Plan::Unknown(Barrier::Projection);
            };
            if fields.iter().zip(&expressions).any(|(field, expr)| {
                field.info.is_none() && !row_number(expr, &sources, &scope) && !count_star(expr)
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
                let expression = expressions[index];
                if keys.len() == 1
                    && typed_null(expression)
                    && matches!(inner(&key.expr), Expr::Identifier(_))
                {
                    return Plan::NoToken;
                }
                if column(expression, &sources, &scope).is_none()
                    && !plus_column(expression, &sources, &scope)
                    && !row_number(expression, &sources, &scope)
                {
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
        if column(&key.expr, &sources, &scope).is_none()
            && !plus_column(&key.expr, &sources, &scope)
        {
            return Plan::Unknown(Barrier::Expression);
        }
        let target = expressions
            .iter()
            .position(|expr| same(expr, &key.expr, &sources, &scope));
        result.push(target.map_or(0, |index| (index + 1) as u16));
    }
    Plan::Token(result)
}
