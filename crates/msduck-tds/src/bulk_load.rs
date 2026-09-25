//! Pure, bounded decoder for the TDS 7.2–7.4 BulkLoadBCP token stream.
//!
//! Values are retained as wire bytes. Type conversion, target binding, transaction
//! ownership and socket framing belong to the server adapter, not this codec.

const MAX_PENDING: usize = 16 * 1024 * 1024;
const MAX_COLUMNS: usize = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Malformed,
    UnsupportedType,
    EncryptedColumn,
    ColumnLimit,
    TokenLimit,
    NbcRow,
    NonFinalDone,
    TrailingBytes,
    TruncatedEom,
    MissingDone,
    Poisoned,
    Finished,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EomMode {
    RequireDone,
    /// FreeTDS/freebcp can terminate at a complete ROW boundary without DONE.
    AllowRowBoundary,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValueFormat {
    Fixed(usize),
    ByteLen { max: usize, exact: bool },
    ShortLen { max: usize, unicode: bool },
    LongLen { max: usize },
    LegacyLob { max: usize, unicode: bool },
    Plp { unicode: bool },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeInfo {
    pub id: u8,
    pub format: ValueFormat,
    pub precision: Option<u8>,
    pub scale: Option<u8>,
    pub collation: Option<[u8; 5]>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Column {
    pub name_utf16: Vec<u16>,
    pub user_type: u32,
    pub flags: u16,
    pub type_info: TypeInfo,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Value {
    /// None is SQL NULL; Some(empty) is an empty non-NULL value.
    pub bytes: Option<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Done {
    pub status: u16,
    pub command: u16,
    pub row_count: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Chunk {
    pub rows: Vec<Vec<Value>>,
    pub done: Option<Done>,
    pub finished: bool,
}

pub struct Decoder {
    pending: Vec<u8>,
    columns: Option<Vec<Column>>,
    done: Option<Done>,
    mode: EomMode,
    finished: bool,
    poisoned: bool,
}

impl Decoder {
    pub fn new(mode: EomMode) -> Self {
        Self {
            pending: Vec::new(),
            columns: None,
            done: None,
            mode,
            finished: false,
            poisoned: false,
        }
    }

    pub fn columns(&self) -> Option<&[Column]> {
        self.columns.as_deref()
    }

    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    pub fn push(&mut self, input: &[u8], eom: bool) -> Result<Chunk, Error> {
        if self.poisoned {
            return Err(Error::Poisoned);
        }
        if self.finished {
            return Err(Error::Finished);
        }
        match self.push_inner(input, eom) {
            Ok(chunk) => Ok(chunk),
            Err(error) => {
                self.poisoned = true;
                self.pending.clear();
                Err(error)
            }
        }
    }

    fn push_inner(&mut self, input: &[u8], eom: bool) -> Result<Chunk, Error> {
        let mut rows = Vec::new();
        let mut at = 0;
        loop {
            self.drain_tokens(&mut rows)?;
            if at == input.len() {
                break;
            }
            if self.done.is_some() {
                return Err(Error::TrailingBytes);
            }
            let room = MAX_PENDING - self.pending.len();
            if room == 0 {
                return Err(Error::TokenLimit);
            }
            let take = (input.len() - at).min(room).min(64 * 1024);
            self.pending.extend_from_slice(&input[at..at + take]);
            at += take;
        }
        if eom {
            if !self.pending.is_empty() || self.columns.is_none() {
                return Err(Error::TruncatedEom);
            }
            if self.done.is_none() && self.mode == EomMode::RequireDone {
                return Err(Error::MissingDone);
            }
            self.finished = true;
        }
        Ok(Chunk {
            rows,
            done: self.done,
            finished: self.finished,
        })
    }

    fn drain_tokens(&mut self, rows: &mut Vec<Vec<Value>>) -> Result<(), Error> {
        loop {
            if self.pending.is_empty() {
                return Ok(());
            }
            if self.done.is_some() {
                return Err(Error::TrailingBytes);
            }
            if self.columns.is_none() {
                match parse_metadata(&self.pending)? {
                    Some((columns, used)) => {
                        self.columns = Some(columns);
                        self.pending.drain(..used);
                    }
                    None => return Ok(()),
                }
                continue;
            }
            let columns = self.columns.as_deref().expect("metadata parsed");
            match self.pending[0] {
                0xd1 => match parse_row(&self.pending, columns)? {
                    Some((row, used)) => {
                        rows.push(row);
                        self.pending.drain(..used);
                    }
                    None => return Ok(()),
                },
                0xfd => match parse_done(&self.pending)? {
                    Some((done, used)) => {
                        self.done = Some(done);
                        self.pending.drain(..used);
                    }
                    None => return Ok(()),
                },
                0xd2 => return Err(Error::NbcRow),
                _ => return Err(Error::Malformed),
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReadError {
    More,
    Invalid(Error),
}

type ReadResult<T> = Result<T, ReadError>;

struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn take(&mut self, len: usize) -> ReadResult<&'a [u8]> {
        let end = self
            .at
            .checked_add(len)
            .ok_or(ReadError::Invalid(Error::TokenLimit))?;
        if end > MAX_PENDING {
            return Err(ReadError::Invalid(Error::TokenLimit));
        }
        if end > self.bytes.len() {
            return Err(ReadError::More);
        }
        let result = &self.bytes[self.at..end];
        self.at = end;
        Ok(result)
    }

    fn u8(&mut self) -> ReadResult<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> ReadResult<u16> {
        Ok(u16::from_le_bytes(
            self.take(2)?.try_into().expect("two bytes"),
        ))
    }
    fn u32(&mut self) -> ReadResult<u32> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("four bytes"),
        ))
    }
    fn u64(&mut self) -> ReadResult<u64> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().expect("eight bytes"),
        ))
    }
}

fn parsed<T>(result: ReadResult<T>) -> Result<Option<T>, Error> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(ReadError::More) => Ok(None),
        Err(ReadError::Invalid(error)) => Err(error),
    }
}

fn invalid(error: Error) -> ReadError {
    ReadError::Invalid(error)
}

fn parse_metadata(bytes: &[u8]) -> Result<Option<(Vec<Column>, usize)>, Error> {
    parsed(read_metadata(&mut Cursor::new(bytes)))
}

fn read_metadata(c: &mut Cursor<'_>) -> ReadResult<(Vec<Column>, usize)> {
    if c.u8()? != 0x81 {
        return Err(invalid(Error::Malformed));
    }
    let count = c.u16()?;
    if count == u16::MAX || usize::from(count) > MAX_COLUMNS {
        return Err(invalid(Error::ColumnLimit));
    }
    let mut columns = Vec::with_capacity(usize::from(count));
    for _ in 0..count {
        let user_type = c.u32()?;
        let flags = c.u16()?;
        if flags & 0x0800 != 0 {
            return Err(invalid(Error::EncryptedColumn));
        }
        let type_info = read_type_info(c)?;
        if matches!(type_info.format, ValueFormat::LegacyLob { .. }) {
            let parts = c.u8()?;
            for _ in 0..parts {
                let units = usize::from(c.u16()?);
                c.take(units.checked_mul(2).ok_or(invalid(Error::TokenLimit))?)?;
            }
        }
        let units = usize::from(c.u8()?);
        let name_utf16 = c
            .take(units * 2)?
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect();
        columns.push(Column {
            name_utf16,
            user_type,
            flags,
            type_info,
        });
    }
    Ok((columns, c.at))
}

fn read_type_info(c: &mut Cursor<'_>) -> ReadResult<TypeInfo> {
    let id = c.u8()?;
    let mut precision = None;
    let mut scale = None;
    let mut collation = None;
    let format = match id {
        0x30 | 0x32 => ValueFormat::Fixed(1),
        0x34 => ValueFormat::Fixed(2),
        0x38 | 0x3a | 0x3b | 0x7a => ValueFormat::Fixed(4),
        0x3c | 0x3d | 0x3e | 0x7f => ValueFormat::Fixed(8),
        0x24 | 0x26 | 0x68 | 0x6d | 0x6e | 0x6f => {
            let max = usize::from(c.u8()?);
            let valid = match id {
                0x24 => max == 16,
                0x26 => matches!(max, 1 | 2 | 4 | 8),
                0x68 => max == 1,
                _ => matches!(max, 4 | 8),
            };
            if !valid {
                return Err(invalid(Error::Malformed));
            }
            ValueFormat::ByteLen { max, exact: true }
        }
        0x6a | 0x6c => {
            let max = usize::from(c.u8()?);
            let p = c.u8()?;
            let s = c.u8()?;
            let expected = match p {
                1..=9 => 5,
                10..=19 => 9,
                20..=28 => 13,
                29..=38 => 17,
                _ => return Err(invalid(Error::Malformed)),
            };
            if max != expected || s > p {
                return Err(invalid(Error::Malformed));
            }
            precision = Some(p);
            scale = Some(s);
            ValueFormat::ByteLen { max, exact: true }
        }
        0x28 => ValueFormat::ByteLen {
            max: 3,
            exact: true,
        },
        0x29..=0x2b => {
            let s = c.u8()?;
            if s > 7 {
                return Err(invalid(Error::Malformed));
            }
            scale = Some(s);
            let time = if s <= 2 {
                3
            } else if s <= 4 {
                4
            } else {
                5
            };
            let max = time
                + match id {
                    0x29 => 0,
                    0x2a => 3,
                    _ => 5,
                };
            ValueFormat::ByteLen { max, exact: true }
        }
        0x25 | 0x27 | 0x2d | 0x2f => {
            let max = usize::from(c.u8()?);
            if matches!(id, 0x27 | 0x2f) {
                collation = Some(c.take(5)?.try_into().expect("five bytes"));
            }
            ValueFormat::ByteLen { max, exact: false }
        }
        0xa5 | 0xa7 | 0xad | 0xaf | 0xe7 | 0xef => {
            let max = usize::from(c.u16()?);
            let character = matches!(id, 0xa7 | 0xaf | 0xe7 | 0xef);
            let unicode = matches!(id, 0xe7 | 0xef);
            if character {
                collation = Some(c.take(5)?.try_into().expect("five bytes"));
            }
            if max == usize::from(u16::MAX) {
                if !matches!(id, 0xa5 | 0xa7 | 0xe7) {
                    return Err(invalid(Error::UnsupportedType));
                }
                ValueFormat::Plp { unicode }
            } else {
                if max > 8000 || (unicode && !max.is_multiple_of(2)) {
                    return Err(invalid(Error::Malformed));
                }
                ValueFormat::ShortLen { max, unicode }
            }
        }
        0x22 | 0x23 | 0x63 => {
            let max = c.u32()? as usize;
            let unicode = id == 0x63;
            if id != 0x22 {
                collation = Some(c.take(5)?.try_into().expect("five bytes"));
            }
            ValueFormat::LegacyLob { max, unicode }
        }
        0x62 => {
            let max = c.u32()? as usize;
            if max > 8009 {
                return Err(invalid(Error::Malformed));
            }
            ValueFormat::LongLen { max }
        }
        _ => return Err(invalid(Error::UnsupportedType)),
    };
    Ok(TypeInfo {
        id,
        format,
        precision,
        scale,
        collation,
    })
}

fn parse_row(bytes: &[u8], columns: &[Column]) -> Result<Option<(Vec<Value>, usize)>, Error> {
    parsed(read_row(&mut Cursor::new(bytes), columns))
}

fn read_row(c: &mut Cursor<'_>, columns: &[Column]) -> ReadResult<(Vec<Value>, usize)> {
    if c.u8()? != 0xd1 {
        return Err(invalid(Error::Malformed));
    }
    let mut row = Vec::with_capacity(columns.len());
    for column in columns {
        row.push(read_value(c, &column.type_info)?);
    }
    Ok((row, c.at))
}

fn read_value(c: &mut Cursor<'_>, ty: &TypeInfo) -> ReadResult<Value> {
    let bytes = match ty.format {
        ValueFormat::Fixed(len) => Some(c.take(len)?.to_vec()),
        ValueFormat::ByteLen { max, exact } => {
            let len = usize::from(c.u8()?);
            if len == 0 {
                None
            } else {
                if len > max || (exact && len != max) {
                    return Err(invalid(Error::Malformed));
                }
                Some(c.take(len)?.to_vec())
            }
        }
        ValueFormat::ShortLen { max, unicode } => {
            let len = usize::from(c.u16()?);
            if len == usize::from(u16::MAX) {
                None
            } else {
                if len > max || (unicode && !len.is_multiple_of(2)) {
                    return Err(invalid(Error::Malformed));
                }
                Some(c.take(len)?.to_vec())
            }
        }
        ValueFormat::LongLen { max } => {
            let len = c.u32()? as usize;
            if len == u32::MAX as usize {
                None
            } else {
                if len > max {
                    return Err(invalid(Error::Malformed));
                }
                Some(c.take(len)?.to_vec())
            }
        }
        ValueFormat::LegacyLob { max, unicode } => {
            let pointer_len = usize::from(c.u8()?);
            if pointer_len == 0 {
                None
            } else {
                c.take(pointer_len + 8)?;
                let len = c.u32()? as usize;
                if len > max || (unicode && !len.is_multiple_of(2)) {
                    return Err(invalid(Error::Malformed));
                }
                Some(c.take(len)?.to_vec())
            }
        }
        ValueFormat::Plp { unicode } => read_plp(c, unicode)?,
    };
    if matches!(ty.id, 0x6a | 0x6c)
        && bytes
            .as_ref()
            .is_some_and(|bytes| !matches!(bytes[0], 0 | 1))
    {
        return Err(invalid(Error::Malformed));
    }
    Ok(Value { bytes })
}

fn read_plp(c: &mut Cursor<'_>, unicode: bool) -> ReadResult<Option<Vec<u8>>> {
    let total = c.u64()?;
    if total == u64::MAX {
        return Ok(None);
    }
    if total != u64::MAX - 1 && total > MAX_PENDING as u64 {
        return Err(invalid(Error::TokenLimit));
    }
    let mut value = Vec::new();
    loop {
        let len = c.u32()? as usize;
        if len == 0 {
            break;
        }
        let next = value
            .len()
            .checked_add(len)
            .ok_or(invalid(Error::TokenLimit))?;
        if next > MAX_PENDING {
            return Err(invalid(Error::TokenLimit));
        }
        value.extend_from_slice(c.take(len)?);
    }
    if total != u64::MAX - 1 && total != value.len() as u64 {
        return Err(invalid(Error::Malformed));
    }
    if unicode && !value.len().is_multiple_of(2) {
        return Err(invalid(Error::Malformed));
    }
    Ok(Some(value))
}

fn parse_done(bytes: &[u8]) -> Result<Option<(Done, usize)>, Error> {
    parsed(read_done(&mut Cursor::new(bytes)))
}

fn read_done(c: &mut Cursor<'_>) -> ReadResult<(Done, usize)> {
    if c.u8()? != 0xfd {
        return Err(invalid(Error::Malformed));
    }
    let status = c.u16()?;
    let command = c.u16()?;
    let row_count = c.u64()?;
    if status & 1 != 0 {
        return Err(invalid(Error::NonFinalDone));
    }
    Ok((
        Done {
            status,
            command,
            row_count,
        },
        c.at,
    ))
}
