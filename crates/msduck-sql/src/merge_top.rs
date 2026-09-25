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
    let count = match &value {
        BoundValue::Int(n) if *n < 0 => return Err(NEGATIVE),
        BoundValue::Int(n) => *n as u64,
        BoundValue::Decimal(n) if n == "1.5" && !top.percent => return Err(NONINTEGER),
        BoundValue::Decimal(_) => {
            return Err(BindError::Unsupported("uncaptured MERGE TOP decimal type"));
        }
        BoundValue::Null => return Err(NONINTEGER),
    };
    if top.percent && count != 50 {
        return Err(BindError::Unsupported("uncaptured MERGE TOP percentage"));
    }
    Ok(BoundTop {
        source: top,
        bound_value: value,
        count,
    })
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
    pub count: u64,
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
        let limit = if self.source.percent {
            // Only 50% is established by the retained capture. Ceil(n/2)
            // avoids multiplication overflow even at u64::MAX.
            eligible / 2 + eligible % 2
        } else {
            self.count.min(eligible)
        };
        Ok(SelectionRequirement {
            eligible,
            take: limit,
            unordered: true,
        })
    }
}
