//! Bounded, deterministic decoding of one TDS 7.3+ RPC TVP parameter.
//!
//! The input begins with TVPTYPE (0xF3), after RPC parameter name/status.
//! Cell bytes are borrowed without conversion. Catalog binding, SQL type
//! validation, and execution belong to the server adapter.

const DEFAULT_COLUMN_FLAG: u16 = 0x0200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_input_bytes: usize,
    pub max_columns: usize,
    pub max_rows: usize,
    pub max_cells: usize,
    pub max_cell_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_input_bytes: 4 * 1024 * 1024,
            max_columns: 1024,
            max_rows: 10_000,
            max_cells: 100_000,
            max_cell_bytes: 65_534,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InputLimit,
    Truncated,
    InvalidType,
    InvalidDatabaseName,
    InvalidIdentifier,
    InvalidColumnCount,
    ColumnLimit,
    RowLimit,
    CellLimit,
    InvalidColumnName,
    InvalidColumnType,
    UnsupportedColumnType(u8),
    UnsupportedMetadata(u8),
    InvalidCellLength,
    UnexpectedToken(u8),
    TrailingBytes,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeName {
    /// Raw UTF-16 code units; no locale or identifier normalization is applied.
    pub schema: Vec<u16>,
    pub name: Vec<u16>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ColumnType {
    IntN { width: u8 },
    NVarChar { max_bytes: u16, collation: [u8; 5] },
    VarBinary { max_bytes: u16 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Column {
    pub user_type: u32,
    pub flags: u16,
    pub column_type: ColumnType,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cell<'a> {
    /// The column's fDefault flag suppresses its value on the wire.
    Default,
    Null,
    Bytes(&'a [u8]),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value<'a> {
    /// Wire null marker. SQL Server may reject this for an input RPC parameter.
    Null,
    Table {
        columns: Vec<Column>,
        rows: Vec<Vec<Cell<'a>>>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tvp<'a> {
    pub type_name: TypeName,
    pub value: Value<'a>,
}

/// Decode one TVP at the start of `input`, returning the number of consumed bytes.
/// A caller parsing a larger RPC request may use the count to find the next parameter.
pub fn decode_prefix(input: &[u8], limits: Limits) -> Result<(Tvp<'_>, usize), Error> {
    if input.len() > limits.max_input_bytes {
        return Err(Error::InputLimit);
    }
    let mut c = Cursor { input, offset: 0 };
    if c.byte()? != 0xf3 {
        return Err(Error::InvalidType);
    }
    if !c.b_varchar()?.is_empty() {
        return Err(Error::InvalidDatabaseName);
    }
    let type_name = TypeName {
        schema: c.b_varchar()?,
        name: c.b_varchar()?,
    };
    let count = c.u16()?;
    if count == u16::MAX {
        expect_end(&mut c)?;
        expect_end(&mut c)?;
        return Ok((
            Tvp {
                type_name,
                value: Value::Null,
            },
            c.offset,
        ));
    }
    let count = usize::from(count);
    if count == 0 {
        return Err(Error::InvalidColumnCount);
    }
    if count > 1024 || count > limits.max_columns {
        return Err(Error::ColumnLimit);
    }
    let mut columns = Vec::with_capacity(count);
    for _ in 0..count {
        let user_type = c.u32()?;
        let flags = c.u16()?;
        let column_type = match c.byte()? {
            0x26 => {
                let width = c.byte()?;
                if !matches!(width, 1 | 2 | 4 | 8) {
                    return Err(Error::InvalidColumnType);
                }
                ColumnType::IntN { width }
            }
            0xe7 => {
                let max_bytes = c.u16()?;
                if max_bytes == u16::MAX || max_bytes % 2 != 0 {
                    return Err(Error::InvalidColumnType);
                }
                let collation = c.take(5)?.try_into().map_err(|_| Error::Truncated)?;
                ColumnType::NVarChar {
                    max_bytes,
                    collation,
                }
            }
            0xa5 => {
                let max_bytes = c.u16()?;
                if max_bytes == u16::MAX {
                    return Err(Error::InvalidColumnType);
                }
                ColumnType::VarBinary { max_bytes }
            }
            id => return Err(Error::UnsupportedColumnType(id)),
        };
        if !c.b_varchar()?.is_empty() {
            return Err(Error::InvalidColumnName);
        }
        columns.push(Column {
            user_type,
            flags,
            column_type,
        });
    }
    match c.byte()? {
        0 => {}
        token @ (0x10 | 0x11) => return Err(Error::UnsupportedMetadata(token)),
        token => return Err(Error::UnexpectedToken(token)),
    }
    let mut rows = Vec::new();
    loop {
        match c.byte()? {
            0 => break,
            1 => {
                if rows.len() >= limits.max_rows {
                    return Err(Error::RowLimit);
                }
                let next_cells = rows
                    .len()
                    .checked_add(1)
                    .and_then(|n| n.checked_mul(count))
                    .ok_or(Error::CellLimit)?;
                if next_cells > limits.max_cells {
                    return Err(Error::CellLimit);
                }
                let mut row = Vec::with_capacity(count);
                for column in &columns {
                    row.push(read_cell(&mut c, column, limits.max_cell_bytes)?);
                }
                rows.push(row);
            }
            token => return Err(Error::UnexpectedToken(token)),
        }
    }
    Ok((
        Tvp {
            type_name,
            value: Value::Table { columns, rows },
        },
        c.offset,
    ))
}

/// Decode an isolated RPC TVP value and reject any following bytes.
pub fn decode_exact(input: &[u8], limits: Limits) -> Result<Tvp<'_>, Error> {
    let (value, used) = decode_prefix(input, limits)?;
    if used != input.len() {
        return Err(Error::TrailingBytes);
    }
    Ok(value)
}

fn expect_end(c: &mut Cursor<'_>) -> Result<(), Error> {
    match c.byte()? {
        0 => Ok(()),
        token => Err(Error::UnexpectedToken(token)),
    }
}

fn read_cell<'a>(c: &mut Cursor<'a>, column: &Column, limit: usize) -> Result<Cell<'a>, Error> {
    if column.flags & DEFAULT_COLUMN_FLAG != 0 {
        return Ok(Cell::Default);
    }
    let (length, max_bytes, unicode) = match column.column_type {
        ColumnType::IntN { width } => {
            let length = usize::from(c.byte()?);
            if length == 0 {
                return Ok(Cell::Null);
            }
            (length, usize::from(width), false)
        }
        ColumnType::NVarChar { max_bytes, .. } => {
            let length = c.u16()?;
            if length == u16::MAX {
                return Ok(Cell::Null);
            }
            (usize::from(length), usize::from(max_bytes), true)
        }
        ColumnType::VarBinary { max_bytes } => {
            let length = c.u16()?;
            if length == u16::MAX {
                return Ok(Cell::Null);
            }
            (usize::from(length), usize::from(max_bytes), false)
        }
    };
    if length > limit {
        return Err(Error::CellLimit);
    }
    if length > max_bytes || (unicode && length % 2 != 0) {
        return Err(Error::InvalidCellLength);
    }
    if matches!(column.column_type, ColumnType::IntN { .. }) && length != max_bytes {
        return Err(Error::InvalidCellLength);
    }
    Ok(Cell::Bytes(c.take(length)?))
}

struct Cursor<'a> {
    input: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, length: usize) -> Result<&'a [u8], Error> {
        let end = self.offset.checked_add(length).ok_or(Error::Truncated)?;
        let bytes = self.input.get(self.offset..end).ok_or(Error::Truncated)?;
        self.offset = end;
        Ok(bytes)
    }

    fn byte(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, Error> {
        let bytes = self.take(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn u32(&mut self) -> Result<u32, Error> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn b_varchar(&mut self) -> Result<Vec<u16>, Error> {
        let count = usize::from(self.byte()?);
        if count > 128 {
            return Err(Error::InvalidIdentifier);
        }
        let mut units = Vec::with_capacity(count);
        for _ in 0..count {
            units.push(self.u16()?);
        }
        Ok(units)
    }
}
