//! Pure MERGE TOP binding. The parser and runtime integration are separate.
use sqlparser::{
    ast::{BinaryOperator, Expr, Top, TopQuantity, UnaryOperator, Value},
    keywords::Keyword,
    parser::{Parser, ParserError},
};

/// Consume TOP immediately after MERGE. The returned AST retains its expression
/// and PERCENT flag; the surrounding MERGE parser must retain it alongside Merge.
pub fn parse_after_merge(parser: &mut Parser) -> Result<Option<Top>, ParserError> {
    if parser.parse_keyword(Keyword::TOP) {
        parser.parse_top().map(Some)
    } else {
        Ok(None)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoundValue {
    /// SQL INT, including the captured declared `@n INT` input.
    Int(i32),
    Decimal(String),
    Null,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindError {
    Sql {
        number: i32,
        state: u8,
        class: u8,
        message: &'static str,
    },
    Unsupported(&'static str),
}

const NEGATIVE: BindError = BindError::Sql {
    number: 127,
    state: 1,
    class: 15,
    message: "A TOP N or FETCH rowcount value may not be negative.",
};
const NONINTEGER: BindError = BindError::Sql {
    number: 1060,
    state: 1,
    class: 15,
    message: "The number of rows provided for a TOP or FETCH clauses row count parameter must be an integer.",
};
const INVALID_PERCENT: BindError = BindError::Sql {
    number: 1014,
    state: 1,
    class: 15,
    message: "A TOP or FETCH clause contains an invalid value.",
};
const PERCENT_RANGE: BindError = BindError::Sql {
    number: 1031,
    state: 1,
    class: 15,
    message: "Percent values must be between 0 and 100.",
};

/// Resolve variables against an explicit, already bound snapshot. Each leaf is
/// visited once. Expressions outside the captured forms remain unsupported.
pub fn bind(
    top: Top,
    mut variable: impl FnMut(&str) -> Option<BoundValue>,
) -> Result<BoundTop, BindError> {
    if top.with_ties {
        return Err(BindError::Unsupported("MERGE TOP WITH TIES"));
    }
    let value = match &top.quantity {
        Some(TopQuantity::Expr(expr)) => evaluate(expr, &mut variable)?,
        Some(TopQuantity::Constant(_)) => {
            return Err(BindError::Unsupported("unparenthesized MERGE TOP"));
        }
        None => return Err(BindError::Unsupported("MERGE TOP without quantity")),
    };
    let amount = if top.percent {
        TopAmount::PercentThousandths(percent_thousandths(&value)?)
    } else {
        let count = match &value {
            BoundValue::Int(n) if *n < 0 => return Err(NEGATIVE),
            BoundValue::Int(n) => *n as u64,
            BoundValue::Decimal(n) if n == "1.5" => return Err(NONINTEGER),
            BoundValue::Decimal(_) => {
                return Err(BindError::Unsupported(
                    "uncaptured MERGE TOP count decimal type",
                ));
            }
            BoundValue::Null => return Err(NONINTEGER),
        };
        TopAmount::Rows(count)
    };
    Ok(BoundTop {
        source: top,
        bound_value: value,
        amount,
    })
}

/// Captured percentage inputs are INT or fixed decimal with at most three
/// fractional digits. The scaled representation avoids arithmetic overflow and
/// preserves the explicit source type in `BoundTop::bound_value`.
fn percent_thousandths(value: &BoundValue) -> Result<u32, BindError> {
    match value {
        BoundValue::Int(n) if !(0..=100).contains(n) => Err(PERCENT_RANGE),
        BoundValue::Int(n) => Ok((*n as u32) * 1_000),
        BoundValue::Null => Err(INVALID_PERCENT),
        BoundValue::Decimal(source) => {
            let (negative, source) = match source.strip_prefix('-') {
                Some(rest) => (true, rest),
                None => (false, source.as_str()),
            };
            let (whole, fraction) = source
                .split_once('.')
                .ok_or(BindError::Unsupported("uncaptured MERGE TOP decimal form"))?;
            if whole.is_empty()
                || fraction.is_empty()
                || fraction.len() > 3
                || !whole.bytes().all(|b| b.is_ascii_digit())
                || !fraction.bytes().all(|b| b.is_ascii_digit())
            {
                return Err(BindError::Unsupported("uncaptured MERGE TOP decimal form"));
            }
            let whole: u32 = whole
                .parse()
                .map_err(|_| BindError::Unsupported("MERGE TOP percentage overflow"))?;
            let part: u32 = fraction
                .parse()
                .map_err(|_| BindError::Unsupported("MERGE TOP percentage overflow"))?;
            let scale = 10u32.pow((3 - fraction.len()) as u32);
            let scaled = whole
                .checked_mul(1_000)
                .and_then(|n| part.checked_mul(scale).and_then(|part| n.checked_add(part)))
                .ok_or(BindError::Unsupported("MERGE TOP percentage overflow"))?;
            if negative || scaled > 100_000 {
                Err(PERCENT_RANGE)
            } else {
                Ok(scaled)
            }
        }
    }
}

fn evaluate(
    expr: &Expr,
    variable: &mut impl FnMut(&str) -> Option<BoundValue>,
) -> Result<BoundValue, BindError> {
    match expr {
        Expr::Nested(inner) => evaluate(inner, variable),
        Expr::Value(v) => match &v.value {
            Value::Null => Ok(BoundValue::Null),
            Value::Number(n, _) if n.chars().all(|c| c.is_ascii_digit()) => n
                .parse::<i32>()
                .map(BoundValue::Int)
                .map_err(|_| BindError::Unsupported("MERGE TOP non-INT literal")),
            Value::Number(n, _) if n.contains('.') => Ok(BoundValue::Decimal(n.clone())),
            _ => Err(BindError::Unsupported("MERGE TOP literal type")),
        },
        Expr::Identifier(id) if id.quote_style.is_none() && id.value.starts_with('@') => {
            variable(&id.value).ok_or(BindError::Unsupported("unbound MERGE TOP variable"))
        }
        Expr::UnaryOp {
            op: UnaryOperator::Minus,
            expr,
        } => match evaluate(expr, variable)? {
            BoundValue::Int(n) => n
                .checked_neg()
                .map(BoundValue::Int)
                .ok_or(BindError::Unsupported("MERGE TOP integer overflow")),
            BoundValue::Decimal(n) if !n.starts_with('-') => {
                Ok(BoundValue::Decimal(format!("-{n}")))
            }
            other => Ok(other),
        },
        Expr::UnaryOp {
            op: UnaryOperator::Plus,
            expr,
        } => evaluate(expr, variable),
        Expr::BinaryOp {
            left,
            op: BinaryOperator::Plus,
            right,
        } => {
            let left = evaluate(left, variable)?;
            let right = evaluate(right, variable)?;
            match (left, right) {
                (BoundValue::Int(a), BoundValue::Int(b)) => a
                    .checked_add(b)
                    .map(BoundValue::Int)
                    .ok_or(BindError::Unsupported("MERGE TOP integer overflow")),
                _ => Err(BindError::Unsupported("MERGE TOP arithmetic type")),
            }
        }
        _ => Err(BindError::Unsupported("uncaptured MERGE TOP expression")),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundTop {
    pub source: Top,
    /// Retains the explicitly evaluated value and its type category.
    pub bound_value: BoundValue,
    pub amount: TopAmount,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopAmount {
    Rows(u64),
    /// Percentage in units of 0.001%, bounded to 0..=100_000.
    PercentThousandths(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectionRequirement {
    /// Count after joining and action qualification, supplied by the caller.
    pub eligible: u64,
    /// Upper bound on selected candidates, not an affected-row prediction.
    pub take: u64,
    /// The shell must not infer an ordering from this requirement.
    pub unordered: bool,
}

impl BoundTop {
    pub fn selection(&self, eligible: u64) -> Result<SelectionRequirement, BindError> {
        let limit = match self.amount {
            TopAmount::Rows(count) => count.min(eligible),
            TopAmount::PercentThousandths(scaled) => {
                let numerator = u128::from(eligible)
                    .checked_mul(u128::from(scaled))
                    .and_then(|n| n.checked_add(99_999))
                    .ok_or(BindError::Unsupported("MERGE TOP percentage overflow"))?;
                u64::try_from(numerator / 100_000)
                    .map_err(|_| BindError::Unsupported("MERGE TOP percentage overflow"))?
            }
        };
        Ok(SelectionRequirement {
            eligible,
            take: limit,
            unordered: true,
        })
    }
}
