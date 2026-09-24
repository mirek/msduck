//! SQL Server grouping indicator signatures and result widths.
use sqlparser::ast::*;

pub fn result_type(function: &Function) -> Option<DataType> {
    match function.name.to_string().to_ascii_lowercase().as_str() {
        "grouping" => Some(DataType::UTinyInt),
        "grouping_id" => Some(DataType::Int(None)),
        _ => None,
    }
}
pub fn validate(function: &Function) -> Result<(), String> {
    if result_type(function).is_none() {
        return Ok(());
    }
    let name = function.name.to_string().to_ascii_lowercase();
    let FunctionArguments::List(args) = &function.args else {
        return Err(format!("The {name} function requires 1 argument(s)."));
    };
    if (name == "grouping" && args.args.len() != 1) || args.args.is_empty() {
        return Err(format!("The {name} function requires 1 argument(s)."));
    }
    if function.over.is_some()
        || function.filter.is_some()
        || function.null_treatment.is_some()
        || !function.within_group.is_empty()
        || !matches!(function.parameters, FunctionArguments::None)
        || args.duplicate_treatment.is_some()
        || !args.clauses.is_empty()
        || args
            .args
            .iter()
            .any(|arg| !matches!(arg, FunctionArg::Unnamed(FunctionArgExpr::Expr(_))))
    {
        return Err(format!("unsupported {name} function syntax"));
    }
    Ok(())
}
pub fn lower(expr: &mut Expr) {
    if let Expr::Function(function) = expr
        && let Some(data_type) = result_type(function)
    {
        *expr = Expr::Cast {
            kind: CastKind::Cast,
            expr: Box::new(expr.clone()),
            data_type,
            format: None,
        };
    }
}

const TOO_MANY_SETS: &str = "Too many grouping sets. The maximum number is 4096.";
pub fn error_number(message: &str) -> Option<i32> {
    match message {
        MIXED_LEGACY => Some(10702),
        TOO_MANY_SETS => Some(10703),
        TOO_MANY_EXPRESSIONS => Some(10706),
        _ => None,
    }
}
// Count combinations without expanding them. Preserve duplicate grouping sets.
pub fn sets(select: &mut Select) -> Result<(), String> {
    fn product(values: &mut [Expr]) -> Result<usize, String> {
        values.iter_mut().try_fold(1usize, |total, value| {
            Ok(total.saturating_mul(count(value)?).min(4097))
        })
    }
    fn count(expr: &mut Expr) -> Result<usize, String> {
        // sqlparser parses CUBE/ROLLUP inside GROUPING SETS as function calls.
        if let Expr::Function(f) = expr {
            let name = f.name.to_string().to_ascii_lowercase();
            if matches!(name.as_str(), "cube" | "rollup") {
                let FunctionArguments::List(args) = &f.args else {
                    return Err("invalid grouping construct".into());
                };
                if args.duplicate_treatment.is_some()
                    || !args.clauses.is_empty()
                    || f.over.is_some()
                {
                    return Err("invalid grouping construct".into());
                }
                let groups = args
                    .args
                    .iter()
                    .map(|arg| match arg {
                        FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Tuple(values))) => {
                            Ok(values.clone())
                        }
                        FunctionArg::Unnamed(FunctionArgExpr::Expr(value)) => {
                            Ok(vec![value.clone()])
                        }
                        _ => Err("invalid grouping construct".to_string()),
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                *expr = if name == "cube" {
                    Expr::Cube(groups)
                } else {
                    Expr::Rollup(groups)
                };
            }
        }
        match expr {
            Expr::GroupingSets(groups) => groups.iter_mut().try_fold(0usize, |total, group| {
                Ok(total.saturating_add(product(group)?).min(4097))
            }),
            Expr::Cube(groups) => groups.iter_mut().try_fold(1usize, |total, group| {
                Ok(total.saturating_mul(1 + product(group)?).min(4097))
            }),
            Expr::Rollup(groups) => {
                let mut total = 1usize;
                let mut prefix = 1usize;
                for group in groups {
                    prefix = prefix.saturating_mul(product(group)?).min(4097);
                    total = total.saturating_add(prefix).min(4097);
                }
                Ok(total)
            }
            _ => Ok(1),
        }
    }
    if let GroupByExpr::Expressions(values, _) = &mut select.group_by
        && product(values)? > 4096
    {
        return Err(TOO_MANY_SETS.into());
    }
    Ok(())
}
// Call only after set-count and distinct-expression validation.
pub fn expand_sets(select: &mut Select) {
    fn combine(left: Vec<Vec<Expr>>, right: Vec<Vec<Expr>>) -> Vec<Vec<Expr>> {
        left.into_iter()
            .flat_map(|a| {
                right.iter().map(move |b| {
                    let mut row = a.clone();
                    row.extend(b.iter().cloned());
                    row
                })
            })
            .collect()
    }
    fn tuple(values: &[Expr]) -> Vec<Vec<Expr>> {
        values
            .iter()
            .fold(vec![vec![]], |sets, value| combine(sets, expand(value)))
    }
    fn expand(expr: &Expr) -> Vec<Vec<Expr>> {
        match expr {
            Expr::GroupingSets(groups) => groups.iter().flat_map(|group| tuple(group)).collect(),
            Expr::Cube(groups) => groups.iter().fold(vec![vec![]], |sets, group| {
                let mut choices = vec![vec![]];
                choices.extend(tuple(group));
                combine(sets, choices)
            }),
            Expr::Rollup(groups) => (0..=groups.len())
                .flat_map(|len| {
                    groups[..len]
                        .iter()
                        .fold(vec![vec![]], |sets, group| combine(sets, tuple(group)))
                })
                .collect(),
            _ => vec![vec![expr.clone()]],
        }
    }
    if let GroupByExpr::Expressions(values, _) = &mut select.group_by {
        // DuckDB does not bind sqlparser's parenthesized nested CUBE syntax.
        // Expand only after the total has been bounded, keeping duplicate sets.
        for value in values {
            if matches!(value, Expr::GroupingSets(_)) {
                *value = Expr::GroupingSets(expand(value));
            }
        }
    }
}

const TOO_MANY_EXPRESSIONS: &str = "Too many expressions are specified in the GROUP BY clause. The maximum number is 32 when grouping sets are supplied.";
pub fn columns(select: &Select, mut canonical: impl FnMut(&Expr) -> Expr) -> Result<(), String> {
    fn collect<'a>(expr: &'a Expr, values: &mut Vec<&'a Expr>, advanced: &mut bool) {
        match expr {
            Expr::GroupingSets(groups) | Expr::Cube(groups) | Expr::Rollup(groups) => {
                *advanced = true;
                for value in groups.iter().flatten() {
                    collect(value, values, advanced);
                }
            }
            Expr::Tuple(values) if values.is_empty() => {}
            _ => values.push(expr),
        }
    }
    let GroupByExpr::Expressions(groups, _) = &select.group_by else {
        return Ok(());
    };
    let mut values = vec![];
    let mut advanced = false;
    for value in groups {
        collect(value, &mut values, &mut advanced);
    }
    if advanced {
        let mut distinct = std::collections::HashSet::new();
        for value in values {
            distinct.insert(canonical(value));
            if distinct.len() > 32 {
                return Err(TOO_MANY_EXPRESSIONS.into());
            }
        }
    }
    Ok(())
}

const MIXED_LEGACY: &str = "The WITH CUBE and WITH ROLLUP options are not allowed with a ROLLUP, CUBE, or GROUPING SETS specification.";
pub fn legacy(select: &mut Select, mut canonical: impl FnMut(&Expr) -> Expr) -> Result<(), String> {
    if matches!(&select.group_by, GroupByExpr::All(modifiers) if !modifiers.is_empty()) {
        return Err("unsupported GROUP BY ALL modifier".into());
    }
    let GroupByExpr::Expressions(values, modifiers) = &mut select.group_by else {
        return Ok(());
    };
    if modifiers.is_empty() {
        return Ok(());
    }
    if modifiers.len() != 1
        || !matches!(
            modifiers[0],
            GroupByWithModifier::Cube | GroupByWithModifier::Rollup
        )
    {
        return Err("unsupported GROUP BY modifier".into());
    }
    if values.iter().any(|value| {
        matches!(
            value,
            Expr::Cube(_) | Expr::Rollup(_) | Expr::GroupingSets(_)
        )
    }) {
        return Err(MIXED_LEGACY.into());
    }
    let mut seen = std::collections::HashSet::new();
    let mut groups = vec![];
    for value in values.iter() {
        if seen.insert(canonical(value)) {
            groups.push(vec![value.clone()]);
        }
    }
    if groups.len() > 12 {
        return Err(
            "The maximum number of grouping expressions for WITH CUBE or WITH ROLLUP is 12.".into(),
        );
    }
    let cube = matches!(modifiers[0], GroupByWithModifier::Cube);
    modifiers.clear();
    *values = vec![if cube {
        Expr::Cube(groups)
    } else {
        Expr::Rollup(groups)
    }];
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn check(group: &str) -> Result<(), String> {
        let mut statements = sqlparser::parser::Parser::parse_sql(
            &crate::dialect::ServerDialect,
            &format!("SELECT 1 GROUP BY {group}"),
        )
        .unwrap();
        let Statement::Query(query) = &mut statements[0] else {
            unreachable!()
        };
        let SetExpr::Select(select) = query.body.as_mut() else {
            unreachable!()
        };
        sets(select)
    }
    #[test]
    fn grouping_set_counts_include_products_and_duplicate_totals() {
        let columns = (0..12)
            .map(|i| format!("c{i}"))
            .collect::<Vec<_>>()
            .join(",");
        assert!(check(&format!("CUBE({columns})")).is_ok());
        assert_eq!(
            check(&format!("CUBE({columns},c12)")),
            Err(TOO_MANY_SETS.into())
        );
        assert_eq!(
            check(&format!("GROUPING SETS(CUBE({columns}),())")),
            Err(TOO_MANY_SETS.into())
        );
        assert_eq!(
            check(&format!("CUBE({columns}),ROLLUP(extra)")),
            Err(TOO_MANY_SETS.into())
        );
        assert!(check(&format!("GROUPING SETS({})", vec!["()"; 4096].join(","))).is_ok());
        assert_eq!(
            check(&format!("GROUPING SETS({})", vec!["()"; 4097].join(","))),
            Err(TOO_MANY_SETS.into())
        );
        assert!(check("GROUPING SETS(CUBE((a,b),c),ROLLUP(a,b),())").is_ok());
    }
}
