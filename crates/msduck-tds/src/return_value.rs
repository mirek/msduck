//! Bounded TDS 7.2–7.4 RETURNVALUE tokens. The caller supplies already-bound
//! values; this codec never consults a session, database or process state.
use crate::{MAX_MESSAGE, collation::Collation, decimal_value_length};
use anyhow::{Result, ensure};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Output,
    UdfReturn,
}

/// The subset of TYPE_INFO for which this module can encode a complete value.
/// `None` length means a PLP MAX declaration for variable-length families.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Declaration {
    Int(u8),
    Bit,
    Unicode {
        units: Option<u16>,
        fixed: bool,
        collation: Collation,
    },
    Ansi {
        bytes: Option<u16>,
        fixed: bool,
        collation: Collation,
    },
    Binary {
        bytes: Option<u16>,
        fixed: bool,
    },
    Decimal {
        precision: u8,
        scale: u8,
        /// TYPE_INFO storage capacity: 5, 9, 13 or 17 bytes.
        max_length: u8,
    },
}

/// Text is passed as raw UTF-16 units and ANSI/binary as already encoded bytes.
/// Numeric values are exact; no type conversion occurs in this layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Value<'a> {
    Null,
    Int(i64),
    Bit(bool),
    Unicode(&'a [u16]),
    Bytes(&'a [u8]),
    Decimal(i128),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Parameter<'a> {
    pub ordinal: u16,
    /// Raw UTF-16 parameter name, without a leading `@`.
    pub name: &'a [u16],
    pub status: Status,
    pub user_type: u32,
    pub flags: u16,
    pub declaration: Declaration,
    pub value: Value<'a>,
}

/// Encode one complete RETURNVALUE token, or leave `out` unchanged on error.
/// The 16 MiB server message bound also limits a single PLP token here.
pub fn encode(out: &mut Vec<u8>, parameter: &Parameter<'_>) -> Result<()> {
    ensure!(
        parameter.name.len() <= u8::MAX as usize,
        "RETURNVALUE name exceeds 255 UTF-16 units"
    );
    // fEncrypted requires CryptoMetaData, which this codec does not emit.
    ensure!(
        parameter.flags & 0x0800 == 0,
        "encrypted RETURNVALUE is unsupported"
    );
    let mut token = Vec::new();
    token.push(0xac);
    token.extend(parameter.ordinal.to_le_bytes());
    token.push(parameter.name.len() as u8);
    for unit in parameter.name {
        token.extend(unit.to_le_bytes());
    }
    token.push(match parameter.status {
        Status::Output => 1,
        Status::UdfReturn => 2,
    });
    token.extend(parameter.user_type.to_le_bytes());
    token.extend(parameter.flags.to_le_bytes());
    match (parameter.declaration, parameter.value) {
        (Declaration::Int(width @ (1 | 2 | 4 | 8)), value) => {
            token.extend([0x26, width]);
            match value {
                Value::Null => token.push(0),
                Value::Int(number) => {
                    let valid = match width {
                        1 => (0..=255).contains(&number),
                        2 => i16::try_from(number).is_ok(),
                        4 => i32::try_from(number).is_ok(),
                        8 => true,
                        _ => unreachable!(),
                    };
                    ensure!(valid, "RETURNVALUE integer exceeds declared width");
                    token.push(width);
                    token.extend(&number.to_le_bytes()[..usize::from(width)]);
                }
                _ => anyhow::bail!("RETURNVALUE value does not match integer declaration"),
            }
        }
        (Declaration::Int(_), _) => anyhow::bail!("invalid RETURNVALUE integer width"),
        (Declaration::Bit, value) => {
            token.extend([0x68, 1]);
            match value {
                Value::Null => token.push(0),
                Value::Bit(bit) => token.extend([1, u8::from(bit)]),
                _ => anyhow::bail!("RETURNVALUE value does not match BIT declaration"),
            }
        }
        (
            Declaration::Unicode {
                units,
                fixed,
                collation,
            },
            value,
        ) => {
            let capacity = units.map(usize::from);
            ensure!(!fixed || capacity.is_some(), "NCHAR(MAX) is invalid");
            if let Some(capacity) = capacity {
                ensure!(
                    (1..=4000).contains(&capacity),
                    "invalid RETURNVALUE Unicode width"
                );
            }
            token.push(if fixed { 0xef } else { 0xe7 });
            token.extend(units.map_or(u16::MAX, |n| n * 2).to_le_bytes());
            token.extend(collation.bytes());
            match value {
                Value::Null => null_length(&mut token, capacity),
                Value::Unicode(text) => {
                    let bytes = text
                        .len()
                        .checked_mul(2)
                        .ok_or_else(|| anyhow::anyhow!("RETURNVALUE Unicode length overflow"))?;
                    ensure!(
                        bytes <= MAX_MESSAGE,
                        "RETURNVALUE Unicode payload exceeds message bound"
                    );
                    if let Some(capacity) = capacity {
                        ensure!(
                            text.len() <= capacity && (!fixed || text.len() == capacity),
                            "RETURNVALUE Unicode value exceeds or mismatches declared width"
                        );
                    }
                    value_length(&mut token, capacity, bytes)?;
                    for unit in text {
                        token.extend(unit.to_le_bytes());
                    }
                    if capacity.is_none() {
                        token.extend(0u32.to_le_bytes());
                    }
                }
                _ => anyhow::bail!("RETURNVALUE value does not match Unicode declaration"),
            }
        }
        (
            Declaration::Ansi {
                bytes,
                fixed,
                collation,
            },
            value,
        ) => {
            variable_bytes(
                &mut token,
                bytes,
                fixed,
                collation,
                value,
                if fixed { 0xaf } else { 0xa7 },
            )?;
        }
        (Declaration::Binary { bytes, fixed }, value) => {
            variable_bytes(
                &mut token,
                bytes,
                fixed,
                Collation::default(),
                value,
                if fixed { 0xad } else { 0xa5 },
            )?;
        }
        (
            Declaration::Decimal {
                precision,
                scale,
                max_length,
            },
            value,
        ) => {
            ensure!(
                (1..=38).contains(&precision) && scale <= precision,
                "invalid RETURNVALUE decimal declaration"
            );
            ensure!(
                matches!(max_length, 5 | 9 | 13 | 17)
                    && max_length >= crate::decimal_length(precision),
                "invalid RETURNVALUE decimal storage length"
            );
            token.extend([0x6a, max_length, precision, scale]);
            match value {
                Value::Null => token.push(0),
                Value::Decimal(coefficient) => {
                    let magnitude = coefficient.unsigned_abs();
                    ensure!(
                        magnitude < 10u128.pow(u32::from(precision)),
                        "RETURNVALUE decimal exceeds declared precision"
                    );
                    let length = decimal_value_length(magnitude);
                    ensure!(
                        length <= max_length,
                        "RETURNVALUE decimal exceeds declared storage length"
                    );
                    token.push(length);
                    token.push(u8::from(coefficient >= 0));
                    for byte in magnitude.to_le_bytes().iter().take(usize::from(length - 1)) {
                        token.push(*byte);
                    }
                }
                _ => anyhow::bail!("RETURNVALUE value does not match decimal declaration"),
            }
        }
    }
    ensure!(
        token.len() <= MAX_MESSAGE,
        "RETURNVALUE token exceeds message bound"
    );
    ensure!(
        out.len()
            .checked_add(token.len())
            .is_some_and(|length| length <= MAX_MESSAGE),
        "RETURNVALUE would exceed message bound"
    );
    out.extend(token);
    Ok(())
}

fn null_length(out: &mut Vec<u8>, bounded: Option<usize>) {
    if bounded.is_some() {
        out.extend(u16::MAX.to_le_bytes());
    } else {
        out.extend(u64::MAX.to_le_bytes());
    }
}
fn value_length(out: &mut Vec<u8>, bounded: Option<usize>, bytes: usize) -> Result<()> {
    if bounded.is_some() {
        out.extend(u16::try_from(bytes)?.to_le_bytes());
    } else {
        out.extend(u64::try_from(bytes)?.to_le_bytes());
        if bytes != 0 {
            out.extend(u32::try_from(bytes)?.to_le_bytes());
        }
    }
    Ok(())
}
fn variable_bytes(
    out: &mut Vec<u8>,
    declared: Option<u16>,
    fixed: bool,
    collation: Collation,
    value: Value<'_>,
    type_id: u8,
) -> Result<()> {
    let capacity = declared.map(usize::from);
    ensure!(
        !fixed || capacity.is_some(),
        "fixed MAX declaration is invalid"
    );
    if let Some(capacity) = capacity {
        ensure!(
            (1..=8000).contains(&capacity),
            "invalid RETURNVALUE byte width"
        );
    }
    out.push(type_id);
    out.extend(declared.unwrap_or(u16::MAX).to_le_bytes());
    if matches!(type_id, 0xa7 | 0xaf) {
        out.extend(collation.bytes());
    }
    match value {
        Value::Null => null_length(out, capacity),
        Value::Bytes(bytes) => {
            ensure!(
                bytes.len() <= MAX_MESSAGE,
                "RETURNVALUE byte payload exceeds message bound"
            );
            if let Some(capacity) = capacity {
                ensure!(
                    bytes.len() <= capacity && (!fixed || bytes.len() == capacity),
                    "RETURNVALUE value exceeds or mismatches declared byte width"
                );
            }
            value_length(out, capacity, bytes.len())?;
            out.extend(bytes);
            if capacity.is_none() {
                out.extend(0u32.to_le_bytes());
            }
        }
        _ => anyhow::bail!("RETURNVALUE value does not match byte declaration"),
    }
    Ok(())
}
