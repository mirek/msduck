//! Deterministic expression metadata rules shared by binding and lowering.
use super::conditional;
use sqlparser::ast::*;

pub fn retained_argument(f: &Function) -> Option<&Expr> {
    if matches!(
        f.name.to_string().to_ascii_lowercase().as_str(),
        "min" | "max"
    ) {
        let FunctionArguments::List(args) = &f.args else {
            return None;
        };
        let [FunctionArg::Unnamed(FunctionArgExpr::Expr(value))] = args.args.as_slice() else {
            return None;
        };
        return Some(value);
    }
    window_argument(f)
}

pub fn common_scale<'a>(values: impl Iterator<Item = &'a Expr>) -> Option<u8> {
    let mut result = None;
    for value in values {
        if let Some(scale) = time_scale(value) {
            result = Some(result.map_or(scale, |previous: u8| previous.max(scale)));
        } else if !conditional::literal_null(value) {
            return None;
        }
    }
    result
}

pub fn time_scale(expr: &Expr) -> Option<u8> {
    if conditional::candidate(expr) {
        return conditional::values(expr)
            .into_iter()
            .filter_map(time_scale)
            .max();
    }
    match expr {
        Expr::Nested(value) => time_scale(value),
        Expr::Cast {
            data_type: DataType::Time(s, TimezoneInfo::None),
            ..
        }
        | Expr::Convert {
            data_type: Some(DataType::Time(s, TimezoneInfo::None)),
            ..
        } => s.unwrap_or(7).try_into().ok().filter(|s| *s <= 7),
        Expr::Function(f) => {
            if let Some(scale) = timefromparts_scale(f) {
                return Some(scale);
            }
            if let Some(value) = retained_argument(f) {
                return time_scale(value);
            }
            let name = f.name.to_string().to_ascii_lowercase();
            if name == "dateadd"
                && let FunctionArguments::List(args) = &f.args
                && let Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(value))) = args.args.get(2)
            {
                return time_scale(value);
            }
            if let Some(s) = name.strip_prefix("__msduck_time_dateadd_") {
                return s.parse::<u8>().ok().filter(|s| *s <= 7);
            }
            let [_, quantum] = conditional::binary_args(f, "__msduck_time_round").ok()??;
            let Expr::Value(value) = quantum else {
                return None;
            };
            let Value::Number(value, _) = &value.value else {
                return None;
            };
            let quantum = value.parse::<u64>().ok()?;
            (0..=7).find(|s| 10u64.pow(9 - u32::from(*s)) == quantum)
        }
        _ => None,
    }
}

pub fn window_argument(function: &Function) -> Option<&Expr> {
    if !matches!(
        function.name.to_string().to_ascii_lowercase().as_str(),
        "first_value" | "last_value" | "lag" | "lead"
    ) {
        return None;
    }
    let FunctionArguments::List(args) = &function.args else {
        return None;
    };
    let offset = matches!(
        function.name.to_string().to_ascii_lowercase().as_str(),
        "lag" | "lead"
    );
    if args.args.is_empty() || args.args.len() > if offset { 3 } else { 1 } {
        return None;
    }
    let FunctionArg::Unnamed(FunctionArgExpr::Expr(value)) = &args.args[0] else {
        return None;
    };
    Some(value)
}

pub fn precision_scale(expr: &Expr) -> Option<u8> {
    u8::try_from(integer_constant(expr)?)
        .ok()
        .filter(|s| *s <= 7)
}

pub(crate) fn integer_constant(expr: &Expr) -> Option<i32> {
    match expr {
        Expr::Nested(e) => integer_constant(e),
        Expr::Cast {
            expr,
            data_type: DataType::Int(_) | DataType::Integer(_),
            format: None,
            ..
        } => cast_int_constant(expr),
        Expr::Convert {
            expr,
            data_type: Some(DataType::Int(_) | DataType::Integer(_)),
            charset: None,
            styles,
            ..
        } if styles.is_empty() => cast_int_constant(expr),
        Expr::Value(v) => match &v.value {
            Value::Number(n, false) => n.parse().ok(),
            _ => None,
        },
        Expr::UnaryOp { op, expr } => match op {
            UnaryOperator::Plus => integer_constant(expr),
            UnaryOperator::Minus => integer_constant(expr)?.checked_neg(),
            UnaryOperator::BitwiseNot => Some(!integer_constant(expr)?),
            _ => None,
        },
        Expr::BinaryOp { left, op, right } => {
            let (a, b) = (integer_constant(left)?, integer_constant(right)?);
            match op {
                BinaryOperator::Plus => a.checked_add(b),
                BinaryOperator::Minus => a.checked_sub(b),
                BinaryOperator::Multiply => a.checked_mul(b),
                BinaryOperator::Divide => a.checked_div(b),
                BinaryOperator::Modulo => a.checked_rem(b),
                BinaryOperator::BitwiseAnd => Some(a & b),
                BinaryOperator::BitwiseOr => Some(a | b),
                BinaryOperator::BitwiseXor => Some(a ^ b),
                _ => None,
            }
        }
        _ => None,
    }
}

fn cast_int_constant(expr: &Expr) -> Option<i32> {
    let expr = crate::variant_cast::source(expr).unwrap_or(expr);
    match expr {
        Expr::Nested(value) => cast_int_constant(value),
        Expr::Value(value) => match &value.value {
            Value::Number(number, false) => {
                if number.contains(['e', 'E']) {
                    let value = number.parse::<f64>().ok()?;
                    (value.is_finite()
                        && value >= f64::from(i32::MIN)
                        && value < f64::from(i32::MAX) + 1.0)
                        .then(|| value.trunc() as i32)
                } else if let Some((whole, fraction)) = number.split_once('.') {
                    if !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
                        return None;
                    }
                    if whole.is_empty() {
                        Some(0)
                    } else {
                        whole.parse().ok()
                    }
                } else {
                    number.parse().ok()
                }
            }
            Value::SingleQuotedString(text) | Value::NationalStringLiteral(text) => {
                let text = text.trim();
                if text.is_empty() {
                    Some(0)
                } else {
                    text.parse().ok()
                }
            }
            _ => None,
        },
        Expr::UnaryOp { op, expr } => match op {
            UnaryOperator::Plus => cast_int_constant(expr),
            UnaryOperator::Minus => cast_int_constant(expr)?.checked_neg(),
            _ => None,
        },
        _ => integer_constant(expr),
    }
}

pub fn timefromparts_scale(f: &Function) -> Option<u8> {
    let name = f.name.to_string().to_ascii_lowercase();
    if let Some(s) = name.strip_prefix("__msduck_timefromparts_") {
        return s.parse::<u8>().ok().filter(|s| *s <= 7);
    }
    if name != "timefromparts" {
        return None;
    }
    let FunctionArguments::List(args) = &f.args else {
        return None;
    };
    let FunctionArg::Unnamed(FunctionArgExpr::Expr(value)) = args.args.get(4)? else {
        return None;
    };
    precision_scale(value)
}

pub fn datetime2fromparts_scale(f: &Function) -> Option<u8> {
    let name = f.name.to_string().to_ascii_lowercase();
    if let Some(s) = name.strip_prefix("__msduck_datetime2fromparts_") {
        return s.parse::<u8>().ok().filter(|s| *s <= 7);
    }
    if name != "datetime2fromparts" {
        return None;
    }
    let FunctionArguments::List(args) = &f.args else {
        return None;
    };
    let FunctionArg::Unnamed(FunctionArgExpr::Expr(value)) = args.args.get(7)? else {
        return None;
    };
    precision_scale(value)
}

pub fn datetimeoffsetfromparts_scale(f: &Function) -> Option<u8> {
    let name = f.name.to_string().to_ascii_lowercase();
    if let Some(s) = name.strip_prefix("__msduck_datetimeoffsetfromparts_") {
        return s.parse::<u8>().ok().filter(|s| *s <= 7);
    }
    if name != "datetimeoffsetfromparts" {
        return None;
    }
    let FunctionArguments::List(args) = &f.args else {
        return None;
    };
    let FunctionArg::Unnamed(FunctionArgExpr::Expr(value)) = args.args.get(9)? else {
        return None;
    };
    precision_scale(value)
}

pub fn calendar_argument(function: &Function) -> Result<Option<(&str, &Expr)>, String> {
    for name in ["YEAR", "MONTH", "DAY"] {
        if let Some(value) = crate::function_args::unary(function, name)? {
            return Ok(Some((name, value)));
        }
    }
    Ok(None)
}
