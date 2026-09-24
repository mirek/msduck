//! RPC dispatch and connection-owned prepared statement handles.
use crate::{
    engine::{self, Session},
    parameter::Parameter,
    tds::{self, Cursor},
};
use anyhow::{Result, bail, ensure};
use msduck_core::value::Value;
use msduck_core::{
    character::{CharacterType, Family, Length},
    types::{BinaryType, DecimalType, Scale, Type as DataType},
};
use std::collections::HashMap;

struct RpcParameter {
    name: String,
    status: u8,
    parameter: Parameter,
}
struct Prepared {
    sql: String,
    declarations: Vec<(String, DataType)>,
    bytes: usize,
}
#[derive(Default)]
pub struct State {
    prepared: HashMap<i32, Prepared>,
    next_handle: i32,
    bytes: usize,
}
impl State {
    pub fn execute(&mut self, session: &mut Session, data: &[u8]) -> Result<Vec<u8>> {
        let mut c = Cursor::new(tds::batch_body(data)?);
        let length = c.u16()?;
        let procedure = if length == 65535 {
            match c.u16()? {
                10 => "sp_executesql",
                11 => "sp_prepare",
                12 => "sp_execute",
                13 => "sp_prepexec",
                15 => "sp_unprepare",
                id => bail!("unsupported RPC procedure id {id}"),
            }
            .to_owned()
        } else {
            c.text(length as usize)?.to_lowercase()
        };
        ensure!(c.u16()? == 0, "unsupported RPC flags");
        let mut parameters = Vec::new();
        while c.remaining() > 0 {
            let n = c.u8()? as usize;
            let name = c.text(n)?.to_lowercase();
            let status = c.u8()?;
            ensure!(
                status & !1 == 0,
                "unsupported default/encrypted RPC parameter"
            );
            parameters.push(RpcParameter {
                name,
                status,
                parameter: rpc_value(&mut c)?,
            });
        }
        let procedure = procedure.strip_prefix("sys.").unwrap_or(&procedure);
        match procedure {
            "sp_executesql" => {
                let sql = input_text(parameters.first(), false)?;
                let declarations = declarations(parameters.get(1))?;
                let bindings = bind(&declarations, parameters.get(2..).unwrap_or_default())?;
                Ok(session.batch(sql, &bindings, true))
            }
            "sp_prepare" | "sp_prepexec" => {
                let output = parameters
                    .first()
                    .ok_or_else(|| anyhow::anyhow!("missing output handle"))?;
                ensure!(
                    output.status == 1 && matches!(output.parameter.data_type, DataType::Int),
                    "prepared handle must be an int OUTPUT parameter"
                );
                let declarations = declarations(parameters.get(1))?;
                let sql = input_text(parameters.get(2), false)?;
                let bindings = if procedure == "sp_prepare" {
                    ensure!(
                        (3..=4).contains(&parameters.len()),
                        "invalid sp_prepare parameter count"
                    );
                    if parameters.len() == 4 {
                        ensure!(
                            input_int(&parameters[3])? == 0,
                            "unsupported sp_prepare metadata option"
                        );
                    }
                    HashMap::new()
                } else {
                    bind(&declarations, &parameters[3..])?
                };
                // Validate both syntax and database binding without executing the statement.
                session.validate_prepared_sql(sql, &declarations)?;
                let bytes = sql.len()
                    + declarations
                        .iter()
                        .map(|(name, kind)| name.len() + std::mem::size_of_val(kind))
                        .sum::<usize>();
                ensure!(
                    self.prepared.len() < 1024 && self.bytes + bytes <= tds::MAX_MESSAGE,
                    "prepared statement capacity exceeded"
                );
                let handle = self
                    .next_handle
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("prepared statement handle space exhausted"))?;
                self.next_handle = handle;
                let (response, success) = session.batch_response(
                    if procedure == "sp_prepare" { "" } else { sql },
                    &bindings,
                    true,
                    Some((&output.name, handle)),
                );
                if success {
                    self.bytes += bytes;
                    self.prepared.insert(
                        handle,
                        Prepared {
                            sql: sql.to_owned(),
                            declarations,
                            bytes,
                        },
                    );
                }
                Ok(response)
            }
            "sp_execute" => {
                let handle = input_int(
                    parameters
                        .first()
                        .ok_or_else(|| anyhow::anyhow!("missing prepared handle"))?,
                )?;
                let prepared = self.prepared.get(&handle).ok_or_else(|| {
                    anyhow::anyhow!("Could not find prepared statement with handle {handle}.")
                })?;
                let bindings = bind(&prepared.declarations, &parameters[1..])?;
                Ok(session.prepared_batch(&prepared.sql, &bindings))
            }
            "sp_unprepare" => {
                ensure!(parameters.len() == 1, "sp_unprepare requires one handle");
                let handle = input_int(&parameters[0])?;
                let prepared = self.prepared.remove(&handle).ok_or_else(|| {
                    anyhow::anyhow!("Could not find prepared statement with handle {handle}.")
                })?;
                self.bytes -= prepared.bytes;
                Ok(session.batch("", &HashMap::new(), true))
            }
            _ => bail!("unsupported RPC procedure {procedure}"),
        }
    }
}
fn input_text(parameter: Option<&RpcParameter>, nullable: bool) -> Result<&str> {
    let Some(parameter) = parameter else {
        ensure!(nullable, "missing SQL text");
        return Ok("");
    };
    ensure!(
        parameter.status == 0,
        "unsupported output SQL text parameter"
    );
    match &parameter.parameter.value {
        Value::Text(text) => Ok(text),
        Value::Null if nullable => Ok(""),
        _ => bail!("RPC requires Unicode text"),
    }
}
fn input_int(parameter: &RpcParameter) -> Result<i32> {
    ensure!(
        parameter.status == 0,
        "unsupported output integer parameter"
    );
    match parameter.parameter.value {
        Value::Int(value) => Ok(value),
        _ => bail!("RPC requires an int parameter"),
    }
}
fn declarations(parameter: Option<&RpcParameter>) -> Result<Vec<(String, DataType)>> {
    let result = engine::parameter_declarations(input_text(parameter, true)?)?;
    let mut seen = std::collections::HashSet::new();
    for (name, _) in &result {
        ensure!(seen.insert(name), "duplicate parameter declaration {name}");
    }
    Ok(result)
}
fn bind(
    declarations: &[(String, DataType)],
    parameters: &[RpcParameter],
) -> Result<HashMap<String, Parameter>> {
    ensure!(
        parameters.len() == declarations.len(),
        "RPC parameter count does not match declaration"
    );
    let mut bindings = HashMap::new();
    for (index, parameter) in parameters.iter().enumerate() {
        ensure!(parameter.status == 0, "unsupported output value parameter");
        let name = if parameter.name.is_empty() {
            declarations[index].0.clone()
        } else if parameter.name.starts_with('@') {
            parameter.name.clone()
        } else {
            format!("@{}", parameter.name)
        };
        let (_, data_type) = declarations
            .iter()
            .find(|(declared, _)| declared == &name)
            .ok_or_else(|| anyhow::anyhow!("RPC parameter is not declared: {name}"))?;
        ensure!(
            bindings
                .insert(
                    name,
                    Parameter {
                        value: parameter.parameter.value.clone(),
                        data_type: *data_type
                    }
                )
                .is_none(),
            "duplicate RPC parameter"
        );
    }
    Ok(bindings)
}
fn rpc_value(c: &mut Cursor<'_>) -> Result<Parameter> {
    let kind = c.u8()?;
    match kind {
        0x1f => Ok(Parameter {
            value: Value::Null,
            data_type: DataType::Int,
        }),
        0x24 => {
            ensure!(c.u8()? == 16, "invalid uniqueidentifier metadata width");
            let len = c.u8()?;
            let value = if len == 0 {
                Value::Null
            } else {
                ensure!(len == 16, "invalid uniqueidentifier value width");
                Value::Text(uuid::Uuid::from_bytes_le(c.take(16)?.try_into()?).to_string())
            };
            Ok(Parameter {
                value,
                data_type: DataType::UniqueIdentifier,
            })
        }
        0x26 | 0x68 | 0x6d => {
            let max = c.u8()?;
            let len = c.u8()?;
            ensure!(
                match kind {
                    0x26 => matches!(max, 1 | 2 | 4 | 8),
                    0x68 => max == 1,
                    _ => matches!(max, 4 | 8),
                },
                "invalid RPC type width"
            );
            let data_type = match (kind, max) {
                (0x26, 1) => DataType::TinyInt,
                (0x26, 2) => DataType::SmallInt,
                (0x26, 4) => DataType::Int,
                (0x26, 8) => DataType::BigInt,
                (0x68, _) => DataType::Bit,
                (0x6d, 4) => DataType::Real,
                _ => DataType::Float,
            };
            if len == 0 {
                return Ok(Parameter {
                    value: Value::Null,
                    data_type,
                });
            }
            ensure!(len == max, "invalid RPC value width");
            let bytes = c.take(len as usize)?;
            let value = match (kind, len) {
                (0x26, 1) => Value::UTinyInt(bytes[0]),
                (0x26, 2) => Value::SmallInt(i16::from_le_bytes(bytes.try_into()?)),
                (0x26, 4) => Value::Int(i32::from_le_bytes(bytes.try_into()?)),
                (0x26, 8) => Value::BigInt(i64::from_le_bytes(bytes.try_into()?)),
                (0x68, 1) => Value::Boolean(bytes[0] != 0),
                (0x6d, 4) => Value::Float(f32::from_le_bytes(bytes.try_into()?)),
                (0x6d, 8) => Value::Double(f64::from_le_bytes(bytes.try_into()?)),
                _ => bail!("invalid RPC type"),
            };
            Ok(Parameter { value, data_type })
        }
        0x6f | 0x3a | 0x3d => {
            let max = if kind == 0x6f {
                c.u8()?
            } else if kind == 0x3a {
                4
            } else {
                8
            };
            ensure!(matches!(max, 4 | 8), "invalid datetime metadata width");
            let len = if kind == 0x6f { c.u8()? } else { max };
            let data_type = if max == 4 {
                DataType::SmallDateTime
            } else {
                DataType::DateTime
            };
            if len == 0 {
                return Ok(Parameter {
                    value: Value::Null,
                    data_type,
                });
            }
            ensure!(len == max, "invalid datetime value width");
            let bytes = c.take(len as usize)?;
            let (days, micros) = if len == 4 {
                let days = u16::from_le_bytes(bytes[..2].try_into()?) as i64;
                let minutes = u16::from_le_bytes(bytes[2..].try_into()?) as i64;
                ensure!(minutes < 1440, "smalldatetime outside SQL Server range");
                (days, minutes * 60_000_000)
            } else {
                let days = i32::from_le_bytes(bytes[..4].try_into()?) as i64;
                let ticks = u32::from_le_bytes(bytes[4..].try_into()?) as i64;
                ensure!(
                    (-53_690..=2_958_463).contains(&days) && ticks < 25_920_000,
                    "datetime outside SQL Server range"
                );
                // DuckDB TIMESTAMP currently stores microseconds. Round the
                // rational 1/300-second interval to its nearest microsecond.
                (days, (ticks * 1_000_000 + 150) / 300)
            };
            Ok(Parameter {
                value: Value::Timestamp(
                    msduck_core::value::TimeUnit::Microsecond,
                    (days - 25_567) * 86_400_000_000 + micros,
                ),
                data_type,
            })
        }
        0x2b => {
            let scale = c.u8()?;
            ensure!(scale <= 7, "invalid DATETIMEOFFSET scale");
            let len = c.u8()?;
            let value = if len == 0 {
                Value::Null
            } else {
                let value =
                    crate::datetimeoffset::DateTimeOffset::decode(c.take(len as usize)?, scale)?;
                Value::Text(value.format_iso(scale)?)
            };
            Ok(Parameter {
                value,
                data_type: DataType::DateTimeOffset(Scale::new(scale)?),
            })
        }
        0x2a => {
            let scale = c.u8()?;
            ensure!(scale <= 7, "invalid DATETIME2 scale");
            let len = c.u8()?;
            let value = if len == 0 {
                Value::Null
            } else {
                let value = crate::datetime2::DateTime2::decode(c.take(len as usize)?, scale)?;
                Value::Text(value.format_iso(scale)?)
            };
            Ok(Parameter {
                value,
                data_type: DataType::DateTime2(Scale::new(scale)?),
            })
        }
        0x29 => {
            let scale = c.u8()?;
            ensure!(scale <= 7, "invalid time scale");
            let len = c.u8()?;
            let width = match scale {
                0..=2 => 3,
                3..=4 => 4,
                _ => 5,
            };
            let value = if len == 0 {
                Value::Null
            } else {
                ensure!(len == width, "invalid time value width");
                let mut bytes = [0u8; 8];
                bytes[..len as usize].copy_from_slice(c.take(len as usize)?);
                let units = u64::from_le_bytes(bytes);
                let per_second = 10u64.pow(scale as u32);
                ensure!(units < 86400 * per_second, "time outside SQL Server range");
                let seconds = units / per_second;
                let fraction = (units % per_second) * 10u64.pow(7 - scale as u32);
                // DuckDB's Rust Time64 binder truncates to microseconds. Bind
                // text and cast to TIME_NS to retain every 100ns digit.
                Value::Text(format!(
                    "{:02}:{:02}:{:02}.{fraction:07}",
                    seconds / 3600,
                    seconds / 60 % 60,
                    seconds % 60,
                ))
            };
            Ok(Parameter {
                value,
                data_type: DataType::Time(Scale::new(scale)?),
            })
        }
        0x28 => {
            let len = c.u8()?;
            let value = if len == 0 {
                Value::Null
            } else {
                ensure!(len == 3, "invalid date value width");
                let mut bytes = [0u8; 4];
                bytes[..3].copy_from_slice(c.take(3)?);
                let days = u32::from_le_bytes(bytes);
                ensure!(days <= 3_652_058, "date outside SQL Server range");
                Value::Date32(days as i32 - 719_162)
            };
            Ok(Parameter {
                value,
                data_type: DataType::Date,
            })
        }
        0x6a | 0x6c => {
            let max = c.u8()?;
            let precision = c.u8()?;
            let scale = c.u8()?;
            ensure!(
                (1..=38).contains(&precision) && scale <= precision,
                "invalid decimal precision or scale"
            );
            ensure!(
                max == tds::decimal_length(precision),
                "invalid decimal type width"
            );
            let len = c.u8()?;
            let data_type = DataType::Decimal(DecimalType::new(precision, scale)?);
            if len == 0 {
                return Ok(Parameter {
                    value: Value::Null,
                    data_type,
                });
            }
            ensure!(
                matches!(len, 5 | 9 | 13 | 17) && len <= max,
                "invalid decimal value width"
            );
            let sign = c.u8()?;
            ensure!(sign <= 1, "invalid decimal sign");
            let mut bytes = [0u8; 16];
            bytes[..len as usize - 1].copy_from_slice(c.take(len as usize - 1)?);
            let magnitude = u128::from_le_bytes(bytes);
            ensure!(
                magnitude < 10u128.pow(precision as u32),
                "decimal value exceeds declared precision"
            );
            let scaled = if sign == 0 {
                -(magnitude as i128)
            } else {
                magnitude as i128
            };
            Ok(Parameter {
                value: Value::Decimal(msduck_core::value::Decimal::new(precision, scale, scaled)?),
                data_type,
            })
        }
        0x6e => {
            let max = c.u8()?;
            ensure!(matches!(max, 4 | 8), "invalid money type width");
            let len = c.u8()?;
            let precision = if max == 4 { 10 } else { 19 };
            let data_type = if max == 4 {
                DataType::SmallMoney
            } else {
                DataType::Money
            };
            if len == 0 {
                return Ok(Parameter {
                    value: Value::Null,
                    data_type,
                });
            }
            ensure!(len == max, "invalid money value width");
            let scaled = if max == 4 {
                c.u32()? as i32 as i64
            } else {
                // MONEY is signed high int32 followed by unsigned low uint32.
                let high = c.u32()? as i32 as i64;
                let low = c.u32()? as i64;
                (high << 32) | low
            };
            Ok(Parameter {
                value: Value::Decimal(msduck_core::value::Decimal::new(
                    precision as u8,
                    4,
                    scaled as i128,
                )?),
                data_type,
            })
        }
        0xe7 | 0xa7 | 0xa5 => {
            let max = c.u16()?;
            // A zero-width wire value can carry an empty string/binary.
            // Its logical declaration has the minimum length of one; payload
            // bounds below still use the original wire maximum.
            let data_type = if kind == 0xa5 {
                DataType::Binary(BinaryType::new(
                    false,
                    if max == 65535 {
                        Length::Max
                    } else {
                        Length::Bounded(max.max(1))
                    },
                )?)
            } else {
                DataType::Character(CharacterType::new(
                    if kind == 0xe7 {
                        Family::Nvarchar
                    } else {
                        Family::Varchar
                    },
                    if max == 65535 {
                        Length::Max
                    } else {
                        Length::Bounded((if kind == 0xe7 { max / 2 } else { max }).max(1))
                    },
                )?)
            };
            if kind != 0xa5 {
                let collation = c.take(5)?;
                ensure!(
                    kind == 0xe7 || collation == tds::COLLATION,
                    "unsupported varchar RPC collation"
                );
            }
            ensure!(
                max == 65535 || max <= 8000,
                "invalid character/binary type width"
            );
            ensure!(
                kind != 0xe7 || max == 65535 || max.is_multiple_of(2),
                "invalid Unicode type width"
            );
            let bytes = if max == 65535 {
                let total = c.u64()?;
                if total == u64::MAX {
                    return Ok(Parameter {
                        value: Value::Null,
                        data_type,
                    });
                }
                ensure!(
                    total == u64::MAX - 1 || total <= tds::MAX_MESSAGE as u64,
                    "PLP value too large"
                );
                let mut bytes = vec![];
                loop {
                    let size = c.u32()? as usize;
                    if size == 0 {
                        break;
                    }
                    ensure!(
                        bytes.len() + size <= tds::MAX_MESSAGE,
                        "PLP value too large"
                    );
                    bytes.extend(c.take(size)?);
                }
                ensure!(
                    total == u64::MAX - 1 || total == bytes.len() as u64,
                    "PLP length mismatch"
                );
                bytes
            } else {
                let len = c.u16()?;
                if len == 65535 {
                    return Ok(Parameter {
                        value: Value::Null,
                        data_type,
                    });
                }
                ensure!(len <= max, "RPC value exceeds type width");
                c.take(len as usize)?.to_vec()
            };
            if kind == 0xe7 {
                Ok(Parameter {
                    value: {
                        ensure!(bytes.len() % 2 == 0, "odd UTF-16 RPC byte length");
                        Value::from_utf16(
                            bytes
                                .chunks_exact(2)
                                .map(|b| u16::from_le_bytes([b[0], b[1]]))
                                .collect(),
                        )
                    },
                    data_type,
                })
            } else if kind == 0xa7 {
                Ok(Parameter {
                    value: Value::Text(tds::decode_cp1252(&bytes)),
                    data_type,
                })
            } else {
                Ok(Parameter {
                    value: Value::Blob(bytes),
                    data_type,
                })
            }
        }
        _ => bail!("unsupported RPC parameter type 0x{kind:02x}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::Server;
    fn text_parameter(out: &mut Vec<u8>, name: &str, value: &str) {
        out.push(name.encode_utf16().count() as u8);
        out.extend(tds::text(name));
        out.extend([0, 0xe7]);
        let value = tds::text(value);
        out.extend((value.len() as u16).to_le_bytes());
        out.extend(tds::COLLATION);
        out.extend((value.len() as u16).to_le_bytes());
        out.extend(value);
    }
    fn prepare_request(sql: &str, declarations: &str) -> Vec<u8> {
        let mut out = vec![4, 0, 0, 0, 255, 255, 11, 0, 0, 0];
        // unnamed int OUTPUT handle, NULL value
        out.extend([0, 1, 0x26, 4, 0]);
        text_parameter(&mut out, "", declarations);
        text_parameter(&mut out, "", sql);
        out
    }
    fn handle_request(operation: u8, handle: i32) -> Vec<u8> {
        let mut out = vec![4, 0, 0, 0, 255, 255, operation, 0, 0, 0, 0, 0, 0x26, 4, 4];
        out.extend(handle.to_le_bytes());
        out
    }
    #[test]
    fn preparation_validates_without_running_sql_and_release_reclaims_storage() {
        let server = Server::open(":memory:").unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        session
            .db
            .execute_batch("CREATE TABLE dbo.items (id INT)")
            .unwrap();
        let mut state = State::default();
        state
            .execute(
                &mut session,
                &prepare_request("INSERT INTO dbo.items VALUES (@x)", "@x int"),
            )
            .unwrap();
        assert_eq!(state.prepared.len(), 1);
        assert!(state.bytes > 0);
        assert_eq!(
            session
                .db
                .query_row("SELECT COUNT(*) FROM dbo.items", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        state.execute(&mut session, &handle_request(15, 1)).unwrap();
        assert_eq!(state.bytes, 0);
        assert!(state.prepared.is_empty());
        assert!(
            state
                .execute(&mut session, &handle_request(12, 1))
                .unwrap_err()
                .to_string()
                .contains("Could not find prepared statement")
        );
    }
    #[test]
    fn malformed_or_invalid_preparation_allocates_no_handle() {
        let server = Server::open(":memory:").unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        let mut state = State::default();
        let valid = prepare_request("SELECT @x", "@x int");
        for length in 0..valid.len() {
            assert!(state.execute(&mut session, &valid[..length]).is_err());
            assert!(state.prepared.is_empty());
        }
        for request in [
            prepare_request("SELECT @missing", "@x int"),
            prepare_request("SELECT @x", "@x int, @X int"),
            prepare_request("SELECT * FROM missing_table", ""),
        ] {
            assert!(state.execute(&mut session, &request).is_err());
            assert!(state.prepared.is_empty());
        }
        assert_eq!(state.bytes, 0);
        state.execute(&mut session, &valid).unwrap();
        assert!(state.prepared.contains_key(&1));
    }
    #[test]
    fn failed_prepexec_does_not_leak_a_handle() {
        let server = Server::open(":memory:").unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        session
            .db
            .execute_batch(
                "CREATE TABLE dbo.items(id INT PRIMARY KEY); INSERT INTO dbo.items VALUES (1)",
            )
            .unwrap();
        let mut state = State::default();
        let mut request = prepare_request("INSERT INTO dbo.items VALUES (1)", "");
        request[6] = 13;
        let response = state.execute(&mut session, &request).unwrap();
        assert_eq!(
            &response[response.len() - 13..response.len() - 10],
            &[0xfe, 2, 0]
        );
        assert!(state.prepared.is_empty());
        assert_eq!(state.bytes, 0);
        assert_eq!(
            session
                .db
                .query_row("SELECT COUNT(*) FROM dbo.items", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
    #[test]
    fn handle_space_does_not_wrap_or_reuse_freed_handles() {
        let server = Server::open(":memory:").unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        let mut state = State {
            next_handle: i32::MAX,
            ..State::default()
        };
        assert!(
            state
                .execute(&mut session, &prepare_request("SELECT 1", ""))
                .is_err()
        );
        assert!(state.prepared.is_empty());
    }
    #[test]
    fn return_handle_matches_int_output_wire_layout() {
        let mut out = Vec::new();
        tds::return_handle(&mut out, "", 42);
        assert_eq!(
            out,
            [0xac, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0x26, 4, 4, 42, 0, 0, 0]
        );
    }
}

#[cfg(test)]
mod numeric_tests {
    use super::*;
    fn decimal(bytes: &[u8]) -> msduck_core::value::Decimal {
        let parameter = rpc_value(&mut Cursor::new(bytes)).unwrap();
        let Value::Decimal(value) = parameter.value else {
            panic!("expected decimal")
        };
        value
    }
    #[test]
    fn decimal_sign_magnitude_vectors_and_short_values() {
        // DECIMAL(5,2): length 5, sign 0, magnitude 12345 little-endian.
        let value = decimal(&[0x6a, 5, 5, 2, 5, 0, 0x39, 0x30, 0, 0]);
        assert_eq!(value.to_string(), "-123.45");
        // MS-TDS permits shorter value storage than the declared maximum.
        let value = decimal(&[0x6c, 17, 38, 0, 5, 1, 42, 0, 0, 0]);
        assert_eq!(value.coefficient(), 42);
        assert_eq!(value.precision(), 38);
        for precision in [9, 10, 19, 20, 28, 29, 38] {
            let magnitude = 10u128.pow(precision as u32) - 1;
            let width = tds::decimal_length(precision);
            let mut bytes = vec![0x6a, width, precision, 0, width, 0];
            bytes.extend(&magnitude.to_le_bytes()[..width as usize - 1]);
            assert_eq!(decimal(&bytes).coefficient(), -(magnitude as i128));
        }
        assert_eq!(decimal(&[0x6a, 5, 1, 0, 5, 0, 0, 0, 0, 0]).coefficient(), 0);
    }
    #[test]
    fn reject_bad_precision_scale_sign_width_overflow_and_truncation() {
        for bytes in [
            &[0x6a, 5, 0, 0, 0][..],
            &[0x6a, 17, 39, 0, 0],
            &[0x6a, 5, 2, 3, 0],
            &[0x6a, 9, 2, 0, 0],
            &[0x6a, 5, 2, 0, 4, 1, 1, 0, 0],
            &[0x6a, 5, 2, 0, 5, 2, 1, 0, 0, 0],
            &[0x6a, 5, 2, 0, 5, 1, 100, 0, 0, 0],
            &[0x6e, 5, 0],
            &[0x6e, 8, 4, 0, 0, 0, 0],
        ] {
            assert!(rpc_value(&mut Cursor::new(bytes)).is_err(), "{bytes:?}");
        }
        let valid = [0x6a, 5, 5, 2, 5, 1, 0x39, 0x30, 0, 0];
        for len in 0..valid.len() {
            assert!(rpc_value(&mut Cursor::new(&valid[..len])).is_err());
        }
        let parameter = rpc_value(&mut Cursor::new(&[0x6c, 17, 38, 38, 0])).unwrap();
        assert_eq!(parameter.value, Value::Null);
        assert_eq!(parameter.ast_type().to_string(), "DECIMAL(38,38)");
    }
    #[test]
    fn money_word_order_and_signed_limits() {
        // High word first: 1 * 2^32 + 2, scaled by 10000.
        assert_eq!(
            decimal(&[0x6e, 8, 8, 1, 0, 0, 0, 2, 0, 0, 0]).coefficient(),
            4294967298
        );
        assert_eq!(
            decimal(&[0x6e, 8, 8, 0, 0, 0, 0x80, 0, 0, 0, 0]).coefficient(),
            i64::MIN as i128
        );
        assert_eq!(
            decimal(&[0x6e, 8, 8, 255, 255, 255, 0x7f, 255, 255, 255, 255]).coefficient(),
            i64::MAX as i128
        );
        assert_eq!(
            decimal(&[0x6e, 4, 4, 0, 0, 0, 0x80]).to_string(),
            "-214748.3648"
        );
        assert_eq!(
            decimal(&[0x6e, 4, 4, 255, 255, 255, 0x7f]).to_string(),
            "214748.3647"
        );
    }
}

#[cfg(test)]
mod character_tests {
    use super::*;
    fn value(bytes: &[u8]) -> String {
        match rpc_value(&mut Cursor::new(bytes)).unwrap().value {
            Value::Text(text) => text,
            _ => panic!("expected text"),
        }
    }
    fn prefix(max: u16) -> Vec<u8> {
        let mut bytes = vec![0xa7];
        bytes.extend(max.to_le_bytes());
        bytes.extend(tds::COLLATION);
        bytes
    }
    #[test]
    fn cp1252_punctuation_controls_and_empty_values() {
        let mut bytes = prefix(6);
        bytes.extend(6u16.to_le_bytes());
        bytes.extend([0x80, 0x93, 0x94, 0x97, 0xe9, 0x81]);
        assert_eq!(value(&bytes), "€“”—é\u{81}");
        let mut empty = prefix(0);
        empty.extend([0, 0]);
        assert_eq!(value(&empty), "");
        let mut null = prefix(1);
        null.extend([255, 255]);
        assert_eq!(
            rpc_value(&mut Cursor::new(&null)).unwrap().value,
            Value::Null
        );
        assert_eq!(
            tds::decode_cp1252(&[0, 0x7f, 0x9f, 0xa0, 0xff]),
            "\0\u{7f}Ÿ\u{a0}ÿ"
        );
    }
    #[test]
    fn varchar_plp_chunks_and_lengths() {
        for total in [3, u64::MAX - 1] {
            let mut bytes = prefix(65535);
            bytes.extend(total.to_le_bytes());
            bytes.extend(1u32.to_le_bytes());
            bytes.push(0x80);
            bytes.extend(2u32.to_le_bytes());
            bytes.extend([b'a', 0xe9]);
            bytes.extend(0u32.to_le_bytes());
            assert_eq!(value(&bytes), "€aé");
            for len in 0..bytes.len() {
                assert!(rpc_value(&mut Cursor::new(&bytes[..len])).is_err());
            }
        }
        let mut wrong = prefix(65535);
        wrong.extend(1u64.to_le_bytes());
        wrong.extend(0u32.to_le_bytes());
        assert!(rpc_value(&mut Cursor::new(&wrong)).is_err());
    }
    #[test]
    fn reject_unknown_collation_and_invalid_character_lengths() {
        let mut unknown = prefix(1);
        unknown[7] = 0;
        unknown.extend([1, 0, b'a']);
        assert!(
            rpc_value(&mut Cursor::new(&unknown))
                .unwrap_err()
                .to_string()
                .contains("unsupported varchar RPC collation")
        );
        let mut too_long = prefix(1);
        too_long.extend([2, 0, b'a', b'b']);
        assert!(rpc_value(&mut Cursor::new(&too_long)).is_err());
        let mut unicode = prefix(3);
        unicode[0] = 0xe7;
        unicode.extend([0, 0]);
        assert!(rpc_value(&mut Cursor::new(&unicode)).is_err());
    }
}

#[cfg(test)]
mod date_tests {
    use super::*;

    #[test]
    fn date_boundaries_epoch_and_typed_null() {
        for days in [0u32, 719_162, 730_178, 3_652_058] {
            let mut bytes = vec![0x28, 3];
            bytes.extend(&days.to_le_bytes()[..3]);
            let parameter = rpc_value(&mut Cursor::new(&bytes)).unwrap();
            assert_eq!(parameter.value, Value::Date32(days as i32 - 719_162));
            assert_eq!(parameter.data_type, DataType::Date);
            for length in 0..bytes.len() {
                assert!(rpc_value(&mut Cursor::new(&bytes[..length])).is_err());
            }
        }
        let null = rpc_value(&mut Cursor::new(&[0x28, 0])).unwrap();
        assert_eq!(null.value, Value::Null);
        assert_eq!(null.data_type, DataType::Date);
    }

    #[test]
    fn date_rejects_invalid_widths_and_out_of_range_days() {
        for length in [1, 2, 4, 255] {
            assert!(rpc_value(&mut Cursor::new(&[0x28, length, 0, 0, 0, 0])).is_err());
        }
        for days in [3_652_059u32, 0xff_ffff] {
            let mut bytes = vec![0x28, 3];
            bytes.extend(&days.to_le_bytes()[..3]);
            assert!(rpc_value(&mut Cursor::new(&bytes)).is_err());
        }
    }
}

#[cfg(test)]
mod time_tests {
    use super::*;

    fn wire(scale: u8, units: u64) -> Vec<u8> {
        let width = match scale {
            0..=2 => 3,
            3..=4 => 4,
            _ => 5,
        };
        let mut bytes = vec![0x29, scale, width];
        bytes.extend(&units.to_le_bytes()[..width as usize]);
        bytes
    }

    #[test]
    fn every_time_scale_boundary_and_truncation() {
        for scale in 0..=7 {
            let per_second = 10u64.pow(scale as u32);
            for units in [0, 1, 86400 * per_second - 1] {
                let bytes = wire(scale, units);
                let parameter = rpc_value(&mut Cursor::new(&bytes)).unwrap();
                let Value::Text(text) = parameter.value else {
                    panic!("expected exact text")
                };
                let fraction = if units == 0 {
                    0
                } else if units == 1 {
                    10u64.pow(7 - scale as u32) % 10_000_000
                } else {
                    10_000_000 - 10u64.pow(7 - scale as u32)
                };
                assert!(text.ends_with(&format!(".{fraction:07}")), "{text}");
                for length in 0..bytes.len() {
                    assert!(rpc_value(&mut Cursor::new(&bytes[..length])).is_err());
                }
            }
            assert!(rpc_value(&mut Cursor::new(&wire(scale, 86400 * per_second))).is_err());
            let null = rpc_value(&mut Cursor::new(&[0x29, scale, 0])).unwrap();
            assert_eq!(null.value, Value::Null);
            assert_eq!(null.ast_type().to_string(), format!("TIME({scale})"));
        }
    }

    #[test]
    fn time_rejects_invalid_scale_and_width() {
        for bytes in [
            &[0x29, 8, 0][..],
            &[0x29, 255, 0],
            &[0x29, 7, 3, 0, 0, 0],
            &[0x29, 0, 5, 0, 0, 0, 0, 0],
        ] {
            assert!(rpc_value(&mut Cursor::new(bytes)).is_err());
        }
    }
}

#[cfg(test)]
mod legacy_datetime_tests {
    use super::*;
    fn datetime(days: i32, ticks: u32) -> Vec<u8> {
        let mut bytes = vec![0x6f, 8, 8];
        bytes.extend(days.to_le_bytes());
        bytes.extend(ticks.to_le_bytes());
        bytes
    }
    #[test]
    fn legacy_datetime_boundaries_ticks_nulls_and_truncation() {
        for days in [-53_690, -1, 0, 25_567, 2_958_463] {
            for ticks in [0, 1, 2, 25_919_999] {
                let bytes = datetime(days, ticks);
                let value = rpc_value(&mut Cursor::new(&bytes)).unwrap().value;
                assert_eq!(
                    value,
                    Value::Timestamp(
                        msduck_core::value::TimeUnit::Microsecond,
                        (days as i64 - 25_567) * 86_400_000_000
                            + (ticks as i64 * 1_000_000 + 150) / 300
                    )
                );
                for length in 0..bytes.len() {
                    assert!(rpc_value(&mut Cursor::new(&bytes[..length])).is_err());
                }
                let mut fixed = vec![0x3d];
                fixed.extend(&bytes[3..]);
                assert_eq!(rpc_value(&mut Cursor::new(&fixed)).unwrap().value, value);
            }
        }
        for width in [4, 8] {
            assert_eq!(
                rpc_value(&mut Cursor::new(&[0x6f, width, 0]))
                    .unwrap()
                    .value,
                Value::Null
            );
        }
        for bytes in [
            datetime(-53_691, 0),
            datetime(2_958_464, 0),
            datetime(0, 25_920_000),
            vec![0x6f, 3, 0],
            vec![0x6f, 8, 4, 0, 0, 0, 0],
            vec![0x6f, 4, 4, 0, 0, 0xa0, 5],
        ] {
            assert!(rpc_value(&mut Cursor::new(&bytes)).is_err());
        }
    }
    #[test]
    fn smalldatetime_unsigned_days_and_minutes() {
        for days in [0u16, 65535] {
            for minutes in [0u16, 1439] {
                let mut bytes = vec![0x6f, 4, 4];
                bytes.extend(days.to_le_bytes());
                bytes.extend(minutes.to_le_bytes());
                let value = rpc_value(&mut Cursor::new(&bytes)).unwrap().value;
                assert_eq!(
                    value,
                    Value::Timestamp(
                        msduck_core::value::TimeUnit::Microsecond,
                        (days as i64 - 25_567) * 86_400_000_000 + minutes as i64 * 60_000_000
                    )
                );
                for length in 0..bytes.len() {
                    assert!(rpc_value(&mut Cursor::new(&bytes[..length])).is_err());
                }
                let mut fixed = vec![0x3a];
                fixed.extend(&bytes[3..]);
                assert_eq!(rpc_value(&mut Cursor::new(&fixed)).unwrap().value, value);
            }
        }
    }
}

#[cfg(test)]
mod guid_tests {
    use super::*;
    #[test]
    fn guid_mixed_endian_vector_null_and_malformed_values() {
        let bytes = [
            0x24, 16, 16, 0x33, 0x22, 0x11, 0x00, 0x55, 0x44, 0x77, 0x66, 0x88, 0x99, 0xaa, 0xbb,
            0xcc, 0xdd, 0xee, 0xff,
        ];
        let parameter = rpc_value(&mut Cursor::new(&bytes)).unwrap();
        assert_eq!(
            parameter.value,
            Value::Text("00112233-4455-6677-8899-aabbccddeeff".into())
        );
        let mut encoded = Vec::new();
        engine::encode_value(
            &mut encoded,
            &tds::Type::Guid,
            &crate::backend_value::to_backend(&parameter.value).unwrap(),
        )
        .unwrap();
        assert_eq!(encoded, bytes[2..]);
        for length in 0..bytes.len() {
            assert!(rpc_value(&mut Cursor::new(&bytes[..length])).is_err());
        }
        let null = rpc_value(&mut Cursor::new(&[0x24, 16, 0])).unwrap();
        assert_eq!(null.value, Value::Null);
        assert_eq!(null.data_type, DataType::UniqueIdentifier);
        for bytes in [[0x24, 0, 0], [0x24, 15, 0], [0x24, 16, 15], [0x24, 16, 17]] {
            assert!(rpc_value(&mut Cursor::new(&bytes)).is_err());
        }
    }
}

#[cfg(test)]
mod datetime2_tests {
    use super::*;
    #[test]
    fn exact_datetime2_payloads_and_malformed_metadata() {
        let value = crate::datetime2::DateTime2::parse_iso("9999-12-31T23:59:59.1234567").unwrap();
        for scale in 0..=7 {
            let encoded = value.encode(scale).unwrap();
            let mut bytes = vec![0x2a, scale, encoded.len() as u8];
            bytes.extend(encoded);
            let parameter = rpc_value(&mut Cursor::new(&bytes)).unwrap();
            assert_eq!(
                parameter.ast_type().to_string(),
                format!("datetime2({scale})")
            );
            assert_eq!(
                parameter.value,
                Value::Text(value.format_iso(scale).unwrap())
            );
            let absent = rpc_value(&mut Cursor::new(&[0x2a, scale, 0])).unwrap();
            assert_eq!(absent.value, Value::Null);
            assert_eq!(absent.data_type, parameter.data_type);
        }
        for bytes in [
            &[0x2a, 8, 0][..],
            &[0x2a, 7, 6, 0, 0, 0, 0, 0, 0],
            &[0x2a, 7, 8, 0],
            &[0x2a, 0, 6, 255, 255, 255, 0, 0, 0],
            &[0x2a, 0, 6, 0, 0, 0, 255, 255, 255],
        ] {
            assert!(rpc_value(&mut Cursor::new(bytes)).is_err());
        }
    }
}

#[cfg(test)]
mod datetimeoffset_tests {
    use super::*;
    #[test]
    fn exact_datetimeoffset_parameters_and_malformed_metadata() {
        for offset in [-840, -330, 0, 330, 840] {
            let value = crate::datetimeoffset::DateTimeOffset::from_local(
                crate::datetime2::DateTime2::parse_iso("2026-07-01T02:30:00.1234567").unwrap(),
                offset,
            )
            .unwrap();
            for scale in 0..=7 {
                let encoded = value.encode(scale).unwrap();
                let mut bytes = vec![0x2b, scale, encoded.len() as u8];
                bytes.extend(encoded);
                let p = rpc_value(&mut Cursor::new(&bytes)).unwrap();
                assert_eq!(p.ast_type().to_string(), format!("datetimeoffset({scale})"));
                assert_eq!(p.value, Value::Text(value.format_iso(scale).unwrap()));
                let absent = rpc_value(&mut Cursor::new(&[0x2b, scale, 0])).unwrap();
                assert_eq!(absent.value, Value::Null);
                assert_eq!(absent.data_type, p.data_type);
                for len in 0..bytes.len() {
                    assert!(rpc_value(&mut Cursor::new(&bytes[..len])).is_err());
                }
                let n = bytes.len();
                bytes[n - 2..].copy_from_slice(&841i16.to_le_bytes());
                assert!(rpc_value(&mut Cursor::new(&bytes)).is_err());
            }
        }
        for bytes in [
            &[0x2b, 8, 0][..],
            &[0x2b, 7, 8, 0, 0, 0, 0, 0, 0, 0, 0],
            &[0x2b, 0, 8, 255, 255, 255, 0, 0, 0, 0, 0],
        ] {
            assert!(rpc_value(&mut Cursor::new(bytes)).is_err());
        }
    }
}

#[cfg(test)]
mod unicode_binding_tests {
    use super::*;
    #[test]
    fn unicode_rpc_units_preserve_surrogates_and_validate_byte_framing() {
        let mut bounded = vec![0xe7, 6, 0];
        bounded.extend(tds::COLLATION);
        bounded.extend([2, 0, 0x3e, 0xd8]);
        let p = rpc_value(&mut Cursor::new(&bounded)).unwrap();
        assert_eq!(p.value, Value::Unicode(vec![0xd83e]));
        assert_eq!(
            p.data_type,
            DataType::Character(CharacterType::new(Family::Nvarchar, Length::Bounded(3)).unwrap())
        );
        for length in 0..bounded.len() {
            assert!(rpc_value(&mut Cursor::new(&bounded[..length])).is_err());
        }
        let mut max = vec![0xe7, 255, 255];
        max.extend(tds::COLLATION);
        max.extend(2u64.to_le_bytes());
        for byte in [0x86, 0xdd] {
            max.extend(1u32.to_le_bytes());
            max.push(byte);
        }
        max.extend(0u32.to_le_bytes());
        assert_eq!(
            rpc_value(&mut Cursor::new(&max)).unwrap().value,
            Value::Unicode(vec![0xdd86])
        );
        let mut odd = bounded[..8].to_vec();
        odd.extend([1, 0, 0x3e]);
        assert!(rpc_value(&mut Cursor::new(&odd)).is_err());
    }
}
