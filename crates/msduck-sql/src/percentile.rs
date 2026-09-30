//! SQL Server ordered-set percentile windows.
use sqlparser::ast::*;

pub const RANGE: &str = "Input parameter of percentile function is outside of range [0, 1].";
const LITERAL: &str = "Percentile requires a numeric literal between 0 and 1.";

/// Declaration intent; DISC requires the caller's ordering-source declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeKind {
    Continuous,
    Discrete,
}

/// Caller-owned syntax, with no converted fraction or acquired runtime value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimePlan {
    pub kind: RuntimeKind,
    pub fraction: Expr,
    pub ordering: OrderByExpr,
    pub window: WindowType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    Shape(String),
    Diagnostic(msduck_core::diagnostic::SqlError),
    /// Not proven by the retained source matrix; adapters must not guess.
    Unknown,
}

/// Plan statement-wide fractions without evaluating them or reading bindings.
/// Conversion and range errors (including constant NULL/text/range errors) are
/// deferred so the root can preserve empty-input suppression and error timing.
pub fn runtime_plan(
    expr: &Expr,
    declarations: &crate::binding_scope::Scope,
) -> Result<Option<RuntimePlan>, PlanError> {
    let Expr::Function(function) = expr else {
        return Ok(None);
    };
    let name = function.name.to_string().to_ascii_lowercase();
    let kind = match name.as_str() {
        "percentile_cont" => RuntimeKind::Continuous,
        "percentile_disc" => RuntimeKind::Discrete,
        _ => return Ok(None),
    };
    let FunctionArguments::List(args) = &function.args else {
        return Err(PlanError::Shape(format!(
            "The {name} function requires 1 argument(s)."
        )));
    };
    let [FunctionArg::Unnamed(FunctionArgExpr::Expr(fraction))] = args.args.as_slice() else {
        return Err(PlanError::Shape(format!(
            "The {name} function requires 1 argument(s)."
        )));
    };
    // Reuse modifier/window validation with a harmless syntax placeholder.
    // Neither this clone nor the original fraction is executed or converted.
    let mut shape = expr.clone();
    if let Expr::Function(f) = &mut shape
        && let FunctionArguments::List(args) = &mut f.args
    {
        args.args[0] = FunctionArg::Unnamed(FunctionArgExpr::Expr(crate::expr::number(0)));
    }
    lower(&mut shape).map_err(PlanError::Shape)?;
    fn null_order(expr: &Expr) -> bool {
        let expr = crate::variant_cast::source(expr).unwrap_or(expr);
        match expr {
            Expr::Value(value) => matches!(value.value, Value::Null),
            Expr::Nested(value) | Expr::Cast { expr: value, .. } => null_order(value),
            _ => false,
        }
    }
    if null_order(&function.within_group[0].expr) {
        return Err(PlanError::Diagnostic(
            msduck_core::diagnostic::SqlError::new(
                5309,
                1,
                "Windowed functions, aggregates and NEXT VALUE FOR functions do not support constants as ORDER BY clause expressions.",
            ),
        ));
    }
    struct Sources<'a> {
        declarations: &'a crate::binding_scope::Scope,
        row_dependent: bool,
        unknown: bool,
        literal_error: Option<msduck_core::diagnostic::SqlError>,
    }
    impl Visitor for Sources<'_> {
        type Break = ();
        fn pre_visit_query(&mut self, query: &Query) -> std::ops::ControlFlow<()> {
            let SetExpr::Select(select) = query.body.as_ref() else {
                self.unknown = true;
                return std::ops::ControlFlow::Continue(());
            };
            self.row_dependent |= !select.from.is_empty();
            let simple = query.with.is_none()
                && query.order_by.is_none()
                && query.limit_clause.is_none()
                && query.fetch.is_none()
                && query.locks.is_empty()
                && query.for_clause.is_none()
                && query.settings.is_none()
                && query.format_clause.is_none()
                && query.pipe_operators.is_empty()
                && select.projection.len() == 1
                && matches!(
                    &select.projection[0],
                    SelectItem::UnnamedExpr(_) | SelectItem::ExprWithAlias { .. }
                )
                && select.optimizer_hints.is_empty()
                && select.distinct.is_none()
                && select.select_modifiers.is_none()
                && select.top.is_none()
                && select.exclude.is_none()
                && select.into.is_none()
                && select.lateral_views.is_empty()
                && select.prewhere.is_none()
                && select.selection.is_none()
                && select.connect_by.is_empty()
                && matches!(&select.group_by, GroupByExpr::Expressions(e,m) if e.is_empty() && m.is_empty())
                && select.cluster_by.is_empty()
                && select.distribute_by.is_empty()
                && select.sort_by.is_empty()
                && select.having.is_none()
                && select.named_window.is_empty()
                && select.qualify.is_none()
                && select.value_table_mode.is_none();
            self.unknown |= !simple;
            std::ops::ControlFlow::Continue(())
        }
        fn pre_visit_expr(&mut self, expr: &Expr) -> std::ops::ControlFlow<()> {
            match expr {
                Expr::Identifier(name) => {
                    if name.quote_style.is_none() && name.value.starts_with('@') {
                        self.unknown |= !self
                            .declarations
                            .parameters
                            .keys()
                            .any(|key| key.eq_ignore_ascii_case(&name.value));
                    } else {
                        self.row_dependent = true;
                    }
                }
                Expr::CompoundIdentifier(_) => self.row_dependent = true,
                Expr::Value(v) => {
                    if let Value::Number(source, _) = &v.value
                        && let Err(message) = numeric_literal(source)
                    {
                        if let Some(error) = literal_diagnostic(&message) {
                            self.literal_error.get_or_insert(error);
                        } else {
                            self.unknown = true;
                        }
                    }
                    self.unknown |= !matches!(
                        v.value,
                        Value::Number(_, _)
                            | Value::Null
                            | Value::SingleQuotedString(_)
                            | Value::NationalStringLiteral(_)
                    )
                }
                Expr::Nested(_)
                | Expr::Subquery(_)
                | Expr::Case { .. }
                | Expr::IsNull(_)
                | Expr::IsNotNull(_) => {}
                Expr::UnaryOp { op, .. } => {
                    self.unknown |= !matches!(
                        op,
                        UnaryOperator::Plus | UnaryOperator::Minus | UnaryOperator::Not
                    )
                }
                Expr::BinaryOp { op, .. } => {
                    self.unknown |= !matches!(
                        op,
                        BinaryOperator::Plus
                            | BinaryOperator::Minus
                            | BinaryOperator::Multiply
                            | BinaryOperator::Divide
                            | BinaryOperator::Modulo
                            | BinaryOperator::Eq
                            | BinaryOperator::NotEq
                            | BinaryOperator::Lt
                            | BinaryOperator::LtEq
                            | BinaryOperator::Gt
                            | BinaryOperator::GtEq
                            | BinaryOperator::And
                            | BinaryOperator::Or
                    )
                }
                Expr::Cast {
                    kind: CastKind::Cast,
                    data_type,
                    format: None,
                    ..
                } => {
                    self.unknown |= !(numeric_cast_supported(data_type)
                        || matches!(data_type, DataType::Varchar(_) | DataType::Nvarchar(_)))
                }
                Expr::Function(f) => {
                    let name = f.name.to_string().to_ascii_lowercase();
                    let expected = match name.as_str() {
                        "abs" => Some(1),
                        "rand" => Some(0),
                        _ => None,
                    };
                    let plain = !f.uses_odbc_syntax
                        && f.filter.is_none()
                        && f.over.is_none()
                        && f.within_group.is_empty()
                        && f.null_treatment.is_none()
                        && matches!(f.parameters, FunctionArguments::None);
                    let valid_args = matches!(&f.args, FunctionArguments::List(args) if args.duplicate_treatment.is_none() && args.clauses.is_empty() && Some(args.args.len()) == expected && args.args.iter().all(|a| matches!(a, FunctionArg::Unnamed(FunctionArgExpr::Expr(_)))));
                    let marker_args = matches!(&f.args, FunctionArguments::List(args) if args.duplicate_treatment.is_none() && args.clauses.is_empty() && args.args.len() == 1);
                    self.unknown |= !(plain
                        && (valid_args
                            || marker_args && crate::variant_cast::source(expr).is_some()));
                }
                _ => self.unknown = true,
            }
            std::ops::ControlFlow::Continue(())
        }
    }
    let mut sources = Sources {
        declarations,
        row_dependent: false,
        unknown: false,
        literal_error: None,
    };
    let _ = Visit::visit(fraction, &mut sources);
    if let Some(error) = sources.literal_error {
        return Err(PlanError::Diagnostic(error));
    }
    if sources.row_dependent {
        return Err(PlanError::Diagnostic(
            msduck_core::diagnostic::SqlError::new(
                8726,
                1,
                format!(
                    "Input parameter of {} function must be a constant.",
                    function.name.to_string().to_ascii_uppercase()
                ),
            ),
        ));
    }
    if sources.unknown {
        return Err(PlanError::Unknown);
    }
    Ok(Some(RuntimePlan {
        kind,
        fraction: fraction.clone(),
        ordering: function.within_group[0].clone(),
        window: function.over.clone().expect("validated percentile window"),
    }))
}

/// Validate a caller-isolated fraction token stream before AST acquisition.
/// NEXT VALUE FOR is not yet parsed by ServerDialect; no sequence is accessed.
pub fn validate_sequence_source(
    tokens: &[sqlparser::tokenizer::Token],
) -> Result<(), msduck_core::diagnostic::SqlError> {
    use sqlparser::tokenizer::Token;
    let significant = tokens
        .iter()
        .filter(|token| !matches!(token, Token::Whitespace(_) | Token::EOF))
        .collect::<Vec<_>>();
    if significant.windows(3).any(|window| window.iter().zip(["NEXT", "VALUE", "FOR"]).all(|(token, expected)| matches!(token, Token::Word(word) if word.quote_style.is_none() && word.value.eq_ignore_ascii_case(expected)))) {
        return Err(msduck_core::diagnostic::SqlError::syntax(11720, 1,
            "NEXT VALUE FOR function is not allowed in the TOP, OVER, OUTPUT, ON, WHERE, GROUP BY, HAVING, or ORDER BY clauses."));
    }
    Ok(())
}

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
        return Ok((number == 0.0, crate::expr::number(format!("{number:e}"))));
    }
    let Some(number) = numeric_constant(fraction) else {
        return Err(LITERAL.into());
    };
    let number = number?.ok_or(RANGE)?;
    if !(0.0..=1.0).contains(&number) {
        return Err(RANGE.into());
    }
    // A fixed-point spelling would make DuckDB parse this as DECIMAL and
    // convert it to DOUBLE again, which can round a neighboring FLOAT to one.
    // Scientific round-trip spelling preserves the already converted bits.
    Ok((number == 0.0, crate::expr::number(format!("{number:e}"))))
}

// Distinguish a numeric source literal from character-to-FLOAT conversion.
// SQL Server's scientific literal reader flushes subnormal FLOAT values to zero;
// conversion through REAL can retain subnormals, so do not apply that rule later.
fn numeric_literal(number: &str) -> Result<f64, String> {
    if !numeric_spelling(number) {
        return Err(LITERAL.into());
    }
    let scientific = number.contains(['e', 'E']);
    if !scientific && decimal_precision(number) > 38 {
        return Err(format!(
            "The number '{number}' is out of the range for numeric representation (maximum precision 38)."
        ));
    }
    let number_value = number.parse::<f64>().map_err(|_| LITERAL)?;
    if !number_value.is_finite() {
        return Err(format!(
            "The floating point value '{number}' is out of the range of computer representation (8 bytes)."
        ));
    }
    Ok(if scientific && number_value.abs() < f64::MIN_POSITIVE {
        0.0
    } else {
        number_value
    })
}
fn numeric_spelling(number: &str) -> bool {
    let (mantissa, exponent) = number
        .split_once(['e', 'E'])
        .map_or((number, None), |(m, e)| (m, Some(e)));
    let mut dots = 0;
    let mut digits = 0;
    for byte in mantissa.bytes() {
        if byte == b'.' {
            dots += 1;
        } else if byte.is_ascii_digit() {
            digits += 1;
        } else {
            return false;
        }
    }
    digits > 0
        && dots <= 1
        && exponent.is_none_or(|e| {
            let e = e.strip_prefix(['+', '-']).unwrap_or(e);
            !e.is_empty() && e.bytes().all(|b| b.is_ascii_digit())
        })
}
fn decimal_precision(number: &str) -> usize {
    let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
    (whole.trim_start_matches('0').len() + fraction.len()).max(1)
}
/// Recognize complete canonical literal-reader diagnostics, never application
/// identity or text with a backend wrapper, altered suffix or invalid spelling.
pub fn literal_diagnostic(message: &str) -> Option<msduck_core::diagnostic::SqlError> {
    let (number, text) = if let Some(text) = message.strip_prefix("The number '").and_then(|s| {
        s.strip_suffix("' is out of the range for numeric representation (maximum precision 38).")
    }) {
        (1007, text)
    } else if let Some(text) = message
        .strip_prefix("The floating point value '")
        .and_then(|s| s.strip_suffix("' is out of the range of computer representation (8 bytes)."))
    {
        (168, text)
    } else {
        return None;
    };
    if !numeric_spelling(text)
        || numeric_literal(text).as_ref().map_err(String::as_str) != Err(message)
    {
        return None;
    }
    Some(msduck_core::diagnostic::SqlError::syntax(
        number, 1, message,
    ))
}
fn numeric_constant(expr: &Expr) -> Option<Result<Option<f64>, String>> {
    let expr = crate::variant_cast::source(expr).unwrap_or(expr);
    match expr {
        Expr::Value(value) => match &value.value {
            Value::Number(n, _) => Some(numeric_literal(n).map(Some)),
            Value::Null => Some(Ok(None)),
            _ => None,
        },
        Expr::Nested(inner)
        | Expr::UnaryOp {
            op: UnaryOperator::Plus,
            expr: inner,
        } => numeric_constant(inner),
        Expr::UnaryOp {
            op: UnaryOperator::Minus,
            expr: inner,
        } => numeric_constant(inner).map(|n| n.map(|n| n.map(|n| -n))),
        Expr::Cast {
            kind: CastKind::Cast,
            expr: inner,
            data_type,
            format: None,
        } => {
            if !numeric_cast_supported(data_type) {
                return None;
            }
            let value = match numeric_constant(inner)? {
                Ok(value) => value,
                Err(e) => return Some(Err(e)),
            };
            let Some(value) = value else {
                return Some(Ok(None));
            };
            let number = match data_type {
                DataType::Float(ExactNumberInfo::None | ExactNumberInfo::Precision(25..=53))
                | DataType::Double(ExactNumberInfo::None) => value,
                DataType::Real | DataType::Float(ExactNumberInfo::Precision(1..=24)) => {
                    f64::from(value as f32)
                }
                DataType::Bit(_) | DataType::Boolean => {
                    if value != 0.0 {
                        1.0
                    } else {
                        0.0
                    }
                }
                DataType::Int(_) | DataType::Integer(_)
                    if value >= f64::from(i32::MIN) && value <= f64::from(i32::MAX) =>
                {
                    value.trunc()
                }
                DataType::Decimal(info) | DataType::Numeric(info) => decimal_cast(inner, *info)?,
                DataType::Custom(_, _) if crate::money_cast::money_type(data_type).is_some() => {
                    let text = decimal_source(inner)?;
                    let scaled = msduck_core::money::parse_text(&text).ok()?;
                    crate::money_cast::money_type(data_type)?
                        .check_scaled(i128::from(scaled))
                        .ok()?;
                    scaled as f64 / 10_000.0
                }
                _ => return None,
            };
            number.is_finite().then_some(Ok(Some(number)))
        }
        _ => None,
    }
}
fn numeric_cast_supported(kind: &DataType) -> bool {
    match kind {
        DataType::Float(ExactNumberInfo::None | ExactNumberInfo::Precision(1..=53))
        | DataType::Double(ExactNumberInfo::None)
        | DataType::Real
        | DataType::Bit(_)
        | DataType::Boolean
        | DataType::Int(_)
        | DataType::Integer(_) => true,
        DataType::Decimal(info) | DataType::Numeric(info) => match info {
            ExactNumberInfo::None => true,
            ExactNumberInfo::Precision(p) => (1..=38).contains(p),
            ExactNumberInfo::PrecisionAndScale(p, s) => {
                (1..=38).contains(p) && *s >= 0 && *s <= *p as i64
            }
        },
        _ => crate::money_cast::money_type(kind).is_some(),
    }
}
fn decimal_source(expr: &Expr) -> Option<String> {
    let expr = crate::variant_cast::source(expr).unwrap_or(expr);
    match expr {
        Expr::Value(value) => match &value.value {
            Value::Number(n, _) if !n.contains(['e', 'E']) => Some(n.clone()),
            _ => None,
        },
        Expr::Nested(n)
        | Expr::UnaryOp {
            op: UnaryOperator::Plus,
            expr: n,
        } => decimal_source(n),
        Expr::UnaryOp {
            op: UnaryOperator::Minus,
            expr: n,
        } => decimal_source(n).map(|n| format!("-{n}")),
        _ => None,
    }
}
fn decimal_cast(expr: &Expr, info: ExactNumberInfo) -> Option<f64> {
    let (precision, scale) = match info {
        ExactNumberInfo::None => (18, 0),
        ExactNumberInfo::Precision(p) => (p, 0),
        ExactNumberInfo::PrecisionAndScale(p, s) => (p, s),
    };
    if precision == 0 || precision > 38 || scale < 0 || scale > precision as i64 {
        return None;
    }
    let text = decimal_source(expr)?;
    let negative = text.starts_with('-');
    let text = text.strip_prefix('-').unwrap_or(&text);
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    let coefficient = format!("{whole}{fraction}").parse::<i128>().ok()?;
    let shift = scale - i64::try_from(fraction.len()).ok()?;
    let coefficient = if shift >= 0 {
        coefficient.checked_mul(10i128.checked_pow(shift.try_into().ok()?)?)?
    } else {
        let factor = 10i128.checked_pow((-shift).try_into().ok()?)?;
        coefficient / factor + i128::from(coefficient % factor >= factor / 2)
    };
    let decimal = msduck_core::value::Decimal::new(
        precision.try_into().ok()?,
        scale.try_into().ok()?,
        if negative { -coefficient } else { coefficient },
    )
    .ok()?;
    decimal.to_string().parse().ok()
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
            for fraction in ["-0.1", "1.1", "NULL", "1.0000000000000002"] {
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
    fn numeric_sources_round_and_flush_only_scientific_literal_subnormals() {
        for n in [
            "0",
            "0e999999999999999999999",
            "1e-999999999999999999999",
            "1e-308",
        ] {
            assert_eq!(numeric_literal(n), Ok(0.0), "{n}");
        }
        assert_eq!(numeric_literal("1.00000000000000000001"), Ok(1.0));
        assert_eq!(numeric_literal("1e-307"), Ok(1e-307));
        assert!(numeric_literal("1e999999999999999999999").is_err());
        assert!(numeric_literal("bad").is_err());
    }
}
