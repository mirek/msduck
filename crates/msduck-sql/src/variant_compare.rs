//! Integer variants compare by numeric value, independently of their base tag.
use sqlparser::ast::*;
pub fn kind() -> DataType {
    DataType::Custom(ObjectName::from(vec![Ident::new("sql_variant")]), vec![])
}
pub fn known(expr: &Expr) -> bool {
    if crate::expression_metadata::conditional::candidate(expr) {
        return crate::variant_results::known(expr);
    }
    match expr {
        Expr::Cast { data_type, .. } => crate::variant_pack::is_variant(data_type),
        Expr::Convert {
            data_type: Some(data_type),
            ..
        } => crate::variant_pack::is_variant(data_type),
        Expr::Nested(expr) => known(expr),
        Expr::Function(f) => {
            let name = f.name.to_string().to_ascii_lowercase();
            if matches!(name.as_str(), "min" | "max")
                && let FunctionArguments::List(args) = &f.args
                && let [FunctionArg::Unnamed(FunctionArgExpr::Expr(value))] = args.args.as_slice()
            {
                return known(value);
            }
            matches!(
                name.as_str(),
                "__msduck_pack_integer_variant"
                    | "__msduck_variant_property"
                    | "sql_variant_property"
                    | "__msduck_variant_extreme_output"
            )
        }
        _ => false,
    }
}
pub fn key(expr: Expr) -> Expr {
    crate::expr::unary_function(
        "__msduck_variant_integer",
        crate::variant_pack::convert(expr),
    )
}
pub fn lower(expr: &mut Expr) {
    let values: Vec<&mut Expr> = match expr {
        Expr::BinaryOp {
            left,
            op:
                BinaryOperator::Eq
                | BinaryOperator::NotEq
                | BinaryOperator::Lt
                | BinaryOperator::LtEq
                | BinaryOperator::Gt
                | BinaryOperator::GtEq,
            right,
        }
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
    if values.iter().any(|value| known(value)) {
        for value in values {
            *value = key(value.clone());
        }
    }
}
pub fn subquery(value: &mut Expr, query: &mut Box<Query>) {
    let Statement::Query(mut wrapper) = sqlparser::parser::Parser::parse_sql(
        &sqlparser::dialect::GenericDialect {},
        "SELECT __variant_value FROM (SELECT NULL) AS __variant_source(__variant_value)",
    )
    .expect("static subquery wrapper")
    .remove(0) else {
        unreachable!()
    };
    let SetExpr::Select(select) = wrapper.body.as_mut() else {
        unreachable!()
    };
    select.projection = vec![SelectItem::UnnamedExpr(key(Expr::Identifier(Ident::new(
        "__variant_value",
    ))))];
    let TableFactor::Derived { subquery, .. } = &mut select.from[0].relation else {
        unreachable!()
    };
    std::mem::swap(subquery, query);
    *query = wrapper;
    *value = key(value.clone());
}
