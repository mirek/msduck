//! BulkLoad capacity after source declaration/wire admission, before storage.
//! This is not CAST/assignment policy or a whole-load transaction implementation.
use super::{
    CP1251_TO_CP1252, CP1251_TO_SQL_CHAR, CP1252_TO_CP1251, CP1252_TO_SQL_CHAR, ProjectedValue,
    ProjectionError, ProjectionLimits, ProjectionTarget, Resource, allocate, check_limit,
};
use crate::ansi_bytes::{AnsiBytes, AnsiView, ByteError, EncodingIdentity};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    Variable,
    Fixed,
}

/// Explicit source declaration form after root wire/declaration admission.
/// This fact must not be inferred from a row's bytes or current length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceForm {
    Bounded,
    Max,
}

/// Native targets count bytes; SQL UTF16 targets count two-byte units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Capacity {
    Bounded(usize),
    Max,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapacityError {
    Projection(ProjectionError),
    InvalidWidth {
        requested: usize,
        maximum: usize,
    },
    FixedMax,
    Truncation {
        source_bytes: usize,
        target_capacity: usize,
    },
}

impl From<ProjectionError> for CapacityError {
    fn from(error: ProjectionError) -> Self {
        Self::Projection(error)
    }
}
impl fmt::Display for CapacityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Projection(error) => error.fmt(f),
            Self::InvalidWidth { requested, maximum } => {
                write!(f, "ANSI capacity {requested} is outside 1..={maximum}")
            }
            Self::FixedMax => f.write_str("fixed character capacity cannot be MAX"),
            Self::Truncation {
                source_bytes,
                target_capacity,
            } => write!(
                f,
                "ANSI source payload {source_bytes} exceeds SQL capacity {target_capacity}"
            ),
        }
    }
}
impl std::error::Error for CapacityError {}

/// Validated declaration facts, independent of each row's nullable value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    source: EncodingIdentity,
    source_form: SourceForm,
    target: ProjectionTarget,
    family: Family,
    capacity: Capacity,
}

impl Plan {
    pub fn new(
        source: EncodingIdentity,
        source_form: SourceForm,
        target: ProjectionTarget,
        family: Family,
        capacity: Capacity,
    ) -> Result<Self, CapacityError> {
        if !matches!(source, EncodingIdentity::Cp1251 | EncodingIdentity::Cp1252) {
            return Err(ProjectionError::UnsupportedSource(source).into());
        }
        if let ProjectionTarget::Native(encoding) = target
            && !matches!(
                encoding,
                EncodingIdentity::Cp1251 | EncodingIdentity::Cp1252 | EncodingIdentity::Utf8
            )
        {
            return Err(ProjectionError::UnsupportedTarget(encoding).into());
        }
        if family == Family::Fixed && capacity == Capacity::Max {
            return Err(CapacityError::FixedMax);
        }
        if let Capacity::Bounded(width) = capacity {
            let maximum = if target == ProjectionTarget::SqlUtf16 {
                4000
            } else {
                8000
            };
            if width == 0 || width > maximum {
                return Err(CapacityError::InvalidWidth {
                    requested: width,
                    maximum,
                });
            }
        }
        Ok(Self {
            source,
            source_form,
            target,
            family,
            capacity,
        })
    }

    /// Apply to source bytes already admitted by the root declaration/wire plan.
    /// In particular, this cannot validate source CHAR-to-MAX or TYPE_INFO shape.
    /// Only the final payload is allocated; conversion expansion is preflighted.
    pub fn apply(
        self,
        value: Option<AnsiView<'_>>,
        limits: ProjectionLimits,
    ) -> Result<Option<ProjectedValue>, CapacityError> {
        let Some(value) = value else { return Ok(None) };
        if value.encoding() != self.source {
            return Err(ProjectionError::SourceEncodingMismatch {
                declared: self.source,
                actual: value.encoding(),
            }
            .into());
        }
        let input = value.bytes();
        check_limit(Resource::Input, input.len(), limits.input_bytes)?;
        if let Capacity::Bounded(width) = self.capacity
            && input.len() > width
        {
            // Bounded sources and same-codepage MAX conversion inspect all
            // overflow; cross-encoding MAX conversion has a shorter window.
            // Do not infer source form from values or replace this with
            // universal rtrim/a Unicode whitespace predicate.
            let end = if self.source_form == SourceForm::Bounded
                || self.target == ProjectionTarget::Native(self.source)
            {
                input.len()
            } else {
                width + width.min(input.len() - width)
            };
            if input[width..end].iter().any(|&byte| byte != b' ') {
                return Err(CapacityError::Truncation {
                    source_bytes: input.len(),
                    target_capacity: width,
                });
            }
        }
        let characters = if self.source == EncodingIdentity::Cp1251 {
            &CP1251_TO_SQL_CHAR
        } else {
            &CP1252_TO_SQL_CHAR
        };
        let mut prefix_len = 0usize;
        let mut payload_len = 0usize;
        for &byte in input {
            let size = if self.target == ProjectionTarget::Native(EncodingIdentity::Utf8) {
                characters[usize::from(byte)].len_utf8()
            } else {
                1
            };
            let next = payload_len
                .checked_add(size)
                .ok_or(ProjectionError::LengthOverflow)?;
            if matches!(self.capacity, Capacity::Bounded(width) if next > width) {
                break;
            }
            payload_len = next;
            prefix_len += 1;
        }
        let final_len = match (self.family, self.capacity) {
            (Family::Fixed, Capacity::Bounded(width)) => width,
            _ => payload_len,
        };
        let output_bytes = if self.target == ProjectionTarget::SqlUtf16 {
            final_len
                .checked_mul(2)
                .ok_or(ProjectionError::LengthOverflow)?
        } else {
            final_len
        };
        check_limit(Resource::Output, output_bytes, limits.output_bytes)?;
        let result = match self.target {
            ProjectionTarget::SqlUtf16 => {
                let mut units = allocate(final_len, output_bytes)?;
                units.extend(
                    input[..prefix_len]
                        .iter()
                        .map(|&byte| characters[usize::from(byte)] as u16),
                );
                units.resize(final_len, u16::from(b' '));
                ProjectedValue::SqlUtf16(units)
            }
            ProjectionTarget::Native(encoding) => {
                let mut bytes = allocate(final_len, output_bytes)?;
                match encoding {
                    EncodingIdentity::Cp1251 if self.source == EncodingIdentity::Cp1251 => {
                        bytes.extend_from_slice(&input[..prefix_len])
                    }
                    EncodingIdentity::Cp1251 => bytes.extend(
                        input[..prefix_len]
                            .iter()
                            .map(|&byte| CP1252_TO_CP1251[usize::from(byte)]),
                    ),
                    EncodingIdentity::Cp1252 if self.source == EncodingIdentity::Cp1252 => {
                        bytes.extend_from_slice(&input[..prefix_len])
                    }
                    EncodingIdentity::Cp1252 => bytes.extend(
                        input[..prefix_len]
                            .iter()
                            .map(|&byte| CP1251_TO_CP1252[usize::from(byte)]),
                    ),
                    EncodingIdentity::Utf8 => {
                        let mut encoded = [0; 4];
                        for &byte in &input[..prefix_len] {
                            bytes.extend_from_slice(
                                characters[usize::from(byte)]
                                    .encode_utf8(&mut encoded)
                                    .as_bytes(),
                            );
                        }
                    }
                    _ => unreachable!("validated capacity plan target"),
                }
                bytes.resize(final_len, b' ');
                let owned =
                    AnsiBytes::from_vec(encoding, bytes, limits.output_bytes).map_err(|error| {
                        match error {
                            ByteError::LengthOverflow => ProjectionError::LengthOverflow,
                            ByteError::Limit { requested, maximum } => ProjectionError::Limit {
                                resource: Resource::Output,
                                requested,
                                maximum,
                            },
                        }
                    })?;
                ProjectedValue::Native(owned)
            }
        };
        Ok(Some(result))
    }
}
