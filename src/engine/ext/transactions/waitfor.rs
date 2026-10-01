//! WAITFOR DELAY and WAITFOR TIME, following the captured SQL Server rules
//! (docs/gaps-transactions.md):
//!
//! - A literal is validated while the batch compiles (148).
//! - A NULL variable of any type does not wait.
//! - A non-MAX character variable is parsed like a literal at run time; an
//!   invalid or MAX value fails with 241.
//! - A `datetime` variable contributes its time of day; an `int` or
//!   `smallint` variable is a number of seconds (for TIME, seconds after
//!   midnight). Other types fail with 9815.
//! - WAITFOR TIME waits until the next occurrence of that time of day, in
//!   the clock GETDATE() reads.
//!
//! The wait ends early when the request is cancelled (an Attention) or the
//! session is terminated.
use super::super::super::{Execution, Parameter, Session};
use crate::sessions::Wake;
use anyhow::Result;
use msduck_core::{diagnostic::SqlError, types::Type, value::Value};
use msduck_sql::dialect::ext::transactions::{Wait, WaitValue, time_string};
use std::{collections::HashMap, time::Duration};

const DAY_MS: u64 = 86_400_000;

/// How often a wait checks the request's Attention flag.
const POLL: Duration = Duration::from_millis(10);

pub(super) fn run(
    session: &mut Session,
    wait: Wait,
    value: WaitValue<'_>,
    parameters: &HashMap<String, Parameter>,
) -> Result<Execution> {
    let milliseconds = match value {
        WaitValue::Milliseconds(milliseconds) => Some(u64::from(milliseconds)),
        WaitValue::Variable(variable) => variable_milliseconds(&variable.to_string(), parameters)?,
    };
    // WAITFOR leaves @@ROWCOUNT at 0 and has no row count of its own.
    let done = Execution::statement(Vec::new(), None, 0);
    let Some(milliseconds) = milliseconds else {
        return Ok(done);
    };
    let interval = match wait {
        Wait::Delay => Duration::from_millis(milliseconds),
        Wait::Time => until(milliseconds % DAY_MS, crate::current_time::now()),
    };
    let attention = session.read_cancel.clone();
    let wake = session.process.wait(interval, POLL, &|| {
        attention
            .as_ref()
            .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::SeqCst))
    });
    match wake {
        Wake::Elapsed => Ok(done),
        // The engine completes a cancelled request like an interrupted read:
        // no later statement or CATCH block runs.
        Wake::Cancelled => Err(crate::read_cancellation::CancelledRead {
            metadata: Vec::new(),
        }
        .into()),
        Wake::Terminated => Err(anyhow::anyhow!(
            "WAITFOR ended because the session is being terminated"
        )),
    }
}

/// The time from `now` until the next `time_of_day` (milliseconds after
/// local midnight). A time of day that has already passed today is reached
/// tomorrow.
fn until(time_of_day: u64, now: msduck_sql::session_function::Clock) -> Duration {
    let local_ms = (now.utc_ticks / 10_000 + i64::from(now.offset_minutes) * 60_000)
        .rem_euclid(DAY_MS as i64) as u64;
    let remaining = (time_of_day + DAY_MS - local_ms) % DAY_MS;
    Duration::from_millis(remaining)
}

fn variable_milliseconds(
    variable: &str,
    parameters: &HashMap<String, Parameter>,
) -> Result<Option<u64>> {
    let parameter = parameters.get(&variable.to_lowercase()).ok_or_else(|| {
        SqlError::syntax(
            137,
            2,
            format!("Must declare the scalar variable \"{variable}\"."),
        )
    })?;
    // A NULL value never waits, whatever its type.
    if parameter.value == Value::Null {
        return Ok(None);
    }
    let conversion = || -> anyhow::Error {
        SqlError::new(
            241,
            1,
            "Conversion failed when converting date and/or time from character string.",
        )
        .into()
    };
    match parameter.data_type {
        Type::Character(character) => {
            let text = match &parameter.value {
                Value::Null => return Ok(None),
                Value::Text(text) => text.clone(),
                Value::Unicode(units) => String::from_utf16_lossy(units),
                _ => return Err(conversion()),
            };
            if character.length() == msduck_core::character::Length::Max {
                return Err(conversion());
            }
            time_string(&text)
                .map(|milliseconds| Some(u64::from(milliseconds)))
                .ok_or_else(conversion)
        }
        Type::DateTime => Ok(match &parameter.value {
            Value::Null => None,
            Value::Timestamp(unit, value) => {
                use msduck_core::value::TimeUnit;
                let per_ms = match unit {
                    TimeUnit::Second => return Ok(Some(value.rem_euclid(86_400) as u64 * 1000)),
                    TimeUnit::Millisecond => 1,
                    TimeUnit::Microsecond => 1_000,
                    TimeUnit::Nanosecond => 1_000_000,
                };
                Some((value.div_euclid(per_ms)).rem_euclid(DAY_MS as i64) as u64)
            }
            Value::Text(text) => {
                // `yyyy-mm-dd hh:mm:ss.fff`: only the time of day counts.
                let time = text.rsplit(' ').next().unwrap_or(text);
                Some(u64::from(time_string(time).ok_or_else(conversion)?))
            }
            other => anyhow::bail!("unexpected datetime variable value {other:?}"),
        }),
        // A negative count wraps like SQL Server's unsigned wait, which
        // effectively waits until cancelled.
        Type::Int | Type::SmallInt => Ok(match &parameter.value {
            Value::Null => None,
            Value::Int(seconds) => Some(u64::from(*seconds as u32) * 1000),
            Value::SmallInt(seconds) => Some(u64::from(*seconds as i32 as u32) * 1000),
            other => anyhow::bail!("unexpected integer variable value {other:?}"),
        }),
        other => Err(SqlError::new(
            9815,
            0,
            format!(
                "Waitfor delay and waitfor time cannot be of type {}.",
                type_name(other)
            ),
        )
        .into()),
    }
}

/// The type name SQL Server uses in 9815 and 3914.
pub(super) fn type_name(data_type: Type) -> &'static str {
    use msduck_core::character::Family;
    match data_type {
        Type::Bit => "bit",
        Type::TinyInt => "tinyint",
        Type::SmallInt => "smallint",
        Type::Int => "int",
        Type::BigInt => "bigint",
        Type::Real => "real",
        Type::Float => "float",
        Type::Decimal(_) => "decimal",
        Type::Money => "money",
        Type::SmallMoney => "smallmoney",
        Type::Character(character) => match character.family() {
            Family::Varchar => "varchar",
            Family::Char => "char",
            Family::Nvarchar => "nvarchar",
            Family::Nchar => "nchar",
        },
        Type::Binary(binary) if binary.fixed() => "binary",
        Type::Binary(_) => "varbinary",
        Type::Date => "date",
        Type::DateTime => "datetime",
        Type::SmallDateTime => "smalldatetime",
        Type::Time(_) => "time",
        Type::DateTime2(_) => "datetime2",
        Type::DateTimeOffset(_) => "datetimeoffset",
        Type::UniqueIdentifier => "uniqueidentifier",
        Type::Text => "text",
        Type::Ntext => "ntext",
        Type::Image => "image",
        Type::Xml => "xml",
        Type::Variant => "sql_variant",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use msduck_sql::session_function::Clock;

    #[test]
    fn waitfor_time_reaches_the_next_occurrence() {
        // 10:00:00 UTC, two hours east of UTC: local 12:00:00.
        let now = Clock {
            utc_ticks: 10 * 3_600 * 10_000_000,
            offset_minutes: 120,
        };
        assert_eq!(until(12 * 3_600_000 + 500, now), Duration::from_millis(500));
        assert_eq!(
            until(11 * 3_600_000, now),
            Duration::from_millis(DAY_MS - 3_600_000)
        );
        assert_eq!(until(12 * 3_600_000, now), Duration::ZERO);
    }
}
