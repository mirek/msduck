//! BulkLoadBCP wire values: the column checks SQL Server makes against the
//! COLMETADATA token (4816) and the conversion of each ROW value to a typed
//! parameter of the column's INSERT BULK declaration.
//!
//! The token stream itself is decoded by the shared, read-only codec in
//! `crates/msduck-tds/src/bulk_load.rs`, path-imported here as its own tests
//! do (the crate does not export it).
use anyhow::{Result, bail, ensure};
use msduck_core::{
    bulk_character_admission::{self as admission, Metadata, Wire, WireLength},
    character::Family as CharacterFamily,
    types::Type,
    value::{Decimal, TimeUnit, Value},
};

#[allow(dead_code)]
#[path = "../../../../crates/msduck-tds/src/bulk_load.rs"]
pub(super) mod codec;

pub(super) use codec::{Column as WireColumn, TypeInfo, ValueFormat};

/// A wire or declared type family: SQL Server requires the COLMETADATA type
/// to belong to the family the INSERT BULK statement declared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Family {
    Integer(u8),
    Bit,
    Real,
    Float,
    Money(u8),
    DateTime(u8),
    Decimal,
    Date,
    Time,
    DateTime2,
    DateTimeOffset,
    Guid,
    Character { unicode: bool },
    Binary,
    Text,
    Ntext,
    Image,
    Variant,
}

fn declared_family(kind: &Type) -> Option<Family> {
    Some(match kind {
        Type::TinyInt => Family::Integer(1),
        Type::SmallInt => Family::Integer(2),
        Type::Int => Family::Integer(4),
        Type::BigInt => Family::Integer(8),
        Type::Bit => Family::Bit,
        Type::Real => Family::Real,
        Type::Float => Family::Float,
        Type::Money => Family::Money(8),
        Type::SmallMoney => Family::Money(4),
        Type::DateTime => Family::DateTime(8),
        Type::SmallDateTime => Family::DateTime(4),
        Type::Decimal(_) => Family::Decimal,
        Type::Date => Family::Date,
        Type::Time(_) => Family::Time,
        Type::DateTime2(_) => Family::DateTime2,
        Type::DateTimeOffset(_) => Family::DateTimeOffset,
        Type::UniqueIdentifier => Family::Guid,
        Type::Character(character) => Family::Character {
            unicode: matches!(
                character.family(),
                CharacterFamily::Nvarchar | CharacterFamily::Nchar
            ),
        },
        Type::Binary(_) => Family::Binary,
        Type::Text => Family::Text,
        Type::Ntext => Family::Ntext,
        Type::Image => Family::Image,
        Type::Variant => Family::Variant,
        Type::Xml => return None,
    })
}

/// The byte width a fixed or nullable fixed-width wire type carries.
fn width(info: &TypeInfo) -> usize {
    match info.format {
        ValueFormat::Fixed(width) | ValueFormat::ByteLen { max: width, .. } => width,
        _ => 0,
    }
}

fn wire_family(info: &TypeInfo) -> Option<Family> {
    Some(match info.id {
        0x30 => Family::Integer(1),
        0x34 => Family::Integer(2),
        0x38 => Family::Integer(4),
        0x7f => Family::Integer(8),
        0x26 => Family::Integer(width(info) as u8),
        0x32 | 0x68 => Family::Bit,
        0x3b => Family::Real,
        0x3e => Family::Float,
        0x6d if width(info) == 4 => Family::Real,
        0x6d => Family::Float,
        0x7a => Family::Money(4),
        0x3c => Family::Money(8),
        0x6e => Family::Money(width(info) as u8),
        0x3a => Family::DateTime(4),
        0x3d => Family::DateTime(8),
        0x6f => Family::DateTime(width(info) as u8),
        0x6a | 0x6c => Family::Decimal,
        0x28 => Family::Date,
        0x29 => Family::Time,
        0x2a => Family::DateTime2,
        0x2b => Family::DateTimeOffset,
        0x24 => Family::Guid,
        0x27 | 0x2f | 0xa7 | 0xaf => Family::Character { unicode: false },
        0xe7 | 0xef => Family::Character { unicode: true },
        0x25 | 0x2d | 0xa5 | 0xad => Family::Binary,
        0x23 => Family::Text,
        0x63 => Family::Ntext,
        0x22 => Family::Image,
        0x62 => Family::Variant,
        _ => return None,
    })
}

/// What SQL Server checks for one column of the COLMETADATA token: the
/// 4816 state of a mismatch, or `None`. The captured rules
/// (reference/gaps-bulk.json): the wire type must belong to the declared
/// type's family and its NULLABLE flag must equal the target column's
/// nullability (state 1), and a MAX target needs a PLP (MAX) wire type
/// (state 2).
pub(super) fn incompatible(
    wire: &WireColumn,
    declared: &Type,
    target_nullable: bool,
    target_max: bool,
) -> Option<u8> {
    if let Type::Character(character) = declared {
        let wire_family = match wire.type_info.id {
            0xa7 => Some(CharacterFamily::Varchar),
            0xaf => Some(CharacterFamily::Char),
            0xe7 => Some(CharacterFamily::Nvarchar),
            0xef => Some(CharacterFamily::Nchar),
            _ => None,
        };
        let length = match wire.type_info.format {
            ValueFormat::ShortLen { max, .. } => {
                u16::try_from(max).ok().map(WireLength::BoundedBytes)
            }
            ValueFormat::Plp { .. } => Some(WireLength::Plp),
            _ => None,
        };
        if let (Some(family), Some(length)) = (wire_family, length) {
            match admission::metadata(
                *character,
                Wire {
                    family,
                    length,
                    nullable: wire.flags & 1 != 0,
                },
                target_nullable,
                target_max,
            ) {
                Metadata::Admitted => return None,
                Metadata::FamilyOrNullability => return Some(1),
                Metadata::MaxFraming => return Some(2),
                // Preserve the existing adapter behavior for unmeasured shapes.
                Metadata::Unknown => {}
            }
        }
    }
    let family = wire_family(&wire.type_info);
    if family.is_none() || family != declared_family(declared) {
        return Some(1);
    }
    if (wire.flags & 1 != 0) != target_nullable {
        return Some(1);
    }
    let short = matches!(
        wire.type_info.format,
        ValueFormat::ShortLen { .. } | ValueFormat::ByteLen { exact: false, .. }
    );
    (target_max && short).then_some(2)
}

/// One ROW value as a parameter of the declared type. The family was
/// checked against the declaration and source ROW length admitted before
/// this conversion. Fixed-source padding precedes target storage conversion.
pub(super) fn declared_value(
    info: &TypeInfo,
    declared: &Type,
    bytes: Option<&[u8]>,
) -> Result<Value> {
    let mut value = value(info, bytes)?;
    if let Type::Character(character) = declared
        && let Some(padding) = admission::padding_units(*character, bytes.map(<[u8]>::len))
    {
        match &mut value {
            Value::Text(text) => {
                text.try_reserve(padding)?;
                text.extend(std::iter::repeat_n(' ', padding));
            }
            Value::Unicode(units) => {
                units.try_reserve(padding)?;
                units.extend(std::iter::repeat_n(32, padding));
            }
            _ => bail!("invalid admitted fixed character representation"),
        }
    }
    Ok(value)
}

pub(super) fn value(info: &TypeInfo, bytes: Option<&[u8]>) -> Result<Value> {
    let Some(bytes) = bytes else {
        return Ok(Value::Null);
    };
    Ok(match info.id {
        0x30 => Value::UTinyInt(bytes[0]),
        0x34 => Value::SmallInt(i16::from_le_bytes(bytes.try_into()?)),
        0x38 => Value::Int(i32::from_le_bytes(bytes.try_into()?)),
        0x7f => Value::BigInt(i64::from_le_bytes(bytes.try_into()?)),
        0x26 => match bytes.len() {
            1 => Value::UTinyInt(bytes[0]),
            2 => Value::SmallInt(i16::from_le_bytes(bytes.try_into()?)),
            4 => Value::Int(i32::from_le_bytes(bytes.try_into()?)),
            8 => Value::BigInt(i64::from_le_bytes(bytes.try_into()?)),
            _ => bail!("invalid integer width"),
        },
        0x32 | 0x68 => Value::Boolean(bytes[0] != 0),
        0x3b => Value::Float(f32::from_le_bytes(bytes.try_into()?)),
        0x3e => Value::Double(f64::from_le_bytes(bytes.try_into()?)),
        0x6d if bytes.len() == 4 => Value::Float(f32::from_le_bytes(bytes.try_into()?)),
        0x6d => Value::Double(f64::from_le_bytes(bytes.try_into()?)),
        0x7a | 0x3c | 0x6e => money(bytes)?,
        0x3a | 0x3d | 0x6f => datetime(bytes)?,
        0x6a | 0x6c => decimal(info, bytes)?,
        0x28 => {
            let mut days = [0u8; 4];
            days[..3].copy_from_slice(bytes);
            let days = u32::from_le_bytes(days);
            ensure!(days <= 3_652_058, "date outside SQL Server range");
            Value::Date32(days as i32 - 719_162)
        }
        0x29 => time(info, bytes)?,
        0x2a => {
            let scale = info.scale.unwrap_or(7);
            Value::Text(msduck_core::datetime2::DateTime2::decode(bytes, scale)?.format_iso(scale)?)
        }
        0x2b => {
            let scale = info.scale.unwrap_or(7);
            Value::Text(
                msduck_core::datetimeoffset::DateTimeOffset::decode(bytes, scale)?
                    .format_iso(scale)?,
            )
        }
        0x24 => Value::Text(uuid::Uuid::from_bytes_le(bytes.try_into()?).to_string()),
        0x27 | 0x2f | 0xa7 | 0xaf | 0x23 => {
            Value::Text(msduck_core::encoding::decode_cp1252(bytes))
        }
        0xe7 | 0xef | 0x63 => {
            ensure!(bytes.len() % 2 == 0, "odd UTF-16 byte length");
            Value::from_utf16(
                bytes
                    .chunks_exact(2)
                    .map(|unit| u16::from_le_bytes([unit[0], unit[1]]))
                    .collect(),
            )
        }
        0x25 | 0x2d | 0xa5 | 0xad | 0x22 => Value::Blob(bytes.to_vec()),
        other => bail!("unsupported bulk-load type 0x{other:02x}"),
    })
}

fn money(bytes: &[u8]) -> Result<Value> {
    let (precision, scaled) = if bytes.len() == 4 {
        (10, i32::from_le_bytes(bytes.try_into()?) as i64)
    } else {
        // MONEY is a signed high int32 followed by an unsigned low uint32.
        let high = i32::from_le_bytes(bytes[..4].try_into()?) as i64;
        let low = u32::from_le_bytes(bytes[4..].try_into()?) as i64;
        (19, (high << 32) | low)
    };
    Ok(Value::Decimal(Decimal::new(precision, 4, scaled as i128)?))
}

fn datetime(bytes: &[u8]) -> Result<Value> {
    let (days, micros) = if bytes.len() == 4 {
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
        // 1/300-second ticks, rounded to the nearest microsecond as RPC
        // parameters are.
        (days, (ticks * 1_000_000 + 150) / 300)
    };
    Ok(Value::Timestamp(
        TimeUnit::Microsecond,
        (days - 25_567) * 86_400_000_000 + micros,
    ))
}

fn decimal(info: &TypeInfo, bytes: &[u8]) -> Result<Value> {
    let precision = info.precision.unwrap_or(38);
    let scale = info.scale.unwrap_or(0);
    let mut magnitude = [0u8; 16];
    magnitude[..bytes.len() - 1].copy_from_slice(&bytes[1..]);
    let magnitude = u128::from_le_bytes(magnitude);
    ensure!(
        magnitude < 10u128.pow(precision as u32),
        "decimal value exceeds declared precision"
    );
    let coefficient = if bytes[0] == 0 {
        -(magnitude as i128)
    } else {
        magnitude as i128
    };
    Ok(Value::Decimal(Decimal::new(precision, scale, coefficient)?))
}

fn time(info: &TypeInfo, bytes: &[u8]) -> Result<Value> {
    let scale = info.scale.unwrap_or(7);
    let mut units = [0u8; 8];
    units[..bytes.len()].copy_from_slice(bytes);
    let units = u64::from_le_bytes(units);
    let per_second = 10u64.pow(scale as u32);
    ensure!(units < 86_400 * per_second, "time outside SQL Server range");
    let seconds = units / per_second;
    let fraction = (units % per_second) * 10u64.pow(7 - scale as u32);
    Ok(Value::Text(format!(
        "{:02}:{:02}:{:02}.{fraction:07}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_unicode_source_padding_preserves_isolated_units_and_null() {
        let declared = Type::Character(
            msduck_core::character::CharacterType::new(
                CharacterFamily::Nchar,
                msduck_core::character::Length::Bounded(4),
            )
            .unwrap(),
        );
        let info = TypeInfo {
            id: 0xef,
            format: ValueFormat::ShortLen {
                max: 2,
                unicode: true,
            },
            precision: None,
            scale: None,
            collation: None,
        };
        assert_eq!(declared_value(&info, &declared, None).unwrap(), Value::Null);
        assert_eq!(
            declared_value(&info, &declared, Some(&[0x3e, 0xd8])).unwrap(),
            Value::Unicode(vec![0xd83e, 32, 32, 32])
        );
        assert_eq!(
            declared_value(&info, &declared, Some(&[])).unwrap(),
            Value::Text("    ".into())
        );
    }
    use msduck_core::character::{CharacterType, Length};

    fn column(id: u8, format: ValueFormat, flags: u16) -> WireColumn {
        WireColumn {
            name_utf16: Vec::new(),
            user_type: 0,
            flags,
            type_info: TypeInfo {
                id,
                format,
                precision: None,
                scale: None,
                collation: None,
            },
        }
    }

    #[test]
    fn metadata_must_match_the_declaration_nullability_and_max_targets() {
        let int_n = column(
            0x26,
            ValueFormat::ByteLen {
                max: 4,
                exact: true,
            },
            1,
        );
        let int4 = column(0x38, ValueFormat::Fixed(4), 0);
        assert_eq!(incompatible(&int_n, &Type::Int, true, false), None);
        assert_eq!(incompatible(&int4, &Type::Int, false, false), None);
        // Captured 4816: nullability differs from the target column.
        assert_eq!(incompatible(&int_n, &Type::Int, false, false), Some(1));
        assert_eq!(incompatible(&int4, &Type::Int, true, false), Some(1));
        // Captured 4816: the wire type is not the declared type.
        assert_eq!(incompatible(&int4, &Type::BigInt, false, false), Some(1));
        let varchar = Type::Character(
            CharacterType::new(CharacterFamily::Varchar, Length::Bounded(3)).unwrap(),
        );
        assert_eq!(incompatible(&int4, &varchar, false, false), Some(1));
        let nvarchar = column(
            0xe7,
            ValueFormat::ShortLen {
                max: 20,
                unicode: true,
            },
            1,
        );
        let declared = Type::Character(
            CharacterType::new(CharacterFamily::Nvarchar, Length::Bounded(10)).unwrap(),
        );
        assert_eq!(incompatible(&nvarchar, &declared, true, false), None);
        // Captured 4816: a MAX target needs a MAX (PLP) wire type.
        assert_eq!(incompatible(&nvarchar, &declared, true, true), Some(2));
        let plp = column(0xe7, ValueFormat::Plp { unicode: true }, 1);
        let max =
            Type::Character(CharacterType::new(CharacterFamily::Nvarchar, Length::Max).unwrap());
        assert_eq!(incompatible(&plp, &max, true, true), None);
    }

    #[test]
    fn values_keep_the_representation_rpc_parameters_use() {
        let fixed = |id, width| TypeInfo {
            id,
            format: ValueFormat::Fixed(width),
            precision: None,
            scale: None,
            collation: None,
        };
        assert_eq!(
            value(&fixed(0x38, 4), Some(&(-7i32).to_le_bytes())).unwrap(),
            Value::Int(-7)
        );
        assert_eq!(value(&fixed(0x38, 4), None).unwrap(), Value::Null);
        assert_eq!(
            value(&fixed(0x32, 1), Some(&[1])).unwrap(),
            Value::Boolean(true)
        );
        // 2024-01-02 03:04:05.123 as datetime: days since 1900, 1/300 ticks.
        let days = 45_291i32.to_le_bytes();
        let ticks = ((3 * 3600 + 4 * 60 + 5) * 300 + 37u32).to_le_bytes();
        let bytes = [days, ticks].concat();
        assert_eq!(
            value(&fixed(0x3d, 8), Some(&bytes)).unwrap(),
            Value::Timestamp(TimeUnit::Microsecond, 1_704_164_645_123_333)
        );
        let decimal_info = TypeInfo {
            id: 0x6a,
            format: ValueFormat::ByteLen {
                max: 9,
                exact: true,
            },
            precision: Some(18),
            scale: Some(4),
            collation: None,
        };
        let mut bytes = vec![0];
        bytes.extend(125_000u64.to_le_bytes());
        assert_eq!(
            value(&decimal_info, Some(&bytes)).unwrap(),
            Value::Decimal(Decimal::new(18, 4, -125_000).unwrap())
        );
        let unicode = TypeInfo {
            id: 0xe7,
            format: ValueFormat::ShortLen {
                max: 20,
                unicode: true,
            },
            precision: None,
            scale: None,
            collation: None,
        };
        let units: Vec<u8> = "ž🦆"
            .encode_utf16()
            .flat_map(|unit| unit.to_le_bytes())
            .collect();
        assert_eq!(
            value(&unicode, Some(&units)).unwrap(),
            Value::Text("ž🦆".into())
        );
        let dto = TypeInfo {
            id: 0x2b,
            format: ValueFormat::ByteLen {
                max: 10,
                exact: true,
            },
            precision: None,
            scale: Some(7),
            collation: None,
        };
        let encoded = msduck_core::datetimeoffset::DateTimeOffset::parse_iso(
            "2024-01-02T03:04:05.1230000 +02:00",
        )
        .unwrap()
        .encode(7)
        .unwrap();
        assert_eq!(
            value(&dto, Some(&encoded)).unwrap(),
            Value::Text("2024-01-02T03:04:05.1230000 +02:00".into())
        );
    }
}
