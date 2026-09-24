//! Preserve integer variant result types through conditional expressions.
use sqlparser::ast::*;
pub fn known(expr: &Expr) -> bool {
    if crate::expression_metadata::conditional::candidate(expr) {
        return crate::expression_metadata::conditional::values(expr)
            .into_iter()
            .any(known);
    }
    match expr {
        Expr::Cast { data_type, .. }
        | Expr::Convert {
            data_type: Some(data_type),
            ..
        } => crate::variant_pack::is_variant(data_type),
        Expr::Nested(expr) => known(expr),
        Expr::Function(_) => crate::variant_compare::known(expr),
        _ => false,
    }
}
pub fn lower(expr: &mut Expr) {
    if !crate::expression_metadata::conditional::candidate(expr) || !known(expr) {
        return;
    }
    if let Expr::Function(f) = expr {
        for name in ["ISNULL", "__msduck_isnull"] {
            if let Ok(Some([first, replacement])) =
                crate::expression_metadata::conditional::binary_args(f, name)
            {
                *expr = crate::expr::binary_function(
                    "coalesce",
                    crate::variant_pack::convert(first.clone()),
                    crate::variant_pack::convert(replacement.clone()),
                );
                return;
            }
        }
    }
    match expr {
        Expr::Case {
            conditions,
            else_result,
            ..
        } => {
            for value in conditions
                .iter_mut()
                .map(|c| &mut c.result)
                .chain(else_result.iter_mut().map(|v| v.as_mut()))
            {
                *value = crate::variant_pack::convert(value.clone());
            }
        }
        Expr::Function(f) => {
            let skip = match f.name.to_string().to_ascii_uppercase().as_str() {
                "COALESCE" => 0,
                "IIF" | "CHOOSE" => 1,
                _ => return,
            };
            if let FunctionArguments::List(args) = &mut f.args {
                for arg in args.args.iter_mut().skip(skip) {
                    if let FunctionArg::Unnamed(FunctionArgExpr::Expr(value)) = arg {
                        *value = crate::variant_pack::convert(value.clone());
                    }
                }
            }
        }
        _ => {}
    }
}
