//! Deterministic expression metadata rules shared by binding and lowering.
use sqlparser::ast::*;

pub fn isnull_args(function: &Function) -> Result<Option<[&Expr; 2]>, String> {
    binary_args(function, "ISNULL")
}

pub fn binary_args<'a>(
    function: &'a Function,
    name: &str,
) -> Result<Option<[&'a Expr; 2]>, String> {
    if !function.name.to_string().eq_ignore_ascii_case(name) {
        return Ok(None);
    }
    let FunctionArguments::List(args) = &function.args else {
        return Err(format!("{name} requires two scalar arguments"));
    };
    if !matches!(function.parameters, FunctionArguments::None)
        || function.over.is_some()
        || function.filter.is_some()
        || function.null_treatment.is_some()
        || !function.within_group.is_empty()
        || args.duplicate_treatment.is_some()
        || !args.clauses.is_empty()
    {
        return Err(format!("unsupported {name} modifiers"));
    }
    match args.args.as_slice() {
        [
            FunctionArg::Unnamed(FunctionArgExpr::Expr(first)),
            FunctionArg::Unnamed(FunctionArgExpr::Expr(replacement)),
        ] => Ok(Some([first, replacement])),
        _ => Err(format!("{name} requires two scalar arguments")),
    }
}

pub fn literal_null(expr: &Expr) -> bool {
    match expr {
        Expr::Nested(inner) => literal_null(inner),
        Expr::Value(value) => matches!(value.value, Value::Null),
        _ => false,
    }
}

pub fn nullif_args(function: &Function) -> Result<Option<[&Expr; 2]>, String> {
    let Some([first, second]) = binary_args(function, "NULLIF")? else {
        return Ok(None);
    };
    if null_constant(first) {
        return Err("The type of the first argument to NULLIF cannot be the NULL constant because the type of the first argument has to be known.".into());
    }
    Ok(Some([first, second]))
}

pub fn iif_args(function: &Function) -> Result<Option<[&Expr; 3]>, String> {
    if !function.name.to_string().eq_ignore_ascii_case("IIF") {
        return Ok(None);
    }
    let FunctionArguments::List(args) = &function.args else {
        return Err("IIF requires three scalar arguments".into());
    };
    if !matches!(function.parameters, FunctionArguments::None)
        || function.over.is_some()
        || function.filter.is_some()
        || function.null_treatment.is_some()
        || !function.within_group.is_empty()
        || args.duplicate_treatment.is_some()
        || !args.clauses.is_empty()
    {
        return Err("unsupported IIF modifiers".into());
    }
    let [
        FunctionArg::Unnamed(FunctionArgExpr::Expr(a)),
        FunctionArg::Unnamed(FunctionArgExpr::Expr(b)),
        FunctionArg::Unnamed(FunctionArgExpr::Expr(c)),
    ] = args.args.as_slice()
    else {
        return Err("IIF requires three scalar arguments".into());
    };
    Ok(Some([a, b, c]))
}

pub fn null_constant(expr: &Expr) -> bool {
    match expr {
        Expr::Nested(expr) => null_constant(expr),
        Expr::Value(value) => matches!(value.value, Value::Null),
        _ => false,
    }
}

pub fn choose_args(function: &Function) -> Result<Option<Vec<&Expr>>, String> {
    if !function.name.to_string().eq_ignore_ascii_case("CHOOSE") {
        return Ok(None);
    }
    let FunctionArguments::List(args) = &function.args else {
        return Err("CHOOSE requires an index and at least two values".into());
    };
    if !matches!(function.parameters, FunctionArguments::None)
        || function.over.is_some()
        || function.filter.is_some()
        || function.null_treatment.is_some()
        || !function.within_group.is_empty()
        || args.duplicate_treatment.is_some()
        || !args.clauses.is_empty()
    {
        return Err("unsupported CHOOSE modifiers".into());
    }
    if args.args.len() < 3 {
        return Err("CHOOSE requires an index and at least two values".into());
    }
    args.args
        .iter()
        .map(|arg| match arg {
            FunctionArg::Unnamed(FunctionArgExpr::Expr(expr)) => Ok(expr),
            _ => Err("CHOOSE requires scalar arguments".into()),
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

pub fn coalesce_args(function: &Function) -> Result<Option<Vec<&Expr>>, String> {
    if !function.name.to_string().eq_ignore_ascii_case("COALESCE") {
        return Ok(None);
    }
    let FunctionArguments::List(args) = &function.args else {
        return Err("COALESCE requires scalar arguments".into());
    };
    if !matches!(function.parameters, FunctionArguments::None)
        || function.over.is_some()
        || function.filter.is_some()
        || function.null_treatment.is_some()
        || !function.within_group.is_empty()
        || args.duplicate_treatment.is_some()
        || !args.clauses.is_empty()
        || args.args.is_empty()
    {
        return Err("unsupported COALESCE arguments or modifiers".into());
    }
    args.args
        .iter()
        .map(|arg| match arg {
            FunctionArg::Unnamed(FunctionArgExpr::Expr(expr)) => Ok(expr),
            _ => Err("COALESCE requires scalar arguments".into()),
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

pub fn candidate(expr: &Expr) -> bool {
    matches!(expr, Expr::Case { .. })
        || matches!(expr, Expr::Function(f) if matches!(f.name.to_string().to_ascii_uppercase().as_str(), "COALESCE" | "IIF" | "CHOOSE" | "NULLIF" | "ISNULL" | "__MSDUCK_ISNULL"))
}

pub fn values(expr: &Expr) -> Vec<&Expr> {
    match expr {
        Expr::Case {
            conditions,
            else_result,
            ..
        } => conditions
            .iter()
            .map(|c| &c.result)
            .chain(else_result.iter().map(|v| v.as_ref()))
            .collect(),
        Expr::Function(f) => {
            if let Ok(Some([first, _])) = nullif_args(f) {
                return vec![first];
            }
            for name in ["ISNULL", "__msduck_isnull"] {
                if let Ok(Some([first, replacement])) = binary_args(f, name) {
                    return vec![if literal_null(first) {
                        replacement
                    } else {
                        first
                    }];
                }
            }
            if let Ok(Some(args)) = coalesce_args(f) {
                return args;
            }
            if let Ok(Some([_, yes, no])) = iif_args(f) {
                return vec![yes, no];
            }
            if let Ok(Some(args)) = choose_args(f) {
                return args.into_iter().skip(1).collect();
            }
            vec![]
        }
        _ => vec![],
    }
}
