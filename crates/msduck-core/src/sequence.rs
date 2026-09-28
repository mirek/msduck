//! Deterministic sequence advancement over caller-owned state.
//!
//! Catalog identity, persistence, transaction behavior and synchronization
//! belong to the adapter that supplies this state. No process state is read.
use std::{collections::BTreeMap, fmt};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IntegerType {
    TinyInt,
    SmallInt,
    Int,
    BigInt,
}

impl IntegerType {
    pub const fn bounds(self) -> (i64, i64) {
        match self {
            Self::TinyInt => (0, u8::MAX as i64),
            Self::SmallInt => (i16::MIN as i64, i16::MAX as i64),
            Self::Int => (i32::MIN as i64, i32::MAX as i64),
            Self::BigInt => (i64::MIN, i64::MAX),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SequenceError {
    ZeroIncrement,
    InvalidBounds,
    StartOutsideBounds,
    InvalidState,
    Exhausted,
}

impl fmt::Display for SequenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for SequenceError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SequenceSpec {
    kind: IntegerType,
    start: i64,
    increment: i64,
    minimum: i64,
    maximum: i64,
    cycle: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SequenceState {
    current: i64,
    allocated: bool,
}

impl SequenceState {
    pub const fn current_value(self) -> i64 {
        self.current
    }

    pub const fn has_allocated(self) -> bool {
        self.allocated
    }
}

impl SequenceSpec {
    pub fn new(
        kind: IntegerType,
        start: i64,
        increment: i64,
        minimum: i64,
        maximum: i64,
        cycle: bool,
    ) -> Result<Self, SequenceError> {
        if increment == 0 {
            return Err(SequenceError::ZeroIncrement);
        }
        let (type_minimum, type_maximum) = kind.bounds();
        if minimum < type_minimum || maximum > type_maximum || minimum > maximum {
            return Err(SequenceError::InvalidBounds);
        }
        if !(minimum..=maximum).contains(&start) {
            return Err(SequenceError::StartOutsideBounds);
        }
        Ok(Self {
            kind,
            start,
            increment,
            minimum,
            maximum,
            cycle,
        })
    }

    pub const fn kind(self) -> IntegerType {
        self.kind
    }

    pub const fn initial_state(self) -> SequenceState {
        SequenceState {
            current: self.start,
            allocated: false,
        }
    }

    pub fn advance(self, state: &mut SequenceState) -> Result<i64, SequenceError> {
        if !(self.minimum..=self.maximum).contains(&state.current)
            || (!state.allocated && state.current != self.start)
        {
            return Err(SequenceError::InvalidState);
        }
        let value = if !state.allocated {
            self.start
        } else {
            // i128 keeps even i64::MIN + i64::MIN representable until the
            // configured bounds determine whether to cycle or exhaust.
            let candidate = i128::from(state.current) + i128::from(self.increment);
            if (i128::from(self.minimum)..=i128::from(self.maximum)).contains(&candidate) {
                i64::try_from(candidate).map_err(|_| SequenceError::InvalidState)?
            } else if self.cycle {
                if self.increment > 0 {
                    self.minimum
                } else {
                    self.maximum
                }
            } else {
                return Err(SequenceError::Exhausted);
            }
        };
        state.current = value;
        state.allocated = true;
        Ok(value)
    }
}

/// One instance per output row. The caller supplies a database-scoped catalog
/// identity and shared sequence state; repeated references to that identity
/// receive the same value without advancing it again.
#[derive(Debug, Default)]
pub struct RowAllocations {
    values: BTreeMap<u64, i64>,
}

impl RowAllocations {
    pub fn value_for(
        &mut self,
        identity: u64,
        spec: SequenceSpec,
        state: &mut SequenceState,
    ) -> Result<i64, SequenceError> {
        if let Some(value) = self.values.get(&identity) {
            return Ok(*value);
        }
        let value = spec.advance(state)?;
        self.values.insert(identity, value);
        Ok(value)
    }
}
