//! Evaluation for the statement-by-statement interpreter: column-free
//! expressions are evaluated by the session and come back as literals.
use crate::engine::{Parameter, Session};
use anyhow::{Result, anyhow, bail};
use duckdb::types::{TimeUnit, Value};
use msduck_sql::dialect::ext::functions::interpret::Evaluator;
use sqlparser::ast::{DataType, Expr, SelectItem, SetExpr, Statement};
use std::collections::HashMap;

pub(super) struct Evaluation<'a> {
    pub session: &'a Session,
    /// The caller's variables; arguments may reference them.
    pub parameters: &'a HashMap<String, Parameter>,
}

impl Evaluator for Evaluation<'_> {
    fn value(&mut self, expr: Expr, data_type: &DataType) -> Result<Expr> {
        let value = self
            .session
            .evaluate_scalar(expr, data_type.clone(), self.parameters)?;
        literal(value, data_type)
    }

    fn truth(&mut self, expr: Expr) -> Result<bool> {
        Ok(matches!(
            self.session
                .evaluate_expression(expr, self.parameters, true)?,
            Value::Boolean(true)
        ))
    }
}

/// Days since 1970-01-01 as a civil date.
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

fn micros(unit: TimeUnit, value: i64) -> i64 {
    match unit {
        TimeUnit::Second => value * 1_000_000,
        TimeUnit::Millisecond => value * 1_000,
        TimeUnit::Microsecond => value,
        TimeUnit::Nanosecond => value / 1_000,
    }
}

fn escape(text: &str) -> String {
    text.replace('\'', "''")
}

/// A literal expression of `data_type` with the given backend value.
pub(super) fn literal(value: Value, data_type: &DataType) -> Result<Expr> {
    let text = match value {
        Value::Null => "NULL".to_string(),
        Value::Boolean(value) => (if value { "1" } else { "0" }).to_string(),
        Value::TinyInt(v) => format!("'{v}'"),
        Value::SmallInt(v) => format!("'{v}'"),
        Value::Int(v) => format!("'{v}'"),
        Value::BigInt(v) => format!("'{v}'"),
        Value::HugeInt(v) => format!("'{v}'"),
        Value::UTinyInt(v) => format!("'{v}'"),
        Value::USmallInt(v) => format!("'{v}'"),
        Value::UInt(v) => format!("'{v}'"),
        Value::UBigInt(v) => format!("'{v}'"),
        Value::Float(v) => format!("'{v:?}'"),
        Value::Double(v) => format!("'{v:?}'"),
        Value::Decimal(v) => format!("'{v}'"),
        Value::Text(text) => format!("N'{}'", escape(&text)),
        Value::Blob(bytes) => {
            let hex: String = bytes.iter().map(|b| format!("{b:02X}")).collect();
            format!("0x{hex}")
        }
        Value::Date32(days) => {
            let (y, m, d) = civil(i64::from(days));
            format!("'{y:04}-{m:02}-{d:02}'")
        }
        Value::Timestamp(unit, value) => {
            let us = micros(unit, value);
            let days = us.div_euclid(86_400_000_000);
            let time = us.rem_euclid(86_400_000_000);
            let (y, m, d) = civil(days);
            let seconds = time / 1_000_000;
            let fraction = time % 1_000_000;
            let fraction = if fraction % 1000 == 0 {
                format!("{:03}", fraction / 1000)
            } else {
                format!("{fraction:06}")
            };
            format!(
                "'{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{fraction}'",
                seconds / 3600,
                seconds / 60 % 60,
                seconds % 60
            )
        }
        Value::Time64(unit, value) => {
            let ns = match unit {
                TimeUnit::Second => value * 1_000_000_000,
                TimeUnit::Millisecond => value * 1_000_000,
                TimeUnit::Microsecond => value * 1_000,
                TimeUnit::Nanosecond => value,
            };
            let ticks = ns / 100;
            let seconds = ticks / 10_000_000;
            format!(
                "'{:02}:{:02}:{:02}.{:07}'",
                seconds / 3600,
                seconds / 60 % 60,
                seconds % 60,
                ticks % 10_000_000
            )
        }
        other => bail!("unsupported: user-defined function value {other:?}"),
    };
    let statements = msduck_sql::batch::parse(&format!("SELECT CAST({text} AS {data_type})"))?;
    match statements.into_iter().next() {
        Some(Statement::Query(query)) => match *query.body {
            SetExpr::Select(select) => match select.projection.into_iter().next() {
                Some(SelectItem::UnnamedExpr(expr)) => Ok(expr),
                _ => Err(anyhow!("invalid literal")),
            },
            _ => Err(anyhow!("invalid literal")),
        },
        _ => Err(anyhow!("invalid literal")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(18263), (2020, 1, 2));
        assert_eq!(civil(-719_162), (1, 1, 1));
    }
}
