//! Owned scalar bindings, independent of the parser, database and wire format.
//!
//! These are value carriers, not SQL declarations or SQL comparison semantics.
//! The declared type remains separate (for example text can carry a GUID or an
//! exact temporal literal). Integer widths and timestamp units are explicit so
//! adapters do not silently change precision or parameter inference.
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeUnit {
    Second,
    Millisecond,
    Microsecond,
    Nanosecond,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Boolean(bool),
    TinyInt(i8),
    UTinyInt(u8),
    SmallInt(i16),
    Int(i32),
    BigInt(i64),
    Float(f32),
    Double(f64),
    Decimal(Decimal),
    /// Signed units since 1970-01-01T00:00:00, without a time zone.
    Timestamp(TimeUnit, i64),
    Text(String),
    /// Unicode SQL text in UTF-16 units, including isolated surrogates.
    Unicode(Vec<u16>),
    Blob(Vec<u8>),
    /// Signed days since 1970-01-01.
    Date32(i32),
}

impl Value {
    /// Preserve every UTF-16 unit, using ordinary text whenever decoding is
    /// lossless. Logical SQL declarations remain separate from this carrier.
    pub fn from_utf16(units: Vec<u16>) -> Self {
        match String::from_utf16(&units) {
            Ok(text) => Self::Text(text),
            Err(_) => Self::Unicode(units),
        }
    }
}

/// An exact scaled integer with its decimal declaration retained.
/// Structural equality includes precision and scale; it is not SQL equality.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decimal {
    precision: u8,
    scale: u8,
    coefficient: i128,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecimalError {
    Precision,
    Scale,
    Overflow,
}

impl fmt::Display for DecimalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Precision => "decimal precision must be between 1 and 38",
            Self::Scale => "decimal scale exceeds precision",
            Self::Overflow => "decimal coefficient exceeds precision",
        })
    }
}
impl std::error::Error for DecimalError {}

impl Decimal {
    pub fn new(precision: u8, scale: u8, coefficient: i128) -> Result<Self, DecimalError> {
        if !(1..=38).contains(&precision) {
            return Err(DecimalError::Precision);
        }
        if scale > precision {
            return Err(DecimalError::Scale);
        }
        if coefficient.unsigned_abs() >= 10u128.pow(u32::from(precision)) {
            return Err(DecimalError::Overflow);
        }
        Ok(Self {
            precision,
            scale,
            coefficient,
        })
    }

    pub const fn precision(self) -> u8 {
        self.precision
    }
    pub const fn scale(self) -> u8 {
        self.scale
    }
    pub const fn coefficient(self) -> i128 {
        self.coefficient
    }
}

impl fmt::Display for Decimal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let divisor = 10u128.pow(u32::from(self.scale));
        let magnitude = self.coefficient.unsigned_abs();
        let sign = if self.coefficient < 0 { "-" } else { "" };
        if self.scale == 0 {
            write!(f, "{sign}{magnitude}")
        } else {
            write!(
                f,
                "{sign}{}.{:0width$}",
                magnitude / divisor,
                magnitude % divisor,
                width = usize::from(self.scale)
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_decimal_boundaries_and_formatting() {
        for precision in 1..=38 {
            let limit = 10i128.pow(u32::from(precision));
            for scale in 0..=precision {
                for coefficient in [0, 1, -1, limit - 1, 1 - limit] {
                    let decimal = Decimal::new(precision, scale, coefficient).unwrap();
                    assert_eq!(decimal.precision(), precision);
                    assert_eq!(decimal.scale(), scale);
                    assert_eq!(decimal.coefficient(), coefficient);
                    let text = decimal.to_string();
                    assert_eq!(text.replace('.', "").parse::<i128>().unwrap(), coefficient);
                    if scale != 0 {
                        assert_eq!(text.split_once('.').unwrap().1.len(), usize::from(scale));
                    }
                }
                for coefficient in [limit, -limit, i128::MIN, i128::MAX] {
                    assert_eq!(
                        Decimal::new(precision, scale, coefficient),
                        Err(DecimalError::Overflow)
                    );
                }
            }
        }
        assert_eq!(
            Decimal::new(38, 38, -1).unwrap().to_string(),
            "-0.00000000000000000000000000000000000001"
        );
        assert_eq!(Decimal::new(5, 2, -12345).unwrap().to_string(), "-123.45");
        assert_eq!(Decimal::new(5, 2, 0).unwrap().to_string(), "0.00");
        assert_eq!(Decimal::new(0, 0, 0), Err(DecimalError::Precision));
        assert_eq!(Decimal::new(39, 0, 0), Err(DecimalError::Precision));
        assert_eq!(Decimal::new(1, 2, 0), Err(DecimalError::Scale));
        assert_ne!(Decimal::new(1, 0, 1), Decimal::new(38, 0, 1));
    }
}

#[cfg(test)]
mod unicode_value_tests {
    use super::Value;
    #[test]
    fn utf16_values_normalize_only_when_no_units_are_lost() {
        assert_eq!(
            Value::from_utf16(vec![0xd83e]),
            Value::Unicode(vec![0xd83e])
        );
        assert_eq!(
            Value::from_utf16(vec![0xdd86, 0]),
            Value::Unicode(vec![0xdd86, 0])
        );
        assert_eq!(
            Value::from_utf16(vec![0xd83e, 0xdd86, 0]),
            Value::Text("🦆\0".into())
        );
        assert_eq!(Value::from_utf16(vec![]), Value::Text(String::new()));
    }
}
