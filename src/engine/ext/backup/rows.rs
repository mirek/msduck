//! Result sets with SQL Server's exact descriptors, for RESTORE HEADERONLY
//! and RESTORE FILELISTONLY. Every column is nullable, as SQL Server sends
//! them (reference/gaps-backup.json).
use crate::tds::{self, Column, Type};
use anyhow::{Result, bail};
use msduck_core::result::{Origin, Properties};

/// A cell value.
#[derive(Clone, Debug, PartialEq)]
pub enum Cell {
    Null,
    Int(i64),
    Text(String),
    Bit(bool),
    /// Microseconds since the Unix epoch.
    DateTime(i64),
    Decimal(i128),
    /// A GUID in its canonical text form.
    Guid(String),
}

impl Cell {
    pub fn text(value: impl Into<String>) -> Self {
        Self::Text(value.into())
    }

    pub fn optional(value: Option<&str>) -> Self {
        value.map_or(Self::Null, Self::text)
    }
}

/// Column kinds used by these result sets.
#[derive(Clone, Copy, Debug)]
pub enum Kind {
    Nvarchar(u16),
    Nchar(u16),
    TinyInt,
    SmallInt,
    Int,
    BigInt,
    Bit,
    DateTime,
    Numeric(u8),
    Guid,
    Varbinary(u16),
}

impl Kind {
    fn tds(self) -> Type {
        match self {
            Self::Nvarchar(width) => Type::Nvarchar(width),
            Self::Nchar(width) => Type::Nchar(width),
            Self::TinyInt => Type::Int(1),
            Self::SmallInt => Type::Int(2),
            Self::Int => Type::Int(4),
            Self::BigInt => Type::Int(8),
            Self::Bit => Type::Bit,
            Self::DateTime => Type::LegacyDateTime(8),
            Self::Numeric(precision) => Type::Decimal(precision, 0),
            Self::Guid => Type::Guid,
            Self::Varbinary(width) => Type::Varbinary(width),
        }
    }
}

/// COLMETADATA as SQL Server sends these result sets: every column nullable
/// (flags 0x0001), and numeric columns as NUMERICN (0x6C), which the shared
/// encoder writes as DECIMALN.
fn metadata(out: &mut Vec<u8>, columns: &[(&str, Kind)]) -> Result<()> {
    out.push(0x81);
    out.extend(u16::try_from(columns.len())?.to_le_bytes());
    for (name, kind) in columns {
        out.extend(0u32.to_le_bytes());
        out.extend(1u16.to_le_bytes());
        match kind {
            Kind::Numeric(precision) => {
                out.extend([0x6c, tds::DECIMAL_RESULT_MAX_LENGTH, *precision, 0])
            }
            _ => {
                // The shared encoder writes the type info; take it without
                // the column header and name.
                let mut column = Vec::new();
                tds::metadata(
                    &mut column,
                    &[Column {
                        collation: None,
                        properties: Properties {
                            nullable: Some(true),
                            origin: Origin::Unknown,
                        },
                        name: String::new(),
                        kind: kind.tds(),
                    }],
                )?;
                // 0x81, count (2), user type (4), flags (2), type info,
                // then the empty name's length byte.
                out.extend(&column[9..column.len() - 1]);
            }
        }
        let units: Vec<u16> = name.encode_utf16().collect();
        out.push(u8::try_from(units.len())?);
        out.extend(units.iter().flat_map(|unit| unit.to_le_bytes()));
    }
    Ok(())
}

/// COLMETADATA, then a ROW token per row.
pub fn result_set(out: &mut Vec<u8>, columns: &[(&str, Kind)], rows: &[Vec<Cell>]) -> Result<()> {
    let mut body = Vec::new();
    metadata(&mut body, columns)?;
    for row in rows {
        if row.len() != columns.len() {
            bail!(
                "result row has {} values for {} columns",
                row.len(),
                columns.len()
            );
        }
        body.push(0xd1);
        for ((name, kind), cell) in columns.iter().zip(row) {
            value(&mut body, *kind, cell).map_err(|error| anyhow::anyhow!("{name}: {error}"))?;
        }
    }
    out.extend(body);
    Ok(())
}

fn value(out: &mut Vec<u8>, kind: Kind, cell: &Cell) -> Result<()> {
    match (kind, cell) {
        (Kind::Nvarchar(_) | Kind::Nchar(_), Cell::Null) => {
            tds::unicode_value(out, &kind.tds(), None)?
        }
        (Kind::Varbinary(_), Cell::Null) => out.extend(u16::MAX.to_le_bytes()),
        (_, Cell::Null) => out.push(0),
        (Kind::Nvarchar(_) | Kind::Nchar(_), Cell::Text(text)) => {
            let units: Vec<u16> = text.encode_utf16().collect();
            tds::unicode_value(out, &kind.tds(), Some(&units))?
        }
        (Kind::TinyInt, Cell::Int(value)) => {
            out.push(1);
            out.push(u8::try_from(*value)?);
        }
        (Kind::SmallInt, Cell::Int(value)) => {
            out.push(2);
            out.extend(i16::try_from(*value)?.to_le_bytes());
        }
        (Kind::Int, Cell::Int(value)) => {
            out.push(4);
            out.extend(i32::try_from(*value)?.to_le_bytes());
        }
        (Kind::BigInt, Cell::Int(value)) => {
            out.push(8);
            out.extend(value.to_le_bytes());
        }
        (Kind::Bit, Cell::Bit(value)) => out.extend([1, u8::from(*value)]),
        (Kind::DateTime, Cell::DateTime(micros)) => {
            tds::legacy_datetime(out, 8, i128::from(*micros) * 1000, false)?
        }
        (Kind::Numeric(precision), Cell::Decimal(value)) => {
            let magnitude = value.unsigned_abs();
            if magnitude >= 10u128.pow(u32::from(precision)) {
                bail!("value exceeds numeric({precision},0)");
            }
            let length = tds::decimal_value_length(magnitude);
            out.push(length);
            out.push(u8::from(*value >= 0));
            out.extend(&magnitude.to_le_bytes()[..usize::from(length) - 1]);
        }
        (Kind::Guid, Cell::Guid(text)) => {
            out.push(16);
            out.extend(guid_bytes(text)?);
        }
        (kind, cell) => bail!("{cell:?} does not fit {kind:?}"),
    }
    Ok(())
}

/// A GUID's wire bytes: the first three groups little-endian, the rest as
/// written.
pub fn guid_bytes(text: &str) -> Result<[u8; 16]> {
    let uuid = uuid::Uuid::parse_str(text)?;
    let (a, b, c, d) = uuid.as_fields();
    let mut bytes = [0u8; 16];
    bytes[..4].copy_from_slice(&a.to_le_bytes());
    bytes[4..6].copy_from_slice(&b.to_le_bytes());
    bytes[6..8].copy_from_slice(&c.to_le_bytes());
    bytes[8..].copy_from_slice(d);
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guids_use_mixed_endian_bytes() {
        assert_eq!(
            guid_bytes("4A7606AC-93ED-4829-ABC7-D37CEB05B983").unwrap(),
            [
                0xAC, 0x06, 0x76, 0x4A, 0xED, 0x93, 0x29, 0x48, 0xAB, 0xC7, 0xD3, 0x7C, 0xEB, 0x05,
                0xB9, 0x83
            ]
        );
    }

    #[test]
    fn metadata_matches_sql_server_type_info() {
        let mut out = Vec::new();
        metadata(
            &mut out,
            &[
                ("A", Kind::Numeric(25)),
                ("B", Kind::Nvarchar(128)),
                ("C", Kind::TinyInt),
            ],
        )
        .unwrap();
        assert_eq!(&out[..3], [0x81, 3, 0]);
        assert_eq!(&out[3..9], [0, 0, 0, 0, 1, 0]);
        assert_eq!(&out[9..13], [0x6c, 17, 25, 0]);
        assert_eq!(&out[13..16], [1, b'A', 0]);
        assert_eq!(&out[16..22], [0, 0, 0, 0, 1, 0]);
        assert_eq!(&out[22..25], [0xe7, 0, 1]);
        assert_eq!(&out[25..30], tds::COLLATION);
        assert_eq!(&out[30..33], [1, b'B', 0]);
        assert_eq!(&out[39..41], [0x26, 1]);
    }

    #[test]
    fn values_encode_with_nullable_lengths() {
        let mut out = Vec::new();
        value(&mut out, Kind::Numeric(25), &Cell::Decimal(0)).unwrap();
        assert_eq!(out, [5, 1, 0, 0, 0, 0]);
        out.clear();
        value(&mut out, Kind::Nvarchar(128), &Cell::Null).unwrap();
        assert_eq!(out, [0xff, 0xff]);
        out.clear();
        value(&mut out, Kind::SmallInt, &Cell::Int(52)).unwrap();
        assert_eq!(out, [2, 52, 0]);
        assert!(value(&mut out, Kind::TinyInt, &Cell::Int(300)).is_err());
    }
}
