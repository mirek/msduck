//! Pure eight-byte rowversion values and explicit database-counter arithmetic.
//!
//! The caller owns the counter's persistence and decides when a row consumes a
//! value. In particular, a transaction rollback must not restore a counter that
//! was already allocated. This module does not choose a database's initial value.

/// A stored rowversion value. SQL Server sends its eight bytes in counter order.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RowVersion([u8; 8]);

/// The current counter for one database, supplied and persisted by the caller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DatabaseCounter(u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RowVersionError {
    WrongLength {
        actual: usize,
    },
    /// Local checked-arithmetic boundary; SQL Server overflow was not captured.
    CounterExhausted,
}

impl RowVersion {
    /// Read exactly eight bytes without interpreting them as a temporal value.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, RowVersionError> {
        let bytes: [u8; 8] = bytes.try_into().map_err(|_| RowVersionError::WrongLength {
            actual: bytes.len(),
        })?;
        Ok(Self(bytes))
    }

    pub fn bytes(self) -> [u8; 8] {
        self.0
    }
}

impl DatabaseCounter {
    /// Restore an explicit database counter, for example from persisted `@@DBTS`.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, RowVersionError> {
        Ok(Self(u64::from_be_bytes(
            RowVersion::from_bytes(bytes)?.bytes(),
        )))
    }

    /// The current eight-byte value; reading it never allocates a new version.
    pub fn current(self) -> RowVersion {
        RowVersion(self.0.to_be_bytes())
    }

    /// Allocate once and return both the state to persist and the value to store.
    pub fn allocate(self) -> Result<(Self, RowVersion), RowVersionError> {
        let next = self
            .0
            .checked_add(1)
            .ok_or(RowVersionError::CounterExhausted)?;
        let counter = Self(next);
        Ok((counter, counter.current()))
    }
}
