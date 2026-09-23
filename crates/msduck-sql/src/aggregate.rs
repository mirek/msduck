//! Deterministic aggregate argument typing and validation.
use crate::parameter::Parameter;
use sqlparser::ast::*;
use std::collections::HashMap;

pub fn integer_rank(function: &Function, parameters: &HashMap<String, Parameter>) -> Option<u8> {
    if !matches!(
        function.name.to_string().to_ascii_uppercase().as_str(),
        "SUM" | "AVG" | "MIN" | "MAX"
    ) {
        return None;
    }
    let FunctionArguments::List(args) = &function.args else {
        return None;
    };
    let [FunctionArg::Unnamed(FunctionArgExpr::Expr(value))] = args.args.as_slice() else {
        return None;
    };
    crate::case_types::integer_rank(value, parameters).map(|rank| {
        if matches!(
            function.name.to_string().to_ascii_uppercase().as_str(),
            "MIN" | "MAX"
        ) {
            rank
        } else {
            rank.max(2)
        }
    })
}

const NESTED_AGGREGATE: &str =
    "Cannot perform an aggregate function on an expression containing an aggregate or a subquery.";
const NESTED_WINDOW: &str =
    "Windowed functions cannot be used in the context of another windowed function or aggregate.";

pub fn is_aggregate(function: &Function) -> bool {
    matches!(
        function.name.to_string().to_ascii_lowercase().as_str(),
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
            | "string_agg"
            | "checksum_agg"
            | "approx_count_distinct"
            | "any_value"
    )
}

pub fn validate_update<T: Visit>(value: &T) -> Result<(), msduck_core::diagnostic::SqlError> {
    use std::ops::ControlFlow;
    struct Assignments {
        queries: usize,
    }
    impl Visitor for Assignments {
        type Break = msduck_core::diagnostic::SqlError;
        fn pre_visit_query(&mut self, _: &Query) -> ControlFlow<Self::Break> {
            self.queries += 1;
            ControlFlow::Continue(())
        }
        fn post_visit_query(&mut self, _: &Query) -> ControlFlow<Self::Break> {
            self.queries -= 1;
            ControlFlow::Continue(())
        }
        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<Self::Break> {
            if self.queries == 0
                && matches!(expr, Expr::Function(f) if is_aggregate(f) && f.over.is_none())
            {
                return ControlFlow::Break(msduck_core::diagnostic::SqlError::syntax(
                    157,
                    1,
                    "An aggregate may not appear in the set list of an UPDATE statement.",
                ));
            }
            ControlFlow::Continue(())
        }
    }
    struct Statements;
    impl Visitor for Statements {
        type Break = msduck_core::diagnostic::SqlError;
        fn pre_visit_statement(&mut self, statement: &Statement) -> ControlFlow<Self::Break> {
            if let Statement::Update(update) = statement {
                update.assignments.visit(&mut Assignments { queries: 0 })?;
            }
            ControlFlow::Continue(())
        }
    }
    match value.visit(&mut Statements) {
        ControlFlow::Continue(()) => Ok(()),
        ControlFlow::Break(error) => Err(error),
    }
}

fn validate_window_nesting(function: &Function) -> Result<(), String> {
    use std::ops::ControlFlow;
    let aggregate = is_aggregate(function);
    if function.over.is_none() && !aggregate {
        return Ok(());
    }
    struct NestedWindow {
        queries: usize,
    }
    impl Visitor for NestedWindow {
        type Break = ();
        fn pre_visit_query(&mut self, _: &Query) -> ControlFlow<()> {
            self.queries += 1;
            ControlFlow::Continue(())
        }
        fn post_visit_query(&mut self, _: &Query) -> ControlFlow<()> {
            self.queries -= 1;
            ControlFlow::Continue(())
        }
        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
            if self.queries == 0 && matches!(expr, Expr::Function(f) if f.over.is_some()) {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        }
    }
    let mut visitor = NestedWindow { queries: 0 };
    if function.args.visit(&mut visitor).is_break()
        || function.over.visit(&mut visitor).is_break()
        || function.within_group.visit(&mut visitor).is_break()
    {
        return Err(NESTED_WINDOW.into());
    }
    Ok(())
}

pub fn validate(function: &Function) -> Result<(), String> {
    use std::ops::ControlFlow;
    crate::grouping::validate(function)?;
    validate_window_nesting(function)?;
    crate::ranking::validate(function)?;
    crate::window_frame::validate(function)?;
    let name = function.name.to_string().to_ascii_lowercase();
    if !matches!(
        name.as_str(),
        "sum" | "avg" | "min" | "max" | "count" | "count_big" | "stdev" | "stdevp" | "var" | "varp"
    ) {
        return Ok(());
    }
    let FunctionArguments::List(args) = &function.args else {
        return Err(format!("The {name} function requires 1 argument(s)."));
    };
    if args.args.len() != 1 {
        return Err(format!("The {name} function requires 1 argument(s)."));
    }
    if function.over.is_some() && args.duplicate_treatment == Some(DuplicateTreatment::Distinct) {
        return Err("Use of DISTINCT is not allowed with the OVER clause.".into());
    }
    if !matches!(function.parameters, FunctionArguments::None)
        || function.filter.is_some()
        || function.null_treatment.is_some()
        || !function.within_group.is_empty()
        || !args.clauses.is_empty()
    {
        return Err("unsupported aggregate modifiers".into());
    }
    let value = match &args.args[0] {
        FunctionArg::Unnamed(FunctionArgExpr::Wildcard)
            if matches!(name.as_str(), "count" | "count_big")
                && args.duplicate_treatment.is_none() =>
        {
            return Ok(());
        }
        FunctionArg::Unnamed(FunctionArgExpr::Expr(value)) => value,
        _ => return Err("unsupported aggregate argument".into()),
    };
    // A window aggregate may consume the results of grouping aggregates.
    // Ordinary aggregates may contain neither an aggregate nor a subquery.
    if function.over.is_none() {
        struct Nested;
        impl Visitor for Nested {
            type Break = ();
            fn pre_visit_query(&mut self, _: &Query) -> ControlFlow<()> {
                ControlFlow::Break(())
            }
            fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
                if let Expr::Function(f) = expr
                    && matches!(
                        f.name.to_string().to_ascii_lowercase().as_str(),
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
                            | "string_agg"
                            | "checksum_agg"
                            | "approx_count_distinct"
                            | "any_value"
                    )
                {
                    return ControlFlow::Break(());
                }
                ControlFlow::Continue(())
            }
        }
        if value.visit(&mut Nested).is_break() {
            return Err(NESTED_AGGREGATE.into());
        }
    }
    Ok(())
}

pub fn error_number(message: &str) -> Option<i32> {
    if message == NESTED_WINDOW {
        Some(4109)
    } else if message == NESTED_AGGREGATE {
        Some(130)
    } else if message == "Use of DISTINCT is not allowed with the OVER clause." {
        Some(10759)
    } else if message.starts_with("The ") && message.ends_with(" function requires 1 argument(s).")
    {
        Some(174)
    } else {
        None
    }
}

pub fn mark(expr: &mut Expr, parameters: &HashMap<String, Parameter>) -> Result<(), String> {
    let Expr::Function(function) = expr else {
        return Ok(());
    };
    validate(function)?;
    let name = function.name.to_string().to_ascii_lowercase();
    if matches!(name.as_str(), "count" | "count_big")
        && let FunctionArguments::List(args) = &mut function.args
        && args.duplicate_treatment == Some(DuplicateTreatment::Distinct)
        && let [FunctionArg::Unnamed(FunctionArgExpr::Expr(value))] = args.args.as_mut_slice()
        && crate::datetimeoffset_compare::scale(value, parameters).is_some()
    {
        *value = crate::datetimeoffset_compare::key(value.clone());
    }
    if let FunctionArguments::List(args) = &mut function.args
        && let [FunctionArg::Unnamed(FunctionArgExpr::Expr(value))] = args.args.as_mut_slice()
        && crate::variant_compare::known(value)
    {
        if matches!(
            name.as_str(),
            "sum" | "avg" | "stdev" | "stdevp" | "var" | "varp" | "approx_count_distinct"
        ) {
            return Err(format!(
                "Operand data type sql_variant is invalid for {name} operator."
            ));
        }
        if matches!(name.as_str(), "min" | "max") {
            *value = crate::expr::unary_function(
                "__msduck_variant_extreme_input",
                crate::variant_pack::convert(value.clone()),
            );
            args.duplicate_treatment = None;
            *expr = crate::expr::unary_function(
                "__msduck_variant_extreme_output",
                Expr::Function(function.clone()),
            );
            return Ok(());
        }
        if matches!(name.as_str(), "count" | "count_big")
            && args.duplicate_treatment == Some(DuplicateTreatment::Distinct)
        {
            *value = crate::variant_compare::key(value.clone());
        }
    }
    if matches!(
        name.as_str(),
        "sum" | "avg" | "min" | "max" | "stdev" | "stdevp" | "var" | "varp"
    ) && let FunctionArguments::List(args) = &function.args
        && let [FunctionArg::Unnamed(FunctionArgExpr::Expr(value))] = args.args.as_slice()
        && crate::case_types::is_bit(value, parameters)
    {
        return Err(format!(
            "Operand data type bit is invalid for {name} operator."
        ));
    }
    let statistical = match name.as_str() {
        "stdev" => Some("stddev_samp"),
        "stdevp" => Some("stddev_pop"),
        "var" => Some("var_samp"),
        "varp" => Some("var_pop"),
        _ => None,
    };
    if name == "avg"
        && let FunctionArguments::List(args) = &mut function.args
        && let [FunctionArg::Unnamed(FunctionArgExpr::Expr(value))] = args.args.as_mut_slice()
        && let Some(kind) = crate::expression_metadata::storage::kind(value, parameters, &|_| None)
        && let Ok(msduck_core::types::Type::Decimal(decimal)) = crate::sql_type::declaration(&kind)
    {
        // Widen precision without changing scale; keep DISTINCT and the window
        // on the same aggregate, with exactly one evaluation of its input.
        *value = Expr::Cast {
            kind: CastKind::Cast,
            expr: Box::new(value.clone()),
            data_type: DataType::Decimal(ExactNumberInfo::PrecisionAndScale(
                38,
                i64::from(decimal.scale()),
            )),
            format: None,
        };
        function.name = ObjectName::from(vec![Ident::new(format!(
            "__msduck_avg_decimal_{}",
            decimal.scale()
        ))]);
        return Ok(());
    }
    if matches!(name.as_str(), "sum" | "avg")
        && let FunctionArguments::List(args) = &mut function.args
        && let [FunctionArg::Unnamed(FunctionArgExpr::Expr(value))] = args.args.as_mut_slice()
        && crate::expression_metadata::currency::kind(value, parameters, &|_| None).is_some()
    {
        // Both currency families accumulate in MONEY. Keep DISTINCT and OVER
        // on the aggregate and evaluate its original source only once.
        crate::money_cast::coerce(value, msduck_core::money::MoneyType::Money);
        function.name = ObjectName::from(vec![Ident::new(format!("__msduck_{name}_money"))]);
        return Ok(());
    }
    if let Some(target) = statistical {
        if let FunctionArguments::List(args) = &function.args
            && args.duplicate_treatment == Some(DuplicateTreatment::Distinct)
        {
            // DuckDB statistical aggregates bind DOUBLE inputs before DISTINCT,
            // collapsing exact BIGINT/DECIMAL values. Deduplicate their original
            // type first, then apply the statistic to the resulting list.
            let mut values = function.clone();
            values.name = ObjectName::from(vec![Ident::new("list")]);
            function.name = ObjectName::from(vec![Ident::new(format!("list_{target}"))]);
            if let FunctionArguments::List(args) = &mut function.args {
                args.duplicate_treatment = None;
                args.args = vec![FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Function(
                    values,
                )))];
            }
            return Ok(());
        }
        function.name = ObjectName::from(vec![Ident::new(target)]);
        return Ok(());
    }
    let Some(rank) = integer_rank(function, parameters) else {
        return Ok(());
    };
    // MIN/MAX already preserve the argument type in DuckDB. Their known
    // integer outputs still participate in the shared inference and validation.
    if matches!(name.as_str(), "min" | "max") {
        return Ok(());
    }
    function.name = ObjectName::from(vec![Ident::new(format!(
        "__msduck_{name}_{}",
        if rank == 3 { "big" } else { "int" }
    ))]);
    if let FunctionArguments::List(args) = &mut function.args
        && let FunctionArg::Unnamed(FunctionArgExpr::Expr(value)) = &mut args.args[0]
    {
        *value = Expr::Cast {
            kind: CastKind::Cast,
            expr: Box::new(value.clone()),
            data_type: if rank == 3 {
                DataType::BigInt(None)
            } else {
                DataType::Int(None)
            },
            format: None,
        };
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::ServerDialect;
    use sqlparser::parser::Parser;

    #[test]
    fn update_aggregate_placement_keeps_scalar_query_boundaries() {
        for (sql, rejected) in [
            ("UPDATE t SET n=ABS(SUM(t.n))", true),
            ("UPDATE t SET n=(SELECT MAX(q.n) FROM q)", false),
            (
                "WITH q AS (SELECT SUM(n) AS n FROM s) UPDATE t SET n=q.n FROM t CROSS JOIN q",
                false,
            ),
        ] {
            let statement = Parser::parse_sql(&ServerDialect, sql).unwrap().remove(0);
            let result = validate_update(&statement);
            assert_eq!(result.is_err(), rejected, "{sql}");
            if let Err(error) = result {
                assert_eq!(error.number, 157);
                assert_eq!(error.severity, 15);
            }
        }
    }

    fn function(sql: &str) -> Function {
        let Statement::Query(query) = Parser::parse_sql(&ServerDialect, sql).unwrap().remove(0)
        else {
            panic!("query")
        };
        let SetExpr::Select(select) = *query.body else {
            panic!("select")
        };
        let SelectItem::UnnamedExpr(Expr::Function(function)) =
            select.projection.into_iter().next().unwrap()
        else {
            panic!("function")
        };
        function
    }

    #[test]
    fn nested_aggregate_validation_respects_window_and_query_boundaries() {
        for (sql, number) in [
            ("SELECT SUM(AVG(n))", 130),
            ("SELECT SUM((SELECT n FROM t))", 130),
            ("SELECT SUM(ROW_NUMBER() OVER(ORDER BY n))", 4109),
            ("SELECT SUM(DISTINCT n) OVER()", 10759),
        ] {
            let function = function(sql);
            let before = function.clone();
            assert_eq!(
                error_number(&validate(&function).unwrap_err()),
                Some(number),
                "{sql}"
            );
            assert_eq!(function, before);
        }
        for sql in [
            "SELECT SUM(SUM(n)) OVER()",
            "SELECT COUNT(*)",
            "SELECT SUM(n) OVER(ORDER BY n)",
            "SELECT ROW_NUMBER() OVER(ORDER BY (SELECT SUM(n) FROM t))",
        ] {
            validate(&function(sql)).unwrap();
        }
    }
}
