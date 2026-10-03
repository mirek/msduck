//! Lossless native ANSI payloads, independent of SQL conversion and TDS.
use std::fmt;

/// Caller-supplied semantic identity, not a declaration of codec support.
/// Opaque tags are caller identifiers; they never alias the named encodings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EncodingIdentity {
    Cp1252,
    Cp1251,
    Utf8,
    Opaque(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ByteError {
    LengthOverflow,
    Limit { requested: usize, maximum: usize },
}

impl fmt::Display for ByteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LengthOverflow => f.write_str("native ANSI byte count overflow"),
            Self::Limit { requested, maximum } => {
                write!(f, "native ANSI payload {requested} exceeds limit {maximum}")
            }
        }
    }
}
impl std::error::Error for ByteError {}

/// Checked payload accounting without mutating the caller's counter.
pub fn checked_byte_total(
    current: usize,
    additional: usize,
    maximum: usize,
) -> Result<usize, ByteError> {
    let requested = current
        .checked_add(additional)
        .ok_or(ByteError::LengthOverflow)?;
    if requested > maximum {
        return Err(ByteError::Limit { requested, maximum });
    }
    Ok(requested)
}

#[derive(PartialEq, Eq)]
pub struct AnsiBytes {
    encoding: EncodingIdentity,
    bytes: Vec<u8>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct AnsiView<'a> {
    encoding: EncodingIdentity,
    bytes: &'a [u8],
}

impl AnsiBytes {
    /// Take an existing allocation without copying after checking payload size.
    /// The limit bounds active payload bytes, not spare caller-owned capacity.
    pub fn from_vec(
        encoding: EncodingIdentity,
        bytes: Vec<u8>,
        maximum: usize,
    ) -> Result<Self, ByteError> {
        checked_byte_total(0, bytes.len(), maximum)?;
        Ok(Self { encoding, bytes })
    }

    /// Validate before copying borrowed bytes into a new allocation.
    pub fn from_slice(
        encoding: EncodingIdentity,
        bytes: &[u8],
        maximum: usize,
    ) -> Result<Self, ByteError> {
        AnsiView::new(encoding, bytes, maximum)?.try_to_owned(maximum)
    }

    pub fn view(&self) -> AnsiView<'_> {
        AnsiView {
            encoding: self.encoding,
            bytes: &self.bytes,
        }
    }

    pub fn into_parts(self) -> (EncodingIdentity, Vec<u8>) {
        (self.encoding, self.bytes)
    }
}

impl<'a> AnsiView<'a> {
    pub fn new(
        encoding: EncodingIdentity,
        bytes: &'a [u8],
        maximum: usize,
    ) -> Result<Self, ByteError> {
        checked_byte_total(0, bytes.len(), maximum)?;
        Ok(Self { encoding, bytes })
    }

    /// NULL has no payload; the declaration and encoding remain in its plan.
    /// Some(empty) is an empty non-NULL value, including at a zero-byte limit.
    pub fn nullable(
        encoding: EncodingIdentity,
        bytes: Option<&'a [u8]>,
        maximum: usize,
    ) -> Result<Option<Self>, ByteError> {
        bytes
            .map(|bytes| Self::new(encoding, bytes, maximum))
            .transpose()
    }

    pub fn encoding(self) -> EncodingIdentity {
        self.encoding
    }

    pub fn bytes(self) -> &'a [u8] {
        self.bytes
    }

    /// There is deliberately no unchecked Clone/ToOwned allocation path.
    pub fn try_to_owned(self, maximum: usize) -> Result<AnsiBytes, ByteError> {
        checked_byte_total(0, self.bytes.len(), maximum)?;
        Ok(AnsiBytes {
            encoding: self.encoding,
            bytes: self.bytes.to_vec(),
        })
    }
}

impl fmt::Debug for AnsiView<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AnsiBytes")
            .field("encoding", &self.encoding)
            .field("byte_len", &self.bytes.len())
            .field("prefix", &&self.bytes[..self.bytes.len().min(32)])
            .finish_non_exhaustive()
    }
}
impl fmt::Debug for AnsiBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.view().fmt(f)
    }
}
