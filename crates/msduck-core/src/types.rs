//! Logical SQL scalar declarations, independent of parser and storage types.
use crate::character::{CharacterType, Length};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Type {
    Bit,
    TinyInt,
    SmallInt,
    Int,
    BigInt,
    Real,
    Float,
    Decimal(DecimalType),
    Money,
    SmallMoney,
    Character(CharacterType),
    Binary(BinaryType),
    Date,
    DateTime,
    SmallDateTime,
    Time(Scale),
    DateTime2(Scale),
    DateTimeOffset(Scale),
    UniqueIdentifier,
    Text,
    Ntext,
    Image,
    Xml,
    Variant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecimalType {
    precision: u8,
    scale: u8,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scale(u8);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BinaryType {
    fixed: bool,
    length: Length,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    DecimalPrecision,
    DecimalScale,
    TemporalScale,
    BinaryLength,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::DecimalPrecision => "decimal precision must be between 1 and 38",
            Self::DecimalScale => "decimal scale exceeds precision",
            Self::TemporalScale => "temporal scale must be between 0 and 7",
            Self::BinaryLength => "invalid binary length",
        })
    }
}
impl std::error::Error for Error {}

impl DecimalType {
    pub fn new(precision: u8, scale: u8) -> Result<Self, Error> {
        if !(1..=38).contains(&precision) {
            return Err(Error::DecimalPrecision);
        }
        if scale > precision {
            return Err(Error::DecimalScale);
        }
        Ok(Self { precision, scale })
    }
    pub const fn precision(self) -> u8 {
        self.precision
    }
    pub const fn scale(self) -> u8 {
        self.scale
    }
    pub const fn storage_bytes(self) -> u8 {
        match self.precision {
            1..=9 => 5,
            10..=19 => 9,
            20..=28 => 13,
            _ => 17,
        }
    }
}
impl Scale {
    pub fn new(scale: u8) -> Result<Self, Error> {
        if scale > 7 {
            return Err(Error::TemporalScale);
        }
        Ok(Self(scale))
    }
    pub const fn get(self) -> u8 {
        self.0
    }
    pub const fn time_bytes(self) -> u8 {
        match self.0 {
            0..=2 => 3,
            3..=4 => 4,
            _ => 5,
        }
    }
}
impl BinaryType {
    pub fn new(fixed: bool, length: Length) -> Result<Self, Error> {
        match length {
            Length::Max if !fixed => {}
            Length::Bounded(1..=8000) => {}
            _ => return Err(Error::BinaryLength),
        }
        Ok(Self { fixed, length })
    }
    pub const fn fixed(self) -> bool {
        self.fixed
    }
    pub const fn length(self) -> Length {
        self.length
    }
}
impl Type {
    /// SQL storage width for fixed scalar families, not backend or wire size.
    pub const fn scalar_bytes(self) -> Option<u8> {
        Some(match self {
            Self::Bit | Self::TinyInt => 1,
            Self::SmallInt => 2,
            Self::Int | Self::Real | Self::SmallMoney | Self::SmallDateTime => 4,
            Self::BigInt | Self::Float | Self::Money | Self::DateTime => 8,
            Self::Decimal(d) => d.storage_bytes(),
            Self::Date => 3,
            Self::Time(s) => s.time_bytes(),
            Self::DateTime2(s) => s.time_bytes() + 3,
            Self::DateTimeOffset(s) => s.time_bytes() + 5,
            Self::UniqueIdentifier => 16,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn declarations_validate_parameters_and_sql_storage_widths() {
        for precision in 1..=38 {
            for scale in 0..=precision {
                let kind = DecimalType::new(precision, scale).unwrap();
                assert_eq!(kind.precision(), precision);
                assert_eq!(kind.scale(), scale);
                assert_eq!(
                    kind.storage_bytes(),
                    [5, 9, 13, 17][usize::from(precision > 9)
                        + usize::from(precision > 19)
                        + usize::from(precision > 28)]
                );
            }
        }
        assert!(DecimalType::new(0, 0).is_err());
        assert!(DecimalType::new(39, 0).is_err());
        assert!(DecimalType::new(2, 3).is_err());
        for (scale, width) in [3, 3, 3, 4, 4, 5, 5, 5].into_iter().enumerate() {
            let scale = Scale::new(scale as u8).unwrap();
            assert_eq!(Type::Time(scale).scalar_bytes(), Some(width));
            assert_eq!(Type::DateTime2(scale).scalar_bytes(), Some(width + 3));
            assert_eq!(Type::DateTimeOffset(scale).scalar_bytes(), Some(width + 5));
        }
        assert!(Scale::new(8).is_err());
        for fixed in [false, true] {
            for width in [1, 8000] {
                assert!(BinaryType::new(fixed, Length::Bounded(width)).is_ok());
            }
            for width in [0, 8001, u16::MAX] {
                assert!(BinaryType::new(fixed, Length::Bounded(width)).is_err());
            }
            assert_eq!(BinaryType::new(fixed, Length::Max).is_ok(), !fixed);
        }
        assert_ne!(Type::Money, Type::Decimal(DecimalType::new(19, 4).unwrap()));
        assert_ne!(Type::DateTime, Type::DateTime2(Scale::new(7).unwrap()));
    }
}
