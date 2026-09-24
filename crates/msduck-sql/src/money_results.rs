//! Convert currency result branches before native conditional type unification.
use crate::{
    expression_metadata::{conditional, currency},
    parameter::Parameter,
};
use sqlparser::ast::*;
use std::collections::HashMap;

pub fn lower(
    expr: &mut Expr,
    parameters: &HashMap<String, Parameter>,
    column: &impl Fn(&Expr) -> Option<DataType>,
) {
    if !conditional::candidate(expr) {
        return;
    }
    let Some(kind) = currency::kind(expr, parameters, column) else {
        return;
    };
    let cast = |value: &mut Expr| crate::money_cast::coerce(value, kind);
    match expr {
        Expr::Case {
            conditions,
            else_result,
            ..
        } => {
            for branch in conditions {
                cast(&mut branch.result);
            }
            if let Some(value) = else_result {
                cast(value);
            }
        }
        Expr::Function(f) => {
            let start = match f.name.to_string().to_ascii_uppercase().as_str() {
                "IIF" | "CHOOSE" => 1,
                "ISNULL" | "__MSDUCK_ISNULL" | "COALESCE" => 0,
                // NULLIF's comparison type is independent of its result type.
                _ => return,
            };
            if let FunctionArguments::List(args) = &mut f.args {
                for arg in args.args.iter_mut().skip(start) {
                    if let FunctionArg::Unnamed(FunctionArgExpr::Expr(value)) = arg {
                        cast(value);
                    }
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn expr(sql: &str) -> Expr {
        sqlparser::parser::Parser::new(&crate::dialect::ServerDialect)
            .try_with_sql(sql)
            .unwrap()
            .parse_expr()
            .unwrap()
    }
    #[test]
    fn currency_casts_preserve_conditions_selectors_and_existing_try() {
        for sql in [
            "CASE WHEN nextval('condition')=1 THEN '$12' ELSE CAST(1 AS MONEY) END",
            "IIF(nextval('condition')=1,'$12',CAST(1 AS MONEY))",
            "CHOOSE(nextval('selector'),CAST(1 AS MONEY),'$12')",
            "COALESCE(TRY_CAST('bad' AS MONEY),'$12')",
            "ISNULL(CAST(NULL AS SMALLMONEY),nextval('replacement'))",
        ] {
            let mut value = expr(sql);
            lower(&mut value, &HashMap::new(), &|_| None);
            let once = value.clone();
            lower(&mut value, &HashMap::new(), &|_| None);
            assert_eq!(value, once, "{sql}");
            assert_eq!(
                value.to_string().matches("nextval").count(),
                sql.matches("nextval").count()
            );
            if sql.contains("TRY_CAST") {
                assert!(value.to_string().contains("TRY_CAST('bad' AS MONEY)"));
            }
        }
    }
    #[test]
    fn higher_precedence_and_unknown_results_are_not_narrowed() {
        for sql in [
            "CASE WHEN 1=1 THEN CAST(1 AS MONEY) ELSE CAST(1 AS DECIMAL(30,5)) END",
            "COALESCE(CAST(NULL AS MONEY),unknown_value)",
            "NULLIF(CAST(1 AS MONEY),CAST(1 AS DECIMAL(30,5)))",
        ] {
            let mut value = expr(sql);
            let original = value.clone();
            lower(&mut value, &HashMap::new(), &|_| None);
            assert_eq!(value, original);
        }
    }
}
