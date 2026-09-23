//! Compare exact DATETIME2 values independently of their declared scale.
use sqlparser::ast::*;

pub fn scale(
    expr: &Expr,
    parameters: &std::collections::HashMap<String, crate::parameter::Parameter>,
) -> Option<u8> {
    if crate::expression_metadata::conditional::candidate(expr) {
        return crate::expression_metadata::conditional::values(expr)
            .into_iter()
            .filter_map(|v| scale(v, parameters))
            .max();
    }
    match expr {
        Expr::Cast { data_type, .. } => crate::datetime2_cast::scale(data_type).ok().flatten(),
        Expr::Identifier(id) => {
            crate::datetime2_cast::scale(&parameters.get(&id.value.to_lowercase())?.ast_type())
                .ok()
                .flatten()
        }
        Expr::Nested(value) => scale(value, parameters),
        Expr::Function(function) => {
            if let Some(scale) =
                crate::expression_metadata::temporal::datetime2fromparts_scale(function)
            {
                return Some(scale);
            }
            let name = function.name.to_string().to_ascii_lowercase();
            if matches!(
                name.as_str(),
                "min" | "max" | "first_value" | "last_value" | "lag" | "lead" | "quantile_disc"
            ) && let FunctionArguments::List(args) = &function.args
                && let Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(value))) = args.args.first()
            {
                return scale(value, parameters);
            }
            if name == "dateadd"
                && let FunctionArguments::List(args) = &function.args
                && let Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(value))) = args.args.get(2)
            {
                return scale(value, parameters);
            }
            let suffix = name
                .strip_prefix("__msduck_datetime2_cast_")
                .or_else(|| name.strip_prefix("__msduck_datetime2_try_"))
                .or_else(|| name.strip_prefix("__msduck_datetime2_dateadd_"))?;
            suffix.parse::<u8>().ok().filter(|s| *s <= 7)
        }
        _ => None,
    }
}

pub fn is_comparison(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::BinaryOp {
            op: BinaryOperator::Eq
                | BinaryOperator::NotEq
                | BinaryOperator::Lt
                | BinaryOperator::LtEq
                | BinaryOperator::Gt
                | BinaryOperator::GtEq,
            ..
        } | Expr::IsDistinctFrom(_, _)
            | Expr::IsNotDistinctFrom(_, _)
            | Expr::Between { .. }
            | Expr::InList { .. }
            | Expr::InSubquery { .. }
            | Expr::Case {
                operand: Some(_),
                ..
            }
    )
}

pub fn lower(expr: &mut Expr) {
    if !is_comparison(expr) {
        return;
    }
    let values: Vec<&mut Expr> = match expr {
        Expr::BinaryOp { left, right, .. }
        | Expr::IsDistinctFrom(left, right)
        | Expr::IsNotDistinctFrom(left, right) => vec![left.as_mut(), right.as_mut()],
        Expr::Between {
            expr, low, high, ..
        } => vec![expr.as_mut(), low.as_mut(), high.as_mut()],
        Expr::InList { expr, list, .. } => std::iter::once(expr.as_mut())
            .chain(list.iter_mut())
            .collect(),
        Expr::Case {
            operand: Some(input),
            conditions,
            ..
        } => std::iter::once(input.as_mut())
            .chain(conditions.iter_mut().map(|c| &mut c.condition))
            .collect(),
        _ => return,
    };
    let parameters = std::collections::HashMap::new();
    if values
        .iter()
        .any(|value| scale(value, &parameters).is_some())
    {
        for value in values {
            // DuckDB can evaluate STRUCT comparisons in projections but its
            // BETWEEN filter path requires an orderable scalar. Exact ticks
            // are the common comparison key; conversion also propagates NULL
            // to the child field before extraction.
            *value = key(value.clone());
        }
    }
}

/// Extract an orderable comparison key without changing a value's precision.
pub fn key(value: Expr) -> Expr {
    crate::expr::binary_function(
        "struct_extract",
        crate::datetime2_cast::convert(value, 7),
        Expr::Value(Value::SingleQuotedString("__msduck_datetime2_7".into()).into()),
    )
}

pub fn subquery(value: &mut Expr, query: &mut Box<Query>) {
    use sqlparser::{dialect::GenericDialect, parser::Parser};
    // Wrap the complete source so DISTINCT, ordering and row limits retain
    // their original meaning. Attach user SQL as an AST, never as text.
    let Statement::Query(mut wrapper) = Parser::parse_sql(
        &GenericDialect {},
        "SELECT __dt2_value FROM (SELECT NULL) AS __dt2_source(__dt2_value)",
    )
    .expect("constant subquery wrapper")
    .remove(0) else {
        unreachable!()
    };
    let SetExpr::Select(select) = wrapper.body.as_mut() else {
        unreachable!()
    };
    select.projection = vec![SelectItem::UnnamedExpr(key(Expr::Identifier(Ident::new(
        "__dt2_value",
    ))))];
    let TableFactor::Derived { subquery, .. } = &mut select.from[0].relation else {
        unreachable!()
    };
    std::mem::swap(subquery, query);
    *query = wrapper;
    *value = key(value.clone());
}
