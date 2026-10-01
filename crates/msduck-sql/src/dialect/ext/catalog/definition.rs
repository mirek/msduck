//! SQL Server's catalog text for CHECK, DEFAULT and computed column
//! expressions (`sys.check_constraints.definition`,
//! `sys.default_constraints.definition`, `sys.computed_columns.definition`
//! and `OBJECT_DEFINITION`).
//!
//! SQL Server does not keep the expression as written: it stores a
//! normalized form, which this module derives from the parsed T-SQL
//! expression. The rules below follow reference/gaps-catalog.json:
//!
//! - the whole expression is wrapped in parentheses;
//! - columns are bracketed, numeric constants parenthesized (`(1)`,
//!   `(-1.5)`, `(1.0000000000000000e+002)`, `(2147483648.)`), strings,
//!   binary constants and NULL are not;
//! - additive and bitwise operators (`+ - & | ^`) and unary minus are
//!   parenthesized whenever they are an operand; multiplicative operators
//!   (`* / %`) only inside another multiplicative operator or a unary one;
//! - AND/OR chains are flattened on the left, an OR inside an AND and a
//!   right operand of the same operator keep their parentheses;
//! - `IN` becomes an OR chain of equalities in reverse order, `BETWEEN` a
//!   pair of comparisons, `!=` becomes `<>`;
//! - CAST and CONVERT become `CONVERT([type],value[,(style)])`, TRY_CONVERT
//!   becomes `TRY_CAST(value AS [type])`, `IIF` a searched CASE, `YEAR`,
//!   `MONTH` and `DAY` become `datepart`, CURRENT_TIMESTAMP `getdate()`;
//! - function names and CASE, LIKE, ESCAPE and COLLATE keywords are lower
//!   case; AND, OR, NOT and IS [NOT] NULL upper case.
//!
//! Expressions outside these rules have no known catalog text: the result
//! is `None` rather than an approximation.
use sqlparser::ast::*;

/// The catalog definition of `expr`, or `None` when its normalized form is
/// not known.
pub fn definition(expr: &Expr) -> Option<String> {
    let mut budget = Budget {
        nodes: 4096,
        bytes: 1 << 20,
    };
    let mut expr = expr.clone();
    let _ = VisitMut::visit(&mut expr, &mut Associate);
    let (text, _) = render(&expr, &mut budget, 0)?;
    Some(format!("({text})"))
}

/// T-SQL gives `+ - & | ^` one precedence, applied left to right, while
/// the parser binds `^` and `+` tighter than `|` and `&`. An unparenthesized
/// right operand of one of these operators is therefore reassociated to
/// the left, as SQL Server reads it: `a | b ^ c` is `(a | b) ^ c`.
struct Associate;

fn additive(op: &BinaryOperator) -> bool {
    matches!(arithmetic(op), Some((_, Class::Additive)))
}

impl VisitorMut for Associate {
    type Break = ();
    fn post_visit_expr(&mut self, expr: &mut Expr) -> std::ops::ControlFlow<()> {
        loop {
            let Expr::BinaryOp { op, right, .. } = expr else {
                break;
            };
            if !additive(op) {
                break;
            }
            let Expr::BinaryOp { op: inner, .. } = right.as_ref() else {
                break;
            };
            if !additive(inner) {
                break;
            }
            let Expr::BinaryOp { left, op, right } =
                std::mem::replace(expr, Expr::value(Value::Null))
            else {
                unreachable!()
            };
            let Expr::BinaryOp {
                left: middle,
                op: inner,
                right: last,
            } = *right
            else {
                unreachable!()
            };
            *expr = Expr::BinaryOp {
                left: Box::new(Expr::BinaryOp {
                    left,
                    op,
                    right: middle,
                }),
                op: inner,
                right: last,
            };
        }
        std::ops::ControlFlow::Continue(())
    }
}

/// The catalog definition of an expression stored as T-SQL text.
pub fn source_definition(source: &str) -> Option<String> {
    definition(&parse_expression(source)?)
}

/// Parse a scalar expression written in T-SQL.
pub fn parse_expression(source: &str) -> Option<Expr> {
    if source.len() > 1 << 20 {
        return None;
    }
    // Parenthesized, so that `a = b` is not read as a column alias.
    let mut statements = crate::batch::parse(&format!("SELECT ({source})")).ok()?;
    if statements.len() != 1 {
        return None;
    }
    let Statement::Query(query) = statements.remove(0) else {
        return None;
    };
    let SetExpr::Select(select) = *query.body else {
        return None;
    };
    let mut select = *select;
    if select.projection.len() != 1 || select.from.len() != 0 {
        return None;
    }
    match select.projection.remove(0) {
        SelectItem::UnnamedExpr(Expr::Nested(expr)) => Some(*expr),
        _ => None,
    }
}

struct Budget {
    nodes: usize,
    bytes: usize,
}

/// How a rendered expression combines with its parent.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Class {
    /// Columns, constants, function calls, CASE, CONVERT.
    Atom,
    /// `+ - & | ^` and unary minus.
    Additive,
    /// `* / %`.
    Multiplicative,
    /// Comparisons, LIKE, IS [NOT] NULL.
    Predicate,
    And,
    Or,
    Not,
}

fn unnest(mut expr: &Expr) -> &Expr {
    loop {
        match expr {
            Expr::Nested(inner) => expr = inner,
            _ => match crate::variant_cast::source(expr) {
                Some(source) => expr = source,
                None => return expr,
            },
        }
    }
}

const INT_MAX: u128 = i32::MAX as u128;

/// A numeric constant as SQL Server prints it, without parentheses.
fn number(source: &str, negative: bool) -> Option<String> {
    if source.is_empty() || source.len() > 80 {
        return None;
    }
    let sign = |zero: bool| if negative && !zero { "-" } else { "" };
    if source.contains(['e', 'E']) {
        let value = source.parse::<f64>().ok()?;
        if !value.is_finite() {
            return None;
        }
        let value = if negative { -value } else { value };
        let scientific = format!("{value:.16e}");
        let (mantissa, exponent) = scientific.split_once('e')?;
        let exponent = exponent.parse::<i32>().ok()?;
        let exponent = if exponent < 0 {
            format!("-{:03}", -exponent)
        } else {
            format!("+{exponent:03}")
        };
        return Some(format!("{mantissa}e{exponent}"));
    }
    if let Some((integer, fraction)) = source.split_once('.') {
        if !integer.bytes().all(|b| b.is_ascii_digit())
            || !fraction.bytes().all(|b| b.is_ascii_digit())
            || integer.len() + fraction.len() > 38
        {
            return None;
        }
        let integer = integer.trim_start_matches('0');
        let integer = if integer.is_empty() { "0" } else { integer };
        let zero = integer == "0" && fraction.bytes().all(|b| b == b'0');
        return Some(format!("{}{integer}.{fraction}", sign(zero)));
    }
    if !source.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let digits = source.trim_start_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    if digits.len() > 38 {
        return None;
    }
    let value: u128 = digits.parse().ok()?;
    let zero = value == 0;
    // A constant beyond int is numeric, printed with a trailing point.
    let limit = if negative { INT_MAX + 1 } else { INT_MAX };
    if value > limit {
        Some(format!("{}{digits}.", sign(zero)))
    } else {
        Some(format!("{}{digits}", sign(zero)))
    }
}

fn quote(text: &str) -> String {
    text.replace('\'', "''")
}

fn bracket(name: &str) -> String {
    format!("[{}]", name.replace(']', "]]"))
}

/// The bracketed type of CONVERT: `[int]`, `[varchar](10)`, `[decimal](5,2)`.
fn type_name(kind: &DataType) -> Option<String> {
    let text = kind.to_string().to_ascii_lowercase();
    let (name, arguments) = match text.split_once('(') {
        Some((name, rest)) => (name.trim(), Some(rest.strip_suffix(')')?.replace(' ', ""))),
        None => (text.trim(), None),
    };
    let name = match name {
        "integer" => "int",
        "dec" => "decimal",
        "double precision" | "double" => "float",
        "character" => "char",
        "character varying" | "char varying" => "varchar",
        "national character varying" | "national char varying" => "nvarchar",
        "national character" | "national char" => "nchar",
        "rowversion" => "timestamp",
        other => other,
    };
    if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        return None;
    }
    Some(match arguments {
        Some(arguments) => format!("[{name}]({arguments})"),
        None => format!("[{name}]"),
    })
}

/// Full datepart names, as SQL Server prints the first argument of the
/// date functions.
fn datepart(name: &str) -> Option<&'static str> {
    Some(match name.to_ascii_lowercase().as_str() {
        "year" | "yy" | "yyyy" => "year",
        "quarter" | "qq" | "q" => "quarter",
        "month" | "mm" | "m" => "month",
        "dayofyear" | "dy" | "y" => "dayofyear",
        "day" | "dd" | "d" => "day",
        "week" | "wk" | "ww" => "week",
        "weekday" | "dw" | "w" => "weekday",
        "hour" | "hh" => "hour",
        "minute" | "mi" | "n" => "minute",
        "second" | "ss" | "s" => "second",
        "millisecond" | "ms" => "millisecond",
        "microsecond" | "mcs" => "microsecond",
        "nanosecond" | "ns" => "nanosecond",
        "tzoffset" | "tz" => "tzoffset",
        "iso_week" | "isowk" | "isoww" => "iso_week",
        _ => return None,
    })
}

const DATE_FUNCTIONS: &[&str] = &[
    "dateadd",
    "datediff",
    "datediff_big",
    "datename",
    "datepart",
    "datetrunc",
    "date_bucket",
];

fn wrap(text: String) -> String {
    format!("({text})")
}

fn comparison(op: &BinaryOperator) -> Option<&'static str> {
    Some(match op {
        BinaryOperator::Eq => "=",
        BinaryOperator::NotEq => "<>",
        BinaryOperator::Lt => "<",
        BinaryOperator::LtEq => "<=",
        BinaryOperator::Gt => ">",
        BinaryOperator::GtEq => ">=",
        _ => return None,
    })
}

fn arithmetic(op: &BinaryOperator) -> Option<(&'static str, Class)> {
    Some(match op {
        BinaryOperator::Plus => ("+", Class::Additive),
        BinaryOperator::Minus => ("-", Class::Additive),
        BinaryOperator::BitwiseAnd => ("&", Class::Additive),
        BinaryOperator::BitwiseOr => ("|", Class::Additive),
        BinaryOperator::BitwiseXor | BinaryOperator::PGExp => ("^", Class::Additive),
        BinaryOperator::Multiply => ("*", Class::Multiplicative),
        BinaryOperator::Divide => ("/", Class::Multiplicative),
        BinaryOperator::Modulo => ("%", Class::Multiplicative),
        _ => return None,
    })
}

/// A comparison operand: additive expressions are parenthesized.
fn operand(expr: &Expr, budget: &mut Budget, depth: usize) -> Option<String> {
    let (text, class) = render(expr, budget, depth)?;
    Some(match class {
        Class::Atom | Class::Multiplicative => text,
        Class::Additive => wrap(text),
        // A predicate as a value is not a scalar expression.
        _ => return None,
    })
}

/// A boolean operand of AND, OR or NOT.
fn condition(expr: &Expr, budget: &mut Budget, depth: usize) -> Option<(String, Class)> {
    let (text, class) = render(expr, budget, depth)?;
    match class {
        Class::Predicate | Class::And | Class::Or | Class::Not => Some((text, class)),
        _ => None,
    }
}

fn equality(left: &str, right: &Expr, budget: &mut Budget, depth: usize) -> Option<String> {
    Some(format!("{left}={}", operand(right, budget, depth)?))
}

/// `(c1) OR (c2) OR ...` from items rendered in order.
fn or_chain(items: Vec<String>) -> String {
    items.join(" OR ")
}

fn render(expr: &Expr, budget: &mut Budget, depth: usize) -> Option<(String, Class)> {
    if depth >= 128 || budget.nodes == 0 {
        return None;
    }
    budget.nodes -= 1;
    let depth = depth + 1;
    let expr = unnest(expr);
    let result = match expr {
        Expr::Value(value) => (
            match &value.value {
                Value::Number(text, false) => wrap(number(text, false)?),
                Value::SingleQuotedString(text) => format!("'{}'", quote(text)),
                Value::NationalStringLiteral(text) => format!("N'{}'", quote(text)),
                Value::HexStringLiteral(text) => format!("0x{text}"),
                Value::Null => "NULL".into(),
                _ => return None,
            },
            Class::Atom,
        ),
        Expr::Identifier(ident) => {
            if ident.value.starts_with('@') {
                return None;
            }
            (bracket(&ident.value), Class::Atom)
        }
        Expr::UnaryOp { op, expr: inner } => {
            let inner_expr = unnest(inner);
            match op {
                UnaryOperator::Plus => return render(inner_expr, budget, depth),
                UnaryOperator::Minus => {
                    if let Expr::Value(value) = inner_expr
                        && let Value::Number(text, false) = &value.value
                    {
                        (wrap(number(text, true)?), Class::Atom)
                    } else {
                        let (text, class) = render(inner_expr, budget, depth)?;
                        let text = match class {
                            Class::Atom => text,
                            Class::Additive | Class::Multiplicative => wrap(text),
                            _ => return None,
                        };
                        (format!(" -{text}"), Class::Additive)
                    }
                }
                UnaryOperator::BitwiseNot => {
                    let (text, class) = render(inner_expr, budget, depth)?;
                    let text = match class {
                        Class::Atom => text,
                        Class::Additive | Class::Multiplicative => wrap(text),
                        _ => return None,
                    };
                    (format!("~{text}"), Class::Atom)
                }
                UnaryOperator::Not => {
                    let (text, class) = condition(inner_expr, budget, depth)?;
                    let text = match class {
                        Class::And | Class::Or => wrap(text),
                        _ => text,
                    };
                    (format!("NOT {text}"), Class::Not)
                }
                _ => return None,
            }
        }
        Expr::BinaryOp { left, op, right } => {
            if let Some(symbol) = comparison(op) {
                (
                    format!(
                        "{}{symbol}{}",
                        operand(left, budget, depth)?,
                        operand(right, budget, depth)?
                    ),
                    Class::Predicate,
                )
            } else if let Some((symbol, class)) = arithmetic(op) {
                let side = |expr: &Expr, budget: &mut Budget| -> Option<String> {
                    let (text, child) = render(expr, budget, depth)?;
                    Some(match child {
                        Class::Atom => text,
                        Class::Additive => wrap(text),
                        Class::Multiplicative if class == Class::Multiplicative => wrap(text),
                        Class::Multiplicative => text,
                        _ => return None,
                    })
                };
                (
                    format!("{}{symbol}{}", side(left, budget)?, side(right, budget)?),
                    class,
                )
            } else {
                let (symbol, class) = match op {
                    BinaryOperator::And => ("AND", Class::And),
                    BinaryOperator::Or => ("OR", Class::Or),
                    _ => return None,
                };
                let (left_text, left_class) = condition(left, budget, depth)?;
                let (right_text, right_class) = condition(right, budget, depth)?;
                let left_text = if class == Class::And && left_class == Class::Or {
                    wrap(left_text)
                } else {
                    left_text
                };
                let right_text =
                    if right_class == class || (class == Class::And && right_class == Class::Or) {
                        wrap(right_text)
                    } else {
                        right_text
                    };
                (format!("{left_text} {symbol} {right_text}"), class)
            }
        }
        Expr::IsNull(inner) => (
            format!("{} IS NULL", operand(inner, budget, depth)?),
            Class::Predicate,
        ),
        Expr::IsNotNull(inner) => (
            format!("{} IS NOT NULL", operand(inner, budget, depth)?),
            Class::Predicate,
        ),
        Expr::Like {
            negated,
            any: false,
            expr: inner,
            pattern,
            escape_char,
        } => {
            let mut text = format!(
                "{} like {}",
                operand(inner, budget, depth)?,
                operand(pattern, budget, depth)?
            );
            if let Some(escape) = escape_char {
                text.push_str(&format!(" escape {} ", operand(escape, budget, depth)?));
            }
            if *negated {
                (format!("NOT {text}"), Class::Not)
            } else {
                (text, Class::Predicate)
            }
        }
        Expr::InList {
            expr: inner,
            list,
            negated,
        } => {
            if list.is_empty() {
                return None;
            }
            let left = operand(inner, budget, depth)?;
            let items = list
                .iter()
                .rev()
                .map(|item| equality(&left, item, budget, depth))
                .collect::<Option<Vec<_>>>()?;
            let single = items.len() == 1;
            let chain = or_chain(items);
            match (negated, single) {
                (false, true) => (chain, Class::Predicate),
                (false, false) => (chain, Class::Or),
                (true, true) => (format!("NOT {chain}"), Class::Not),
                (true, false) => (format!("NOT ({chain})"), Class::Not),
            }
        }
        Expr::Between {
            expr: inner,
            negated,
            low,
            high,
        } => {
            let value = operand(inner, budget, depth)?;
            let text = format!(
                "{value}>={} AND {value}<={}",
                operand(low, budget, depth)?,
                operand(high, budget, depth)?
            );
            if *negated {
                (format!("NOT ({text})"), Class::Not)
            } else {
                (text, Class::And)
            }
        }
        Expr::Cast {
            kind,
            expr: inner,
            data_type,
            format: None,
        } => match kind {
            CastKind::Cast => (
                format!(
                    "CONVERT({},{})",
                    type_name(data_type)?,
                    value(inner, budget, depth)?
                ),
                Class::Atom,
            ),
            CastKind::TryCast | CastKind::SafeCast => (
                format!(
                    "TRY_CAST({} AS {})",
                    value(inner, budget, depth)?,
                    type_name(data_type)?
                ),
                Class::Atom,
            ),
            _ => return None,
        },
        Expr::Convert {
            is_try,
            expr: inner,
            data_type: Some(data_type),
            charset: None,
            styles,
            ..
        } => {
            if *is_try {
                if !styles.is_empty() {
                    return None;
                }
                (
                    format!(
                        "TRY_CAST({} AS {})",
                        value(inner, budget, depth)?,
                        type_name(data_type)?
                    ),
                    Class::Atom,
                )
            } else {
                let mut text = format!(
                    "CONVERT({},{}",
                    type_name(data_type)?,
                    value(inner, budget, depth)?
                );
                for style in styles {
                    text.push(',');
                    text.push_str(&value(style, budget, depth)?);
                }
                text.push(')');
                (text, Class::Atom)
            }
        }
        Expr::Case {
            operand: case_operand,
            conditions,
            else_result,
            ..
        } => {
            let mut text = String::from("case");
            if let Some(case_operand) = case_operand {
                text.push(' ');
                text.push_str(&value(case_operand, budget, depth)?);
            }
            for when in conditions {
                let condition_text = if case_operand.is_some() {
                    value(&when.condition, budget, depth)?
                } else {
                    condition(&when.condition, budget, depth)?.0
                };
                text.push_str(&format!(
                    " when {condition_text} then {}",
                    value(&when.result, budget, depth)?
                ));
            }
            // SQL Server keeps the separator of an absent ELSE: `then (2)  end`.
            text.push(' ');
            if let Some(else_result) = else_result {
                text.push_str(&format!("else {}", value(else_result, budget, depth)?));
            }
            text.push_str(" end");
            (text, Class::Atom)
        }
        Expr::Collate {
            expr: inner,
            collation,
        } => {
            let [part] = collation.0.as_slice() else {
                return None;
            };
            let name = &part.as_ident()?.value;
            (
                format!("({}) collate {name}", value(inner, budget, depth)?),
                Class::Atom,
            )
        }
        Expr::Function(function) => (function_call(function, budget, depth)?, Class::Atom),
        Expr::Substring {
            expr: inner,
            substring_from: Some(from),
            substring_for: Some(length),
            ..
        } => (
            format!(
                "substring({},{},{})",
                value(inner, budget, depth)?,
                value(from, budget, depth)?,
                value(length, budget, depth)?
            ),
            Class::Atom,
        ),
        _ => return None,
    };
    budget.bytes = budget.bytes.checked_sub(result.0.len())?;
    Some(result)
}

/// A value in a top-level position (function argument, CASE branch,
/// CONVERT operand): never parenthesized.
fn value(expr: &Expr, budget: &mut Budget, depth: usize) -> Option<String> {
    let (text, class) = render(expr, budget, depth)?;
    match class {
        Class::Atom | Class::Additive | Class::Multiplicative => Some(text),
        _ => None,
    }
}

fn function_call(function: &Function, budget: &mut Budget, depth: usize) -> Option<String> {
    if function.over.is_some()
        || function.filter.is_some()
        || function.null_treatment.is_some()
        || !function.within_group.is_empty()
        || !matches!(function.parameters, FunctionArguments::None)
    {
        return None;
    }
    let parts = function
        .name
        .0
        .iter()
        .map(|part| part.as_ident().map(|ident| ident.value.clone()))
        .collect::<Option<Vec<_>>>()?;
    let arguments: Vec<&Expr> = match &function.args {
        FunctionArguments::None => {
            // CURRENT_TIMESTAMP and the other niladic functions.
            let [name] = parts.as_slice() else {
                return None;
            };
            return match name.to_ascii_lowercase().as_str() {
                "current_timestamp" => Some("getdate()".into()),
                "current_user" | "session_user" | "user" => Some("user_name()".into()),
                "system_user" => Some("suser_sname()".into()),
                _ => None,
            };
        }
        FunctionArguments::List(list) => {
            if list.duplicate_treatment.is_some() || !list.clauses.is_empty() {
                return None;
            }
            list.args
                .iter()
                .map(|argument| match argument {
                    FunctionArg::Unnamed(FunctionArgExpr::Expr(expr)) => Some(expr),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()?
        }
        FunctionArguments::Subquery(_) => return None,
    };
    if parts.len() > 1 {
        // A user-defined function: schema-qualified and bracketed.
        let name = parts
            .iter()
            .map(|part| bracket(part))
            .collect::<Vec<_>>()
            .join(".");
        let values = arguments
            .iter()
            .map(|argument| value(argument, budget, depth))
            .collect::<Option<Vec<_>>>()?;
        return Some(format!("{name}({})", values.join(",")));
    }
    let name = parts[0].to_ascii_lowercase();
    // Backend functions other features generate have no T-SQL text.
    if name.starts_with("__msduck") || matches!(name.as_str(), "nextval" | "getvariable") {
        return None;
    }
    match name.as_str() {
        "year" | "month" | "day" if arguments.len() == 1 => {
            return Some(format!(
                "datepart({name},{})",
                value(arguments[0], budget, depth)?
            ));
        }
        "iif" if arguments.len() == 3 => {
            let (test, _) = condition(arguments[0], budget, depth)?;
            return Some(format!(
                "case when {test} then {} else {} end",
                value(arguments[1], budget, depth)?,
                value(arguments[2], budget, depth)?
            ));
        }
        "current_timestamp" if arguments.is_empty() => return Some("getdate()".into()),
        _ => {}
    }
    let mut values = Vec::with_capacity(arguments.len());
    for (index, argument) in arguments.iter().enumerate() {
        if index == 0 && DATE_FUNCTIONS.contains(&name.as_str()) {
            let Expr::Identifier(part) = unnest(argument) else {
                return None;
            };
            values.push(datepart(&part.value)?.to_string());
            continue;
        }
        values.push(value(argument, budget, depth)?);
    }
    if !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        return None;
    }
    Some(format!("{name}({})", values.join(",")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(source: &str) -> Option<String> {
        source_definition(source)
    }

    #[test]
    fn constants_follow_sql_server_normalization() {
        for (source, expected) in [
            ("1", "((1))"),
            ("-1", "((-1))"),
            ("0001", "((1))"),
            ("1.25", "((1.25))"),
            ("1e2", "((1.0000000000000000e+002))"),
            ("'a''b'", "('a''b')"),
            ("N'a''b'", "(N'a''b')"),
            ("NULL", "(NULL)"),
            ("0x01", "(0x01)"),
            ("(((1)))", "((1))"),
            ("-(1)", "((-1))"),
            ("+5", "((5))"),
            ("0.0", "((0.0))"),
            ("-1.5", "((-1.5))"),
            ("9223372036854775807", "((9223372036854775807.))"),
            ("2147483648", "((2147483648.))"),
            ("2147483647", "((2147483647))"),
            (".5", "((0.5))"),
            ("-0", "((0))"),
        ] {
            assert_eq!(check(source).as_deref(), Some(expected), "{source}");
        }
    }

    #[test]
    fn operators_parenthesize_like_sql_server() {
        for (source, expected) in [
            ("a > 0", "([a]>(0))"),
            (
                "a * (b + 1) - 2 / (a - 1) > 0",
                "(([a]*([b]+(1))-(2)/([a]-(1)))>(0))",
            ),
            ("a - (b - c) > 0", "(([a]-([b]-[c]))>(0))"),
            ("(a - b) - c > 0", "((([a]-[b])-[c])>(0))"),
            ("a * b * c > 0", "(([a]*[b])*[c]>(0))"),
            ("a / (b * c) > 0", "([a]/([b]*[c])>(0))"),
            ("(a + b) * c > 0", "(([a]+[b])*[c]>(0))"),
            ("a = b + c", "([a]=([b]+[c]))"),
            ("a + b * c > 0", "(([a]+[b]*[c])>(0))"),
            ("a % b * c > 0", "(([a]%[b])*[c]>(0))"),
            ("-a < 0", "(( -[a])<(0))"),
            ("-(a + b) < 0", "(( -([a]+[b]))<(0))"),
            ("~a = 0", "(~[a]=(0))"),
            ("~(a + b) = 0", "(~([a]+[b])=(0))"),
            ("a - -1 > 0", "(([a]-(-1))>(0))"),
            ("a & 1 = 0", "(([a]&(1))=(0))"),
            ("a | b ^ c > 0", "((([a]|[b])^[c])>(0))"),
            ("a & b + c > 0", "((([a]&[b])+[c])>(0))"),
            ("a | (b ^ c) > 0", "(([a]|([b]^[c]))>(0))"),
            ("s + 'x' <> 'yx'", "(([s]+'x')<>'yx')"),
            ("a != 4", "([a]<>(4))"),
            ("1 + 2 * 3", "((1)+(2)*(3))"),
            ("1 - 2 - 3", "(((1)-(2))-(3))"),
            ("2 * (3 + 4)", "((2)*((3)+(4)))"),
            ("'abc' + 'd'", "('abc'+'d')"),
            ("a * 2", "([a]*(2))"),
            ("-a", "( -[a])"),
        ] {
            assert_eq!(check(source).as_deref(), Some(expected), "{source}");
        }
    }

    #[test]
    fn logic_lists_and_ranges_expand_like_sql_server() {
        for (source, expected) in [
            ("a IN (1,2,3)", "([a]=(3) OR [a]=(2) OR [a]=(1))"),
            ("s IN ('x','y')", "([s]='y' OR [s]='x')"),
            ("a IN (1)", "([a]=(1))"),
            ("a NOT IN (7,8)", "(NOT ([a]=(8) OR [a]=(7)))"),
            (
                "a NOT IN (1) AND NOT (a IN (2,3))",
                "(NOT [a]=(1) AND NOT ([a]=(3) OR [a]=(2)))",
            ),
            ("a BETWEEN 1 AND 10", "([a]>=(1) AND [a]<=(10))"),
            (
                "a NOT BETWEEN 100 AND 200",
                "(NOT ([a]>=(100) AND [a]<=(200)))",
            ),
            ("a BETWEEN b+1 AND c*2", "([a]>=([b]+(1)) AND [a]<=[c]*(2))"),
            (
                "(a > 0 OR b > 0) AND a <> 3",
                "(([a]>(0) OR [b]>(0)) AND [a]<>(3))",
            ),
            (
                "a > 0 AND b > 0 OR c > 0",
                "([a]>(0) AND [b]>(0) OR [c]>(0))",
            ),
            (
                "a > 0 OR (b > 0 OR c > 0)",
                "([a]>(0) OR ([b]>(0) OR [c]>(0)))",
            ),
            (
                "(a > 0 AND b > 0) AND c > 0",
                "([a]>(0) AND [b]>(0) AND [c]>(0))",
            ),
            ("a IN (1,2) AND b > 0", "(([a]=(2) OR [a]=(1)) AND [b]>(0))"),
            ("b > 0 OR a IN (1,2)", "([b]>(0) OR ([a]=(2) OR [a]=(1)))"),
            (
                "a BETWEEN 1 AND 2 AND b > 0",
                "([a]>=(1) AND [a]<=(2) AND [b]>(0))",
            ),
            ("NOT a = 1", "(NOT [a]=(1))"),
            ("NOT (a IS NULL)", "(NOT [a] IS NULL)"),
            ("NOT (a > 0 AND b > 0)", "(NOT ([a]>(0) AND [b]>(0)))"),
            ("b IS NULL OR b >= a", "([b] IS NULL OR [b]>=[a])"),
            ("s LIKE 'a%'", "([s] like 'a%')"),
            ("s NOT LIKE 'z%'", "(NOT [s] like 'z%')"),
            (
                "s LIKE '%[0-9]%' ESCAPE '\\'",
                "([s] like '%[0-9]%' escape '\\' )",
            ),
            (
                "a IN (1, b, c + 1)",
                "([a]=([c]+(1)) OR [a]=[b] OR [a]=(1))",
            ),
        ] {
            assert_eq!(check(source).as_deref(), Some(expected), "{source}");
        }
    }

    #[test]
    fn functions_conversions_and_case_match_sql_server() {
        for (source, expected) in [
            ("len(s) > 2", "(len([s])>(2))"),
            (
                "d < dateadd(day, 1, getdate())",
                "([d]<dateadd(day,(1),getdate()))",
            ),
            ("dateadd(month, -1, d) < d", "(dateadd(month,(-1),[d])<[d])"),
            ("datepart(dw, d) > 0", "(datepart(weekday,[d])>(0))"),
            ("year(d) > 2000", "(datepart(year,[d])>(2000))"),
            ("CAST(s AS INT) > 0", "(CONVERT([int],[s])>(0))"),
            (
                "CONVERT(VARCHAR(10), d, 120) > '2000'",
                "(CONVERT([varchar](10),[d],(120))>'2000')",
            ),
            (
                "CAST(a AS DECIMAL(5,2)) > 0",
                "(CONVERT([decimal](5,2),[a])>(0))",
            ),
            (
                "CAST(s AS NVARCHAR(MAX)) <> ''",
                "(CONVERT([nvarchar](max),[s])<>'')",
            ),
            ("CAST(a AS BIGINT)", "(CONVERT([bigint],[a]))"),
            ("CAST(1 AS BIGINT)", "(CONVERT([bigint],(1)))"),
            ("CONVERT(INT,'2')", "(CONVERT([int],'2'))"),
            (
                "TRY_CAST(s AS INT) IS NOT NULL",
                "(TRY_CAST([s] AS [int]) IS NOT NULL)",
            ),
            ("TRY_CONVERT(int, s) > 0", "(TRY_CAST([s] AS [int])>(0))"),
            (
                "CASE a WHEN 1 THEN 1 ELSE 0 END = 1",
                "(case [a] when (1) then (1) else (0) end=(1))",
            ),
            (
                "CASE WHEN a > 0 THEN a ELSE 0 END",
                "(case when [a]>(0) then [a] else (0) end)",
            ),
            (
                "CASE WHEN a > 0 AND b > 0 THEN 1 WHEN a IS NULL THEN 2 END = 1",
                "(case when [a]>(0) AND [b]>(0) then (1) when [a] IS NULL then (2)  end=(1))",
            ),
            (
                "CASE WHEN a+1>0 THEN a+1 ELSE a*2 END > 0",
                "(case when ([a]+(1))>(0) then [a]+(1) else [a]*(2) end>(0))",
            ),
            (
                "iif(a > 0, 1, 0) = 1",
                "(case when [a]>(0) then (1) else (0) end=(1))",
            ),
            ("coalesce(a, b, 0) >= 0", "(coalesce([a],[b],(0))>=(0))"),
            ("ISNULL(b, 0)", "(isnull([b],(0)))"),
            ("abs(a+b) < 10", "(abs([a]+[b])<(10))"),
            ("CAST(a+1 AS bigint) > 0", "(CONVERT([bigint],[a]+(1))>(0))"),
            ("GETDATE()", "(getdate())"),
            ("NEWID()", "(newid())"),
            ("CURRENT_TIMESTAMP", "(getdate())"),
            (
                "lower(s) = 'x' COLLATE Latin1_General_BIN",
                "(lower([s])=('x') collate Latin1_General_BIN)",
            ),
            (
                "s COLLATE Latin1_General_CS_AS = 'x'",
                "(([s]) collate Latin1_General_CS_AS='x')",
            ),
            (
                "cast(cast(a AS bigint) AS int) > 0",
                "(CONVERT([int],CONVERT([bigint],[a]))>(0))",
            ),
            ("len(s + 'x') > 0", "(len([s]+'x')>(0))"),
            ("substring(s, 1, 2) = 'ab'", "(substring([s],(1),(2))='ab')"),
        ] {
            assert_eq!(check(source).as_deref(), Some(expected), "{source}");
        }
    }

    fn reference() -> serde_json::Value {
        serde_json::from_str(include_str!(
            "../../../../../../reference/gaps-catalog.json"
        ))
        .unwrap()
    }

    fn record<'a>(records: &'a [serde_json::Value], name: &str) -> &'a serde_json::Value {
        records
            .iter()
            .find(|record| record["name"] == name)
            .unwrap_or_else(|| panic!("missing record {name}"))
    }

    fn rows(record: &serde_json::Value) -> &Vec<serde_json::Value> {
        record["result"]["sets"][0]["rows"].as_array().unwrap()
    }

    fn table(sql: &str) -> CreateTable {
        let statement = super::super::declarations::parse(sql)
            .unwrap()
            .into_iter()
            .find(|statement| matches!(statement, Statement::CreateTable(_)))
            .unwrap();
        let Statement::CreateTable(table) = statement else {
            unreachable!()
        };
        table
    }

    /// Every captured CHECK, DEFAULT and computed definition, from SQL
    /// Server's own text of the declaring statements.
    #[test]
    fn definitions_match_every_captured_declaration() {
        let reference = reference();
        let mut checked = 0;
        // CHECK constraints: name -> captured definition.
        let v2 = reference["catalogV2Profile"]["runs"][0].as_array().unwrap();
        let expected: std::collections::HashMap<String, String> =
            rows(record(v2, "check definitions"))
                .iter()
                .map(|row| {
                    (
                        row[0].as_str().unwrap().to_owned(),
                        row[1].as_str().unwrap().to_owned(),
                    )
                })
                .collect();
        for setup in ["setup checks one", "setup checks two", "setup checks three"] {
            let table = table(record(v2, setup)["sql"].as_str().unwrap());
            for constraint in &table.constraints {
                let TableConstraint::Check(check) = constraint else {
                    continue;
                };
                let name = &check.name.as_ref().unwrap().value;
                assert_eq!(
                    definition(&check.expr).as_deref(),
                    Some(expected[name].as_str()),
                    "{name}"
                );
                checked += 1;
            }
        }
        assert_eq!(checked, expected.len());
        // DEFAULTs and computed columns: column -> captured definition.
        let columns = |records: &[serde_json::Value], setup: &str, query: &str, column: usize| {
            let table = table(record(records, setup)["sql"].as_str().unwrap());
            let mut count = 0;
            for row in rows(record(records, query)) {
                let name = row[0].as_str().unwrap();
                let definition_text = row[column].as_str().unwrap();
                let declared = table
                    .columns
                    .iter()
                    .find(|c| c.name.value == name)
                    .unwrap_or_else(|| panic!("{name}"));
                let expr = declared
                    .options
                    .iter()
                    .find_map(|o| match &o.option {
                        ColumnOption::Default(expr) => Some(expr),
                        _ => None,
                    })
                    .or_else(|| crate::dialect::computed_column::computed(declared).map(|(e, _)| e))
                    .unwrap();
                assert_eq!(definition(expr).as_deref(), Some(definition_text), "{name}");
                count += 1;
            }
            count
        };
        checked += columns(v2, "setup defaults", "default definitions", 2);
        checked += columns(v2, "setup computed", "computed definitions", 7);
        checked += columns(v2, "setup computed text", "computed text definitions", 5);
        let v1 = reference["definitionProfile"]["runs"][0]
            .as_array()
            .unwrap();
        let named = |records: &[serde_json::Value], setup: &str, query: &str| {
            let table = table(record(records, setup)["sql"].as_str().unwrap());
            let mut count = 0;
            for row in rows(record(records, query)) {
                let row = row.as_array().unwrap();
                let index = row[1].as_u64().unwrap() as usize - 1;
                let text = if row.len() == 4 { &row[2] } else { &row[8] };
                let declared = &table.columns[index];
                let expr = declared
                    .options
                    .iter()
                    .find_map(|o| match &o.option {
                        ColumnOption::Default(expr) => Some(expr),
                        _ => None,
                    })
                    .or_else(|| crate::dialect::computed_column::computed(declared).map(|(e, _)| e))
                    .unwrap();
                assert_eq!(
                    definition(expr).as_deref(),
                    text.as_str(),
                    "{}",
                    declared.name
                );
                count += 1;
            }
            count
        };
        checked += named(v1, "setup default definitions", "default definitions");
        checked += named(v1, "setup computed definitions", "computed definitions");
        assert!(checked > 150, "{checked}");
    }

    #[test]
    fn unknown_forms_have_no_definition() {
        for source in [
            "EXISTS (SELECT 1)",
            "@v + 1",
            "(SELECT 1)",
            "a > ALL (SELECT 1)",
            "nextval('main.__msduck_identity_1')",
            "getvariable('__msduck_session_login')",
        ] {
            assert_eq!(check(source), None, "{source}");
        }
    }
}
