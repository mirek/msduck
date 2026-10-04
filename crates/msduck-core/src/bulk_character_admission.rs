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

impl Wire {
    /// Shapes admitted by the measured modern character rules. This does not
    /// establish source/target admission or interpret any current ROW payload.
    pub fn is_supported_shape(self) -> bool {
        match self.length {
            WireLength::BoundedBytes(bytes) => {
                bytes != 0
                    && bytes <= 8000
                    && (!matches!(self.family, Family::Nchar | Family::Nvarchar) || bytes % 2 == 0)
            }
            WireLength::Plp => matches!(self.family, Family::Varchar | Family::Nvarchar),
        }
    }
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
    if !wire.is_supported_shape() {
        return Metadata::Unknown;
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

/// Fixed-source spaces after an admitted original ROW. None means NULL,
/// variable source, or an invalid/oversized byte shape; validate ROW first.
/// Unicode spaces are units, not bytes. No payload is read or allocated here.
pub fn padding_units(declared: CharacterType, bytes: Option<usize>) -> Option<usize> {
    let (Length::Bounded(width), Some(bytes)) = (declared.length(), bytes) else {
        return None;
    };
    let units = match declared.family() {
        Family::Char => bytes,
        Family::Nchar if bytes % 2 == 0 => bytes / 2,
        _ => return None,
    };
    usize::from(width).checked_sub(units)
}
