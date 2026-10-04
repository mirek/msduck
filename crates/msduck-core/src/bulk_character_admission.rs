//! Character BulkLoad admission over explicit, already decoded facts.
//! Source-row capacity is independent of target conversion/storage capacity.
use crate::character::{CharacterType, Family, Length};

/// Wire lengths are bytes, including for Unicode declarations measured in units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireLength {
    BoundedBytes(u16),
    Plp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Wire {
    pub family: Family,
    pub length: WireLength,
    pub nullable: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Metadata {
    Admitted,
    /// SQL4816 state1: family or target nullability mismatch.
    FamilyOrNullability,
    /// SQL4816 state2: a MAX declaration or target requires PLP.
    MaxFraming,
    /// Invalid/unmeasured shape; this is not a fabricated SQL diagnostic.
    Unknown,
}

pub fn metadata(
    declared: CharacterType,
    wire: Wire,
    target_nullable: bool,
    target_max: bool,
) -> Metadata {
    match wire.length {
        WireLength::BoundedBytes(bytes)
            if bytes == 0
                || bytes > 8000
                || (matches!(wire.family, Family::Nchar | Family::Nvarchar) && bytes % 2 != 0) =>
        {
            return Metadata::Unknown;
        }
        WireLength::Plp if matches!(wire.family, Family::Char | Family::Nchar) => {
            return Metadata::Unknown;
        }
        _ => {}
    }
    if declared.family() != wire.family || wire.nullable != target_nullable {
        return Metadata::FamilyOrNullability;
    }
    if (declared.length() == Length::Max || target_max) && wire.length != WireLength::Plp {
        return Metadata::MaxFraming;
    }
    Metadata::Admitted
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    Admitted,
    /// SQL4815 state1 severity17. Even trailing padding counts as source bytes.
    DeclaredLengthExceeded,
}

/// Validate original byte count before conversion; None denotes SQL NULL.
pub fn row(declared: CharacterType, bytes: Option<usize>) -> Row {
    let (Length::Bounded(width), Some(bytes)) = (declared.length(), bytes) else {
        return Row::Admitted;
    };
    let width = usize::from(width)
        * if matches!(declared.family(), Family::Nchar | Family::Nvarchar) {
            2
        } else {
            1
        };
    if bytes > width {
        Row::DeclaredLengthExceeded
    } else {
        Row::Admitted
    }
}
