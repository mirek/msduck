//! SQL Server ordered-set percentile windows.
use sqlparser::ast::*;

pub fn discrete_value(function: &Function) -> Option<&Expr> {
    if function
        .name
        .to_string()
        .eq_ignore_ascii_case("quantile_disc")
        && let FunctionArguments::List(args) = &function.args
        && let Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(value))) = args.args.first()
    {
        return Some(value);
    }

    if function
        .name
        .to_string()
        .eq_ignore_ascii_case("percentile_disc")
        && function.within_group.len() == 1
    {
        Some(&function.within_group[0].expr)
    } else {
        None
    }
}

pub fn lower(expr: &mut Expr) -> Result<(), String> {
    let Expr::Function(function) = expr else {
        return Ok(());
    };
    let name = function.name.to_string().to_ascii_lowercase();
    if !matches!(name.as_str(), "percentile_cont" | "percentile_disc") {
        return Ok(());
    }
    let FunctionArguments::List(args) = &function.args else {
        return Err(format!("The {name} function requires 1 argument(s)."));
    };
    let [FunctionArg::Unnamed(FunctionArgExpr::Expr(fraction))] = args.args.as_slice() else {
        return Err(format!("The {name} function requires 1 argument(s)."));
    };
    if args.duplicate_treatment.is_some()
        || !args.clauses.is_empty()
        || !matches!(function.parameters, FunctionArguments::None)
        || function.filter.is_some()
        || function.null_treatment.is_some()
    {
        return Err("unsupported percentile modifiers".into());
    }
    let Expr::Value(literal) = fraction else {
        return Err("Percentile requires a numeric literal between 0 and 1.".into());
    };
    let Value::Number(number, _) = &literal.value else {
        return Err("Percentile requires a numeric literal between 0 and 1.".into());
    };
    let zero =
        fraction_is_zero(number).ok_or("Percentile requires a numeric literal between 0 and 1.")?;
    if function.within_group.is_empty() {
        return Err(format!(
            "The function '{name}' must have a WITHIN GROUP clause."
        ));
    }
    if function.within_group.len() != 1 {
        return Err("Percentile WITHIN GROUP requires exactly one ORDER BY expression.".into());
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
        if !spec.order_by.is_empty() {
            return Err(format!(
                "The function '{name}' may not have ORDER BY in its OVER clause."
            ));
        }
    }
    let order = &function.within_group[0];
    let descending = order.options.sort == Some(OrderBySort::Desc);
    let mut value = order.expr.clone();
    if name == "percentile_cont" {
        value = crate::expr::unary_function("__msduck_percentile_input", value);
    }
    let mut fraction = fraction.clone();
    if descending && !zero {
        fraction = Expr::UnaryOp {
            op: UnaryOperator::Minus,
            expr: Box::new(fraction),
        };
    }
    // Negative quantiles reverse DuckDB's ordering, except negative zero.
    let target = if descending && zero {
        "max"
    } else if name == "percentile_cont" {
        "quantile_cont"
    } else {
        "quantile_disc"
    };
    function.name = ObjectName::from(vec![Ident::new(target)]);
    function.within_group.clear();
    if let FunctionArguments::List(args) = &mut function.args {
        args.args = vec![FunctionArg::Unnamed(FunctionArgExpr::Expr(value))];
        if target != "max" {
            args.args
                .push(FunctionArg::Unnamed(FunctionArgExpr::Expr(fraction)));
        }
    }
    Ok(())
}

// Compare the decimal literal exactly: rounding to f64 here would admit
// 1.00000000000000000001 and misclassify tiny positive fractions as zero.
fn fraction_is_zero(number: &str) -> Option<bool> {
    let (mantissa, exponent) = number.split_once(['e', 'E']).unwrap_or((number, "0"));
    let exponent = exponent.parse::<i64>().unwrap_or_else(|_| {
        if exponent.starts_with('-') {
            i64::MIN
        } else {
            i64::MAX
        }
    });
    let whole = mantissa.split('.').next()?.len() as i64;
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let significant = digits.trim_start_matches('0');
    if significant.is_empty() {
        return Some(true);
    }
    let position = whole
        .saturating_sub((digits.len() - significant.len()) as i64)
        .saturating_add(exponent);
    (position <= 0
        || (position == 1
            && significant.starts_with('1')
            && significant[1..].bytes().all(|b| b == b'0')))
    .then_some(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fraction_classification_retains_extreme_decimal_exponents() {
        for number in [
            "0",
            "0.0000",
            "0e999999999999999999999",
            "000.0e-999999999999999999999",
        ] {
            assert_eq!(fraction_is_zero(number), Some(true), "{number}");
        }
        for number in [
            "1",
            "1.00000000000000000000",
            "0.00000000000000000001",
            "1e-999999999999999999999",
        ] {
            assert_eq!(fraction_is_zero(number), Some(false), "{number}");
        }
        for number in [
            "1.00000000000000000001",
            "1e999999999999999999999",
            "2",
            "99.99",
            "bad",
        ] {
            assert_eq!(fraction_is_zero(number), None, "{number}");
        }
    }
}
