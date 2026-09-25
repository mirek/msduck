//! Pure TDS token encoding for the captured SQL Server `FOR XML PATH` shapes.
//!
//! Path-imported until the shared TDS export is available. The caller owns SQL
//! binding, serialization, row-chunk selection, error tokens and packet framing.

pub const TEXT_COLUMN_NAME: &str = "XML_F52E2B61-18A1-11d1-B105-00805F49916B";
pub const MAX_ENCODED_BYTES: usize = 16 * 1024 * 1024;
const NTEXT_MAX_BYTES: usize = 0x7fff_fffe;

#[derive(Clone, Copy)]
pub enum Mode<'a> {
    NText {
        collation: [u8; 5],
        table: &'a str,
        pointer: &'a [u8],
        timestamp: [u8; 8],
    },
    Xml,
}

#[derive(Clone, Copy)]
pub enum Row<'a> {
    /// One NTEXT ROW token. The caller chooses the boundaries for long values.
    NText(&'a [u16]),
    /// One XML ROW token containing zero or more nonempty PLP chunks.
    Xml(&'a [&'a [u16]]),
}

#[derive(Clone, Copy)]
pub struct Plan<'a> {
    pub mode: Mode<'a>,
    pub name: &'a str,
    pub flags: u16,
    pub rows: &'a [Row<'a>],
    /// Logical source count; this is independent of the number of wire ROWs.
    pub source_row_count: u64,
}

pub enum Outcome<'a> {
    Success(Plan<'a>),
    /// Caller emits ERROR and DONE_ERROR; this encoder emits no success tokens.
    ValidationError,
}

#[derive(Debug, Eq, PartialEq)]
pub enum Error {
    UnsupportedDescriptor,
    InvalidName,
    InvalidTable,
    InvalidPointer,
    WrongRowKind,
    TooManyXmlRows,
    EmptyPlpChunk,
    ValueTooLarge,
    ResultTooLarge,
    Allocation,
}

struct Validated {
    name: Vec<u16>,
    table: Vec<u16>,
    size: usize,
}

fn add(size: &mut usize, part: usize) -> Result<(), Error> {
    *size = size.checked_add(part).ok_or(Error::ResultTooLarge)?;
    if *size > MAX_ENCODED_BYTES {
        return Err(Error::ResultTooLarge);
    }
    Ok(())
}

fn bytes(units: usize) -> Result<usize, Error> {
    units.checked_mul(2).ok_or(Error::ValueTooLarge)
}

fn validate(plan: &Plan<'_>) -> Result<Validated, Error> {
    let name: Vec<u16> = plan.name.encode_utf16().collect();
    if name.len() > u8::MAX as usize {
        return Err(Error::InvalidName);
    }
    let mut size = 0;
    let table = match plan.mode {
        Mode::NText { table, pointer, .. } => {
            if plan.name != TEXT_COLUMN_NAME || plan.flags != 1 {
                return Err(Error::UnsupportedDescriptor);
            }
            if pointer.is_empty() || pointer.len() > u8::MAX as usize {
                return Err(Error::InvalidPointer);
            }
            let table: Vec<u16> = table.encode_utf16().collect();
            if table.is_empty() || table.len() > u16::MAX as usize {
                return Err(Error::InvalidTable);
            }
            // COLMETADATA, one column, user type, flags, NTEXT TYPE_INFO,
            // one table-name part, and B_VARCHAR column name.
            add(&mut size, 1 + 2 + 4 + 2 + 1 + 4 + 5 + 1 + 2)?;
            add(&mut size, bytes(table.len())?)?;
            table
        }
        Mode::Xml => {
            if (name.is_empty() && plan.flags != 3) || (!name.is_empty() && plan.flags != 35) {
                return Err(Error::UnsupportedDescriptor);
            }
            if plan.rows.len() > 1 {
                return Err(Error::TooManyXmlRows);
            }
            // COLMETADATA, one column, user type, flags, XML TYPE_INFO.
            add(&mut size, 1 + 2 + 4 + 2 + 1 + 1)?;
            Vec::new()
        }
    };
    add(&mut size, 1 + bytes(name.len())?)?;
    for row in plan.rows {
        match (plan.mode, row) {
            (Mode::NText { pointer, .. }, Row::NText(units)) => {
                let length = bytes(units.len())?;
                if length > NTEXT_MAX_BYTES {
                    return Err(Error::ValueTooLarge);
                }
                add(&mut size, 1 + 1 + pointer.len() + 8 + 4)?;
                add(&mut size, length)?;
            }
            (Mode::Xml, Row::Xml(chunks)) => {
                add(&mut size, 1 + 8 + 4)?;
                for chunk in *chunks {
                    if chunk.is_empty() {
                        return Err(Error::EmptyPlpChunk);
                    }
                    let length = bytes(chunk.len())?;
                    if length > u32::MAX as usize {
                        return Err(Error::ValueTooLarge);
                    }
                    add(&mut size, 4)?;
                    add(&mut size, length)?;
                }
            }
            _ => return Err(Error::WrongRowKind),
        }
    }
    add(&mut size, 13)?; // DONE + status + command + u64 count
    Ok(Validated { name, table, size })
}

fn append_utf16(out: &mut Vec<u8>, units: &[u16]) {
    for unit in units {
        out.extend_from_slice(&unit.to_le_bytes());
    }
}

/// Append one complete success token stream atomically after validating every
/// length and row kind. ValidationError leaves `out` unchanged.
pub fn encode(out: &mut Vec<u8>, outcome: Outcome<'_>) -> Result<(), Error> {
    let Outcome::Success(plan) = outcome else {
        return Ok(());
    };
    let validated = validate(&plan)?;
    let mut encoded = Vec::new();
    encoded
        .try_reserve_exact(validated.size)
        .map_err(|_| Error::Allocation)?;
    encoded.extend_from_slice(&[0x81, 1, 0]);
    encoded.extend_from_slice(&0u32.to_le_bytes());
    encoded.extend_from_slice(&plan.flags.to_le_bytes());
    match plan.mode {
        Mode::NText {
            collation, table, ..
        } => {
            debug_assert!(!table.is_empty());
            encoded.push(0x63);
            encoded.extend_from_slice(&(NTEXT_MAX_BYTES as u32).to_le_bytes());
            encoded.extend_from_slice(&collation);
            encoded.push(1); // one table-name part
            encoded.extend_from_slice(&(validated.table.len() as u16).to_le_bytes());
            append_utf16(&mut encoded, &validated.table);
        }
        Mode::Xml => encoded.extend_from_slice(&[0xf1, 0]),
    }
    encoded.push(validated.name.len() as u8);
    append_utf16(&mut encoded, &validated.name);
    for row in plan.rows {
        encoded.push(0xd1);
        match (plan.mode, row) {
            (
                Mode::NText {
                    pointer, timestamp, ..
                },
                Row::NText(units),
            ) => {
                encoded.push(pointer.len() as u8);
                encoded.extend_from_slice(pointer);
                encoded.extend_from_slice(&timestamp);
                encoded.extend_from_slice(&(bytes(units.len())? as u32).to_le_bytes());
                append_utf16(&mut encoded, units);
            }
            (Mode::Xml, Row::Xml(chunks)) => {
                encoded.extend_from_slice(&(u64::MAX - 1).to_le_bytes());
                for chunk in *chunks {
                    encoded.extend_from_slice(&(bytes(chunk.len())? as u32).to_le_bytes());
                    append_utf16(&mut encoded, chunk);
                }
                encoded.extend_from_slice(&0u32.to_le_bytes());
            }
            _ => unreachable!("row kind was validated"),
        }
    }
    encoded.push(0xfd);
    encoded.extend_from_slice(&0x0010u16.to_le_bytes());
    encoded.extend_from_slice(&0x00c1u16.to_le_bytes());
    encoded.extend_from_slice(&plan.source_row_count.to_le_bytes());
    debug_assert_eq!(encoded.len(), validated.size);
    out.try_reserve(encoded.len())
        .map_err(|_| Error::Allocation)?;
    out.extend_from_slice(&encoded);
    Ok(())
}
