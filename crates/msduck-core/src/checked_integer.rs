//! Checked INT/BIGINT arithmetic over already-bound, typed operands.
//!
//! Expected SQL failures are values, not backend exceptions. Session settings,
//! evaluation order, operand conversion and transport delivery belong to callers.
use crate::diagnostic::SqlError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Integer {
    Int(Option<i32>),
    BigInt(Option<i64>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Add,
    Subtract,
    Multiply,
    Divide,
    Modulo,
}

impl Integer {
    fn value(self) -> Option<i128> {
        match self {
            Self::Int(value) => value.map(i128::from),
            Self::BigInt(value) => value.map(i128::from),
        }
    }
}

/// BIGINT has precedence even when its operand is NULL. SQL NULL propagates
/// before zero-divisor checks. Minimum-value remainder by -1 raises the same
/// overflow as division, despite its mathematical remainder being zero.
pub fn calculate(operation: Operation, left: Integer, right: Integer) -> Result<Integer, SqlError> {
    let big = matches!(left, Integer::BigInt(_)) || matches!(right, Integer::BigInt(_));
    let (Some(left), Some(right)) = (left.value(), right.value()) else {
        return Ok(if big {
            Integer::BigInt(None)
        } else {
            Integer::Int(None)
        });
    };
    let (minimum, maximum, name) = if big {
        (i128::from(i64::MIN), i128::from(i64::MAX), "bigint")
    } else {
        (i128::from(i32::MIN), i128::from(i32::MAX), "int")
    };
    let overflow = || {
        SqlError::new(
            8115,
            2,
            format!("Arithmetic overflow error converting expression to data type {name}."),
        )
    };
    if matches!(operation, Operation::Divide | Operation::Modulo) {
        if right == 0 {
            return Err(SqlError::new(8134, 1, "Divide by zero error encountered."));
        }
        if left == minimum && right == -1 {
            return Err(overflow());
        }
    }
    // Two signed 64-bit inputs, including their product, fit in i128. The
    // declared SQL result width is enforced below; widening is only internal.
    let value = match operation {
        Operation::Add => left + right,
        Operation::Subtract => left - right,
        Operation::Multiply => left * right,
        Operation::Divide => left / right,
        Operation::Modulo => left % right,
    };
    if !(minimum..=maximum).contains(&value) {
        return Err(overflow());
    }
    Ok(if big {
        Integer::BigInt(Some(value as i64))
    } else {
        Integer::Int(Some(value as i32))
    })
}
