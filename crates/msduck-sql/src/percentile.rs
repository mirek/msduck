//! SQL Server ordered-set percentile windows.
use sqlparser::ast::*;

pub const RANGE: &str = "Input parameter of percentile function is outside of range [0, 1].";
const LITERAL: &str = "Percentile requires a numeric literal between 0 and 1.";

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
    let (zero, mut fraction) = normalized_fraction(fraction)?;
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

// Only explicit, caller-owned constants are folded. Dynamic inputs remain unknown.
fn character_literal(expr: &Expr) -> Option<(String, bool)> {
    use msduck_core::character::{CastInput, CharacterType, Family, Length};
    match expr {
        Expr::Value(value) => match &value.value {
            Value::SingleQuotedString(text) => Some((text.clone(), false)),
            Value::NationalStringLiteral(text) => Some((text.clone(), true)),
            _ => None,
        },
        Expr::Nested(value)
        | Expr::UnaryOp {
            op: UnaryOperator::Plus,
            expr: value,
        } => character_literal(value),
        Expr::Cast {
            kind: CastKind::Cast,
            expr: value,
            data_type,
            format: None,
        } => {
            let (unicode, width) = match data_type {
                DataType::Varchar(width) => (false, width),
                DataType::Nvarchar(width) => (true, width),
                _ => return None,
            };
            let length = match width {
                Some(CharacterLength::Max) => Length::Max,
                Some(CharacterLength::IntegerLength { length, unit: None }) => {
                    Length::Bounded((*length).try_into().ok()?)
                }
                None => Length::Bounded(30),
                _ => return None,
            };
            let (text, _) = character_literal(value)?;
            let target = CharacterType::new(
                if unicode {
                    Family::Nvarchar
                } else {
                    Family::Varchar
                },
                length,
            )
            .ok()?;
            Some((
                target.cast(&text, CastInput::Text).ok()?.into_owned(),
                unicode,
            ))
        }
        // Root character lowering can already have converted an explicit CAST
        // to this pure adapter call before percentile lowering sees its parent.
        Expr::Function(function)
            if matches!(
                function.name.to_string().as_str(),
                "__msduck_cast_varchar" | "__msduck_cast_nvarchar"
            ) && matches!(function.parameters, FunctionArguments::None)
                && function.filter.is_none()
                && function.over.is_none()
                && function.within_group.is_empty()
                && function.null_treatment.is_none() =>
        {
            let FunctionArguments::List(args) = &function.args else {
                return None;
            };
            if args.duplicate_treatment.is_some() || !args.clauses.is_empty() {
                return None;
            }
            let [
                FunctionArg::Unnamed(FunctionArgExpr::Expr(value)),
                FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(width))),
            ] = args.args.as_slice()
            else {
                return None;
            };
            let Value::Number(width, _) = &width.value else {
                return None;
            };
            let width = width.parse::<i64>().ok()?;
            let length = if width == -1 {
                Length::Max
            } else {
                Length::Bounded(width.try_into().ok()?)
            };
            let unicode = function.name.to_string() == "__msduck_cast_nvarchar";
            let (text, _) = character_literal(value)?;
            let target = CharacterType::new(
                if unicode {
                    Family::Nvarchar
                } else {
                    Family::Varchar
                },
                length,
            )
            .ok()?;
            Some((
                target.cast(&text, CastInput::Text).ok()?.into_owned(),
                unicode,
            ))
        }
        _ => None,
    }
}

fn normalized_fraction(fraction: &Expr) -> Result<(bool, Expr), String> {
    if let Some((text, unicode)) = character_literal(fraction) {
        let number = character_fraction(&text, unicode)?;
        return Ok((number == 0.0, crate::expr::number(number)));
    }
    let zero = match fraction {
        Expr::Value(literal) => match &literal.value {
            Value::Number(number, _) => fraction_is_zero(number).ok_or(RANGE)?,
            Value::Null => return Err(RANGE.into()),
            _ => return Err(LITERAL.into()),
        },
        Expr::UnaryOp {
            op: UnaryOperator::Minus,
            expr,
        } => {
            let Expr::Value(literal) = expr.as_ref() else {
                return Err(LITERAL.into());
            };
            let Value::Number(number, _) = &literal.value else {
                return Err(LITERAL.into());
            };
            if fraction_is_zero(number) != Some(true) {
                return Err(RANGE.into());
            }
            true
        }
        _ => return Err(LITERAL.into()),
    };
    Ok((zero, fraction.clone()))
}

fn character_fraction(text: &str, unicode: bool) -> Result<f64, String> {
    let invalid = || {
        format!(
            "Error converting data type {} to float.",
            if unicode { "nvarchar" } else { "varchar" }
        )
    };
    let text = text
        .split('\0')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ');
    // Empty / ASCII-space-only text is zero, but whitespace-only control or
    // Unicode characters are invalid. Leading whitespace includes U+180E,
    // which the current Unicode Rust predicate no longer classifies as space.
    if text.is_empty() {
        return Ok(0.0);
    }
    let text = text.trim_start_matches(|c: char| c.is_whitespace() || c == '\u{180e}');
    let bytes = text.as_bytes();
    let mut at = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
    let start = at;
    while bytes.get(at).is_some_and(u8::is_ascii_digit) {
        at += 1
    }
    let mut digits = at - start;
    if bytes.get(at) == Some(&b'.') {
        at += 1;
        let start = at;
        while bytes.get(at).is_some_and(u8::is_ascii_digit) {
            at += 1
        }
        digits += at - start;
    }
    if digits == 0 {
        return Err(invalid());
    }
    if matches!(bytes.get(at), Some(b'e' | b'E' | b'd' | b'D')) {
        at += 1;
        if matches!(bytes.get(at), Some(b'+' | b'-')) {
            at += 1
        }
        let start = at;
        while bytes.get(at).is_some_and(u8::is_ascii_digit) {
            at += 1
        }
        if at == start {
            return Err(invalid());
        }
    }
    if at != bytes.len() {
        return Err(invalid());
    }
    let number = text
        .replace(['d', 'D'], "e")
        .parse::<f64>()
        .map_err(|_| invalid())?;
    if !number.is_finite() {
        return Err("Arithmetic overflow error converting expression to data type float.".into());
    }
    if !(0.0..=1.0).contains(&number) {
        return Err(RANGE.into());
    }
    Ok(number)
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
    use crate::dialect::ServerDialect;
    use sqlparser::parser::Parser;

    fn expression(sql: &str) -> Expr {
        let Statement::Query(query) = Parser::parse_sql(&ServerDialect, sql).unwrap().remove(0)
        else {
            panic!("query")
        };
        let SetExpr::Select(select) = *query.body else {
            panic!("select")
        };
        let SelectItem::UnnamedExpr(expr) = select.projection.into_iter().next().unwrap() else {
            panic!("expression")
        };
        expr
    }

    #[test]
    fn captured_invalid_fractions_keep_error_identity_and_ast() {
        for function in ["PERCENTILE_CONT", "PERCENTILE_DISC"] {
            for fraction in ["-0.1", "1.1", "NULL", "1.00000000000000000001"] {
                let sql =
                    format!("SELECT {function}({fraction}) WITHIN GROUP (ORDER BY n) OVER ()");
                let mut expr = expression(&sql);
                let original = expr.clone();
                assert_eq!(lower(&mut expr), Err(RANGE.into()), "{sql}");
                assert_eq!(expr, original, "invalid lowering changed the AST: {sql}");
                assert_eq!(crate::ranking::error_number(RANGE), Some(8727));
            }
        }
    }

    #[test]
    fn valid_and_unproven_fraction_forms_remain_distinct() {
        for function in ["PERCENTILE_CONT", "PERCENTILE_DISC"] {
            for fraction in ["0", ".5", "1", "-0", "-0.0", "-0e1"] {
                let sql =
                    format!("SELECT {function}({fraction}) WITHIN GROUP (ORDER BY n) OVER ()");
                let mut expr = expression(&sql);
                lower(&mut expr).unwrap();
                let Expr::Function(lowered) = expr else {
                    panic!("lowered function")
                };
                assert!(
                    lowered.name.to_string().starts_with("quantile_")
                        || lowered.name.to_string() == "max",
                    "{sql}"
                );
            }
            for fraction in ["@p", "n", "RAND()"] {
                let sql =
                    format!("SELECT {function}({fraction}) WITHIN GROUP (ORDER BY n) OVER ()");
                let mut expr = expression(&sql);
                assert_eq!(lower(&mut expr), Err(LITERAL.into()), "{sql}");
            }
        }
    }

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
