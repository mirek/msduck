//! Ranking signatures and explicit window specifications.
use sqlparser::ast::*;

pub fn returns_bigint(function: &Function) -> bool {
    matches!(
        function.name.to_string().to_ascii_lowercase().as_str(),
        "row_number" | "rank" | "dense_rank" | "ntile"
    )
}

pub fn validate(function: &Function) -> Result<(), String> {
    let name = function.name.to_string().to_ascii_lowercase();
    if !returns_bigint(function) && !matches!(name.as_str(), "percent_rank" | "cume_dist") {
        return Ok(());
    }
    let count = if name == "ntile" { 1 } else { 0 };
    let FunctionArguments::List(args) = &function.args else {
        return Err(format!("The {name} function requires {count} argument(s)."));
    };
    if args.args.len() != count {
        return Err(format!("The {name} function requires {count} argument(s)."));
    }
    if name == "ntile"
        && !matches!(
            &args.args[0],
            FunctionArg::Unnamed(FunctionArgExpr::Expr(_))
        )
    {
        return Err("unsupported NTILE argument".into());
    }
    if !matches!(function.parameters, FunctionArguments::None)
        || args.duplicate_treatment.is_some()
        || !args.clauses.is_empty()
        || function.filter.is_some()
        || function.null_treatment.is_some()
        || !function.within_group.is_empty()
    {
        return Err("unsupported ranking function modifiers".into());
    }
    let Some(over) = &function.over else {
        return Err(format!("The function '{name}' must have an OVER clause."));
    };
    if let WindowType::WindowSpec(spec) = over {
        if spec.window_frame.is_some() {
            return Err(format!(
                "The function '{name}' may not have a window frame."
            ));
        }
        // Query translation expands named window inheritance before validation.
        if spec.order_by.is_empty() && spec.window_name.is_none() {
            return Err(format!(
                "The function '{name}' must have an OVER clause with ORDER BY."
            ));
        }
    }
    Ok(())
}

pub fn error_number(message: &str) -> Option<i32> {
    for name in [
        "row_number",
        "rank",
        "dense_rank",
        "ntile",
        "percent_rank",
        "cume_dist",
        "percentile_cont",
        "percentile_disc",
        "first_value",
        "last_value",
        "lag",
        "lead",
    ] {
        if message == format!("The function '{name}' takes between 1 and 3 arguments.") {
            return Some(10755);
        }
        if message == format!("The {name} function requires 0 argument(s).") {
            return Some(174);
        }
        if message == format!("The function '{name}' must have a WITHIN GROUP clause.") {
            return Some(10754);
        }
        if message == format!("The function '{name}' must have an OVER clause.") {
            return Some(10753);
        }
        if message == format!("The function '{name}' must have an OVER clause with ORDER BY.") {
            return Some(4112);
        }
        if message == format!("The function '{name}' may not have a window frame.") {
            return Some(4106);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::ServerDialect;
    use sqlparser::parser::Parser;

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
    fn window_validation_keeps_names_and_stable_diagnostics() {
        for (sql, number) in [
            ("SELECT ROW_NUMBER()", 10753),
            ("SELECT ROW_NUMBER() OVER()", 4112),
            ("SELECT RANK(1) OVER(ORDER BY n)", 174),
            (
                "SELECT DENSE_RANK() OVER(ORDER BY n ROWS UNBOUNDED PRECEDING)",
                4106,
            ),
        ] {
            let function = function(sql);
            let original = function.clone();
            let error = validate(&function).unwrap_err();
            assert_eq!(error_number(&error), Some(number), "{sql}: {error}");
            assert_eq!(function, original);
        }
        for sql in [
            "SELECT ROW_NUMBER() OVER(ORDER BY n)",
            "SELECT NTILE(@buckets) OVER(PARTITION BY a ORDER BY n)",
            "SELECT RANK() OVER w",
            "SELECT PERCENT_RANK() OVER(ORDER BY n)",
        ] {
            validate(&function(sql)).unwrap();
        }
        assert_eq!(error_number("unrelated backend error"), None);
    }
}
