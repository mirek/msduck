//! RPC requests that name a procedure, and RPC OUTPUT parameters.
//!
//! A request naming a procedure (tedious `callProcedure`, mssql
//! `request.execute`) runs through `Session::rpc_call`, the same exec hooks
//! as `EXEC` in a SQL batch. So does sp_executesql (and a prepared
//! statement) with OUTPUT parameters, as `EXEC sp_executesql`. This module
//! completes the response as SQL Server does (reference/gaps-rpc-procedures.json):
//!
//! - a completed call sends RETURNSTATUS, then one RETURNVALUE per OUTPUT
//!   parameter in request order, then DONEPROC (CurCmd 224);
//! - a failed procedure call sends only its error and DONEPROC with the
//!   error flag; a failed sp_executesql also sends its error number as
//!   RETURNSTATUS and the OUTPUT parameters' input values;
//! - an aborted batch ends with DONEPROC and the error flag.
//!
//! A RETURNVALUE echoes the request: its ordinal, the parameter name as
//! sent (empty for a positional parameter), and the parameter's declared
//! type, with SQL Server's collation and a decimal capacity of 17 bytes.
//! The value converts to that type.
use super::RpcParameter;
use crate::{engine::Session, parameter::Parameter, tds};
use anyhow::{Result, bail, ensure};
use msduck_core::{
    character::{CharacterType, Family, Length},
    diagnostic::SqlError,
    types::Type as DataType,
    value::Value,
};

/// One RPC parameter, as an argument of `Session::rpc_call`.
pub(crate) struct RpcArgument<'a> {
    /// `@name` as sent; `None` binds by position.
    pub name: Option<&'a str>,
    pub value: &'a Parameter,
    /// The fByRefValue status flag: an OUTPUT argument.
    pub output: bool,
    /// The fDefaultValue status flag: the parameter's default (`DEFAULT`).
    pub default: bool,
}

/// How a call ended.
pub(crate) enum RpcOutcome {
    /// It completed with this return status.
    Completed(i32),
    /// It failed with this error number. `error` holds the error's tokens,
    /// which are not yet part of the call's tokens: SQL Server places some
    /// of them after RETURNSTATUS.
    Failed { number: i32, error: Vec<u8> },
    /// The batch ended; its error is already in the tokens.
    Aborted,
}

/// What `Session::rpc_call` produced.
pub(crate) struct RpcResult {
    pub tokens: Vec<u8>,
    pub outcome: RpcOutcome,
    /// The final value of each OUTPUT argument, by argument index.
    pub outputs: Vec<Option<Parameter>>,
}

/// The final DONEPROC of an RPC response.
const DONE_PROC: u8 = 0xfe;
/// Its CurCmd, as SQL Server sends it for every RPC.
const RPC_COMMAND: u16 = 224;

fn return_status(out: &mut Vec<u8>, status: i32) {
    out.push(0x79);
    out.extend(status.to_le_bytes());
}

/// TYPE_INFO for a RETURNVALUE of `data_type`, and the descriptor its value
/// is encoded with. `kind` is the TYPE_INFO byte the client sent, which
/// distinguishes DECIMALN from NUMERICN.
fn declaration(kind: u8, data_type: DataType) -> Result<(Vec<u8>, tds::Type)> {
    let character = |id: u8, bytes: Length| {
        let mut info = vec![id];
        info.extend(
            match bytes {
                Length::Bounded(n) => n,
                Length::Max => u16::MAX,
            }
            .to_le_bytes(),
        );
        info.extend(tds::COLLATION);
        info
    };
    Ok(match data_type {
        DataType::TinyInt => (vec![0x26, 1], tds::Type::Int(1)),
        DataType::SmallInt => (vec![0x26, 2], tds::Type::Int(2)),
        DataType::Int => (vec![0x26, 4], tds::Type::Int(4)),
        DataType::BigInt => (vec![0x26, 8], tds::Type::Int(8)),
        DataType::Bit => (vec![0x68, 1], tds::Type::Bit),
        DataType::Real => (vec![0x6d, 4], tds::Type::Float(4)),
        DataType::Float => (vec![0x6d, 8], tds::Type::Float(8)),
        DataType::Decimal(decimal) => (
            vec![
                if kind == 0x6c { 0x6c } else { 0x6a },
                tds::DECIMAL_RESULT_MAX_LENGTH,
                decimal.precision(),
                decimal.scale(),
            ],
            tds::Type::Decimal(decimal.precision(), decimal.scale()),
        ),
        DataType::Money => (vec![0x6e, 8], tds::Type::Money(8)),
        DataType::SmallMoney => (vec![0x6e, 4], tds::Type::Money(4)),
        DataType::Date => (vec![0x28], tds::Type::Date),
        DataType::Time(scale) => (vec![0x29, scale.get()], tds::Type::Time(scale.get())),
        DataType::DateTime2(scale) => (vec![0x2a, scale.get()], tds::Type::DateTime2(scale.get())),
        DataType::DateTimeOffset(scale) => (
            vec![0x2b, scale.get()],
            tds::Type::DateTimeOffset(scale.get()),
        ),
        DataType::DateTime => (vec![0x6f, 8], tds::Type::LegacyDateTime(8)),
        DataType::SmallDateTime => (vec![0x6f, 4], tds::Type::LegacyDateTime(4)),
        DataType::UniqueIdentifier => (vec![0x24, 16], tds::Type::Guid),
        DataType::Character(character_type) => {
            let length = character_type.length();
            match (character_type.family(), length) {
                (Family::Nvarchar, Length::Bounded(n)) => (
                    character(0xe7, Length::Bounded(n * 2)),
                    tds::Type::Nvarchar(n),
                ),
                (Family::Nvarchar, Length::Max) => (character(0xe7, Length::Max), tds::Type::Text),
                (Family::Varchar, Length::Bounded(n)) => {
                    (character(0xa7, length), tds::Type::Varchar(n))
                }
                (Family::Varchar, Length::Max) => {
                    (character(0xa7, length), tds::Type::Varchar(u16::MAX))
                }
                (family, _) => bail!("unsupported RETURNVALUE type {family:?}"),
            }
        }
        DataType::Binary(binary) if !binary.fixed() => match binary.length() {
            Length::Bounded(n) => {
                let mut info = vec![0xa5];
                info.extend(n.to_le_bytes());
                (info, tds::Type::Varbinary(n))
            }
            Length::Max => (vec![0xa5, 0xff, 0xff], tds::Type::Binary),
        },
        other => bail!("unsupported RETURNVALUE type {other:?}"),
    })
}

/// A value of the declared type, in its RETURNVALUE encoding.
fn encode(out: &mut Vec<u8>, kind: &tds::Type, value: &Value) -> Result<()> {
    use duckdb::types::Value as Backend;
    match (kind, value) {
        (tds::Type::Text | tds::Type::Nvarchar(_), Value::Unicode(units)) => {
            tds::unicode_value(out, kind, Some(units))
        }
        (tds::Type::Text | tds::Type::Nvarchar(_), Value::Text(text)) => {
            let units: Vec<u16> = text.encode_utf16().collect();
            tds::unicode_value(out, kind, Some(&units))
        }
        (tds::Type::Float(8), Value::Float(value)) => {
            crate::engine::encode_value(out, kind, &Backend::Double(f64::from(*value)))
        }
        (tds::Type::Float(4), Value::Double(value)) => {
            crate::engine::encode_value(out, kind, &Backend::Float(*value as f32))
        }
        (tds::Type::DateTime2(scale), Value::Timestamp(unit, value)) => {
            let nanos = i128::from(*value) * nanoseconds(*unit);
            let encoded = crate::datetime2::DateTime2::from_unix_nanos(nanos)?.encode(*scale)?;
            out.push(encoded.len() as u8);
            out.extend(encoded);
            Ok(())
        }
        (tds::Type::Time(scale), Value::Text(text)) => {
            let units = time_units(text, *scale)?;
            let width = match scale {
                0..=2 => 3,
                3..=4 => 4,
                _ => 5,
            };
            out.push(width as u8);
            out.extend(&units.to_le_bytes()[..width]);
            Ok(())
        }
        _ => crate::engine::encode_value(out, kind, &crate::backend_value::to_backend(value)?),
    }
}

fn nanoseconds(unit: msduck_core::value::TimeUnit) -> i128 {
    use msduck_core::value::TimeUnit;
    match unit {
        TimeUnit::Second => 1_000_000_000,
        TimeUnit::Millisecond => 1_000_000,
        TimeUnit::Microsecond => 1_000,
        TimeUnit::Nanosecond => 1,
    }
}

/// `hh:mm:ss[.fffffff]` as units of 10^-scale seconds.
fn time_units(text: &str, scale: u8) -> Result<u64> {
    let (clock, fraction) = text.split_once('.').unwrap_or((text, ""));
    let parts: Vec<u64> = clock.split(':').map(str::parse).collect::<Result<_, _>>()?;
    let [hours, minutes, seconds] = parts[..] else {
        bail!("invalid time value");
    };
    ensure!(
        hours < 24 && minutes < 60 && seconds < 60 && fraction.len() <= 7,
        "invalid time value"
    );
    let digits = format!("{fraction:0<7}");
    let ticks = (hours * 3600 + minutes * 60 + seconds) * 10_000_000 + digits.parse::<u64>()?;
    let quantum = 10u64.pow(u32::from(7 - scale));
    Ok(ticks / quantum)
}

/// One RETURNVALUE token for the request's parameter `ordinal`.
fn return_value(
    out: &mut Vec<u8>,
    ordinal: usize,
    parameter: &RpcParameter,
    value: &Value,
) -> Result<()> {
    let (info, kind) = declaration(parameter.kind, parameter.parameter.data_type)?;
    let name: Vec<u16> = parameter.name.encode_utf16().collect();
    ensure!(
        name.len() <= 255,
        "RETURNVALUE name exceeds 255 UTF-16 units"
    );
    let mut token = vec![0xac];
    token.extend(u16::try_from(ordinal)?.to_le_bytes());
    token.push(name.len() as u8);
    token.extend(name.iter().flat_map(|unit| unit.to_le_bytes()));
    // Status 1 (an OUTPUT parameter), user type 0 and flags 0, as captured.
    token.push(1);
    token.extend([0; 6]);
    token.extend(info);
    encode(&mut token, &kind, value)?;
    ensure!(
        out.len().saturating_add(token.len()) <= tds::MAX_MESSAGE,
        "RETURNVALUE would exceed message bound"
    );
    out.extend(token);
    Ok(())
}

fn argument(parameter: &RpcParameter) -> RpcArgument<'_> {
    RpcArgument {
        name: (!parameter.name.is_empty()).then_some(parameter.name.as_str()),
        value: &parameter.parameter,
        output: parameter.output(),
        default: parameter.status & 2 != 0,
    }
}

/// A parameter name without `@` names the same parameter; an empty name
/// stays positional.
fn at_name(name: &str) -> String {
    match name {
        "" => String::new(),
        name if name.starts_with('@') => name.to_owned(),
        name => format!("@{name}"),
    }
}

/// An RPC request naming procedure `name`.
pub(super) fn call(
    session: &mut Session,
    name: &str,
    parameters: &[RpcParameter],
) -> Result<Vec<u8>> {
    let names: Vec<String> = parameters
        .iter()
        .map(|parameter| at_name(&parameter.name))
        .collect();
    let arguments: Vec<RpcArgument<'_>> = parameters
        .iter()
        .zip(&names)
        .map(|(parameter, name)| RpcArgument {
            name: (!name.is_empty()).then_some(name.as_str()),
            ..argument(parameter)
        })
        .collect();
    let result = session.rpc_call(name, &arguments);
    let mut out = result.tokens;
    match result.outcome {
        RpcOutcome::Completed(status) => {
            return_status(&mut out, status);
            for (ordinal, (parameter, value)) in parameters.iter().zip(&result.outputs).enumerate()
            {
                if let Some(value) = value {
                    return_value(&mut out, ordinal, parameter, &value.value)?;
                }
            }
            tds::done(&mut out, DONE_PROC, 0, RPC_COMMAND, 0);
        }
        RpcOutcome::Failed { number, error } => {
            // Captured: a changed @@TRANCOUNT (266) follows the status.
            if number == 266 {
                return_status(&mut out, 0);
            }
            out.extend(error);
            tds::done(&mut out, DONE_PROC, 2, RPC_COMMAND, 0);
        }
        RpcOutcome::Aborted => tds::done(&mut out, DONE_PROC, 2, RPC_COMMAND, 0),
    }
    Ok(out)
}

/// sp_executesql (or a prepared statement) with OUTPUT parameters, run as
/// `EXEC sp_executesql`. `values` pairs each value parameter with its
/// ordinal in the request. A prepared statement's `handle` is returned
/// first. Returns the response and whether the batch ran to its end (an
/// aborted batch returns no handle).
pub(super) fn execute_sql(
    session: &mut Session,
    statement: &Parameter,
    declarations: &Parameter,
    values: &[(usize, &RpcParameter)],
    handle: Option<(&RpcParameter, i32)>,
) -> Result<(Vec<u8>, bool)> {
    let mut arguments = vec![
        RpcArgument {
            name: None,
            value: statement,
            output: false,
            default: false,
        },
        RpcArgument {
            name: None,
            value: declarations,
            output: false,
            default: false,
        },
    ];
    let names: Vec<String> = values
        .iter()
        .map(|(_, parameter)| at_name(&parameter.name))
        .collect();
    arguments.extend(
        values
            .iter()
            .zip(&names)
            .map(|((_, parameter), name)| RpcArgument {
                name: (!name.is_empty()).then_some(name.as_str()),
                ..argument(parameter)
            }),
    );
    let result = session.rpc_call("sp_executesql", &arguments);
    let mut out = result.tokens;
    let handle_value = |out: &mut Vec<u8>| -> Result<()> {
        if let Some((parameter, handle)) = handle {
            return_value(out, 0, parameter, &Value::Int(handle))?;
        }
        Ok(())
    };
    let outputs = values.iter().zip(&result.outputs[2..]);
    match result.outcome {
        RpcOutcome::Completed(status) => {
            return_status(&mut out, status);
            handle_value(&mut out)?;
            for ((ordinal, parameter), value) in outputs {
                if let Some(value) = value {
                    return_value(&mut out, *ordinal, parameter, &value.value)?;
                }
            }
            tds::done(&mut out, DONE_PROC, 0, RPC_COMMAND, 0);
            Ok((out, true))
        }
        RpcOutcome::Failed { number, error } => {
            // Captured: the status is the error number (1 for a missing
            // value, 8178), and OUTPUT parameters return the values they
            // were sent with.
            out.extend(error);
            return_status(&mut out, if number == 8178 { 1 } else { number });
            handle_value(&mut out)?;
            for (ordinal, parameter) in values {
                if parameter.output() {
                    return_value(&mut out, *ordinal, parameter, &parameter.parameter.value)?;
                }
            }
            tds::done(&mut out, DONE_PROC, 2, RPC_COMMAND, 0);
            Ok((out, true))
        }
        RpcOutcome::Aborted => {
            tds::done(&mut out, DONE_PROC, 2, RPC_COMMAND, 0);
            Ok((out, false))
        }
    }
}

/// Statement or declaration text for `execute_sql`, as nvarchar(max).
pub(super) fn text(text: Option<&str>) -> Parameter {
    Parameter {
        value: text.map_or(Value::Null, |text| Value::Text(text.to_owned())),
        data_type: DataType::Character(
            CharacterType::new(Family::Nvarchar, Length::Max).expect("nvarchar(max) is valid"),
        ),
    }
}

/// sp_execute or sp_unprepare naming an unknown handle (captured: 8179,
/// state 4 or 8, then RETURNSTATUS and the OUTPUT parameters' values).
pub(super) fn missing_handle(
    handle: i32,
    state: u8,
    values: &[(usize, &RpcParameter)],
) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    tds::sql_error(
        &mut out,
        &SqlError::new(
            8179,
            state,
            format!("Could not find prepared statement with handle {handle}."),
        ),
    );
    return_status(&mut out, 8179);
    for (ordinal, parameter) in values {
        if parameter.output() {
            return_value(&mut out, *ordinal, parameter, &parameter.parameter.value)?;
        }
    }
    tds::done(&mut out, DONE_PROC, 2, RPC_COMMAND, 0);
    Ok(out)
}

/// Add the prepared `handle` to an engine RPC response. SQL Server returns
/// it after RETURNSTATUS whenever the batch ran to its end, even after a
/// statement-terminating error such as a duplicate key. An aborted batch
/// returns no handle. Returns whether the batch ran to its end.
pub(super) fn with_handle(
    response: &mut Vec<u8>,
    parameter: &RpcParameter,
    handle: i32,
) -> Result<bool> {
    // The engine completes an RPC batch with RETURNSTATUS and DONEPROC
    // (status 0, CurCmd 224); every other ending carries the error flag.
    const COMPLETION: [u8; 13] = [DONE_PROC, 0, 0, 0xe0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    let length = response.len();
    if length < 18 || response[length - 13..] != COMPLETION || response[length - 18] != 0x79 {
        return Ok(false);
    }
    let done = response.split_off(length - 13);
    return_value(response, 0, parameter, &Value::Int(handle))?;
    response.extend(done);
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parameter(name: &str, kind: u8, data_type: DataType) -> RpcParameter {
        RpcParameter {
            name: name.into(),
            status: 1,
            kind,
            parameter: Parameter {
                value: Value::Null,
                data_type,
            },
        }
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    // Tokens captured from SQL Server 2022 (reference/gaps-rpc-procedures.json,
    // "output types" and "sql output types").
    #[test]
    fn return_values_match_captured_tokens() {
        let cases: Vec<(usize, &str, u8, DataType, Value, &str)> = vec![
            (
                0,
                "@i",
                0x26,
                DataType::Int,
                Value::Int(1),
                "ac000002400069000100000000000026040401000000",
            ),
            (
                1,
                "@bi",
                0x26,
                DataType::BigInt,
                Value::BigInt(2),
                "ac010003400062006900010000000000002608080200000000000000",
            ),
            (
                2,
                "@si",
                0x26,
                DataType::SmallInt,
                Value::SmallInt(3),
                "ac020003400073006900010000000000002602020300",
            ),
            (
                3,
                "@ti",
                0x26,
                DataType::TinyInt,
                Value::UTinyInt(4),
                "ac0300034000740069000100000000000026010104",
            ),
            (
                4,
                "@bt",
                0x68,
                DataType::Bit,
                Value::Boolean(true),
                "ac0400034000620074000100000000000068010101",
            ),
            (
                5,
                "@s",
                0xe7,
                DataType::Character(
                    CharacterType::new(Family::Nvarchar, Length::Bounded(10)).unwrap(),
                ),
                Value::Text("ab".into()),
                "ac0500024000730001000000000000e714000904d00034040061006200",
            ),
            (
                6,
                "@v",
                0xa7,
                DataType::Character(
                    CharacterType::new(Family::Varchar, Length::Bounded(5)).unwrap(),
                ),
                Value::Text("cd".into()),
                "ac0600024000760001000000000000a705000904d0003402006364",
            ),
            (
                7,
                "@d",
                0x6a,
                DataType::Decimal(msduck_core::types::DecimalType::new(5, 2).unwrap()),
                Value::Decimal(msduck_core::value::Decimal::new(5, 2, 125).unwrap()),
                "ac07000240006400010000000000006a11050205017d000000",
            ),
            (
                8,
                "@f",
                0x6d,
                DataType::Float,
                Value::Double(0.5),
                "ac08000240006600010000000000006d0808000000000000e03f",
            ),
            (
                9,
                "@r",
                0x6d,
                DataType::Real,
                Value::Float(0.25),
                "ac09000240007200010000000000006d04040000803e",
            ),
            (
                10,
                "@m",
                0x6e,
                DataType::Money,
                Value::Decimal(msduck_core::value::Decimal::new(19, 4, 25000).unwrap()),
                "ac0a000240006d00010000000000006e080800000000a8610000",
            ),
            (
                11,
                "@dt",
                0x2a,
                DataType::DateTime2(msduck_core::types::Scale::new(3).unwrap()),
                Value::Text("2024-01-02T03:04:05.678".into()),
                "ac0b0003400064007400010000000000002a03072e8ba80046460b",
            ),
            (
                12,
                "@da",
                0x28,
                DataType::Date,
                Value::Date32(19724),
                "ac0c000340006400610001000000000000280346460b",
            ),
            (
                13,
                "@g",
                0x24,
                DataType::UniqueIdentifier,
                Value::Text("00112233-4455-6677-8899-aabbccddeeff".into()),
                "ac0d0002400067000100000000000024101033221100554477668899aabbccddeeff",
            ),
            (
                14,
                "@b",
                0xa5,
                DataType::Binary(
                    msduck_core::types::BinaryType::new(false, Length::Bounded(4)).unwrap(),
                ),
                Value::Blob(vec![1, 2]),
                "ac0e00024000620001000000000000a5040002000102",
            ),
            (
                0,
                "@x",
                0x26,
                DataType::Int,
                Value::Null,
                "ac0000024000780001000000000000260400",
            ),
            (
                2,
                "",
                0x26,
                DataType::Int,
                Value::Int(3),
                "ac0200000100000000000026040403000000",
            ),
            (
                1,
                "@c",
                0xe7,
                DataType::Character(
                    CharacterType::new(Family::Nvarchar, Length::Bounded(4000)).unwrap(),
                ),
                Value::Text("6".into()),
                "ac0100024000630001000000000000e7401f0904d0003402003600",
            ),
        ];
        for (ordinal, name, kind, data_type, value, expected) in cases {
            let mut out = Vec::new();
            return_value(&mut out, ordinal, &parameter(name, kind, data_type), &value).unwrap();
            assert_eq!(hex(&out), expected, "{name}");
        }
    }

    #[test]
    fn handle_follows_status_only_after_a_completed_batch() {
        let handle = parameter("@handle", 0x26, DataType::Int);
        let mut completed = vec![0xaa, 0x79, 0x23, 0x0a, 0, 0];
        tds::done(&mut completed, DONE_PROC, 0, RPC_COMMAND, 0);
        assert!(with_handle(&mut completed, &handle, 1).unwrap());
        assert_eq!(
            hex(&completed),
            "aa79230a0000ac0000074000680061006e0064006c0065000100000000000026040401000000fe0000e0000000000000000000"
        );
        let mut aborted = vec![0xaa];
        tds::done(&mut aborted, DONE_PROC, 2, 0, 0);
        let before = aborted.clone();
        assert!(!with_handle(&mut aborted, &handle, 1).unwrap());
        assert_eq!(aborted, before);
    }

    #[test]
    fn time_text_scales() {
        assert_eq!(time_units("01:02:03.1234567", 7).unwrap(), 37_231_234_567);
        assert_eq!(time_units("01:02:03.1234567", 3).unwrap(), 3_723_123);
        assert_eq!(time_units("23:59:59", 0).unwrap(), 86_399);
        assert!(time_units("24:00:00", 0).is_err());
    }
}
