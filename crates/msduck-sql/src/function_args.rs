//! Shared validation for scalar function calls.
use sqlparser::ast::*;

pub fn unary<'a>(function: &'a Function, name: &str) -> Result<Option<&'a Expr>, String> {
    if !function.name.to_string().eq_ignore_ascii_case(name) {
        return Ok(None);
    }
    let FunctionArguments::List(args) = &function.args else {
        return Err(format!("{name} requires one scalar argument"));
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
        [FunctionArg::Unnamed(FunctionArgExpr::Expr(value))] => Ok(Some(value)),
        _ => Err(format!("{name} requires one scalar argument")),
    }
}
