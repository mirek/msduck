//! Source-aware legacy datetime values. No backend or wire effects.
//!
//! Character input here is explicitly ISO syntax, not the general SQL parser:
//! callers must resolve styles, language and DATEFORMAT before selecting this API.
use crate::{datetime2::DateTime2, diagnostic::SqlError};

const SECOND: i64 = 10_000_000;
const DAY: i64 = 86_400 * SECOND;
const EPOCH_DAY: i64 = 693_595;
const DATETIME_MIN_DAY: i64 = -53_690;
const DATETIME_MAX_DAY: i64 = 2_958_463;
const DATETIME_DAY_UNITS: i64 = 25_920_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CharacterKind {
    VarChar,
    NVarChar,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    DateTime,
    SmallDateTime,
}
impl Target {
    fn name(self) -> &'static str {
        match self {
            Self::DateTime => "datetime",
            Self::SmallDateTime => "smalldatetime",
        }
    }
    fn syntax(self) -> SqlError {
        match self {
            Self::DateTime => SqlError::new(
                241,
                1,
                "Conversion failed when converting date and/or time from character string.",
            ),
            Self::SmallDateTime => SqlError::new(
                295,
                3,
                "Conversion failed when converting character string to smalldatetime data type.",
            ),
        }
    }
    fn range(self, source: &str) -> SqlError {
        SqlError::new(
            242,
            3,
            format!(
                "The conversion of a {source} data type to a {} data type resulted in an out-of-range value.",
                self.name()
            ),
        )
    }
}

/// A validated legacy value; construction always applies SQL conversion rules.
/// Equality includes the target type. Cross-type SQL comparison must apply its
/// own type precedence, rather than comparing these storage representations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Value {
    target: Target,
    days: i32,
    units: u32,
}
impl Value {
    pub fn target(self) -> Target {
        self.target
    }
    /// Days since 1900-01-01 and either ticks of 1/300 second (DATETIME) or
    /// whole minutes (SMALLDATETIME). Values are already rounded for storage.
    pub fn storage_parts(self) -> (i32, u32) {
        (self.days, self.units)
    }

    /// Widen the stored value to DATETIME2(7). DATETIME's rational ticks round
    /// to the nearest 100ns unit; using its millisecond display would lose
    /// information needed by SQL mixed-type comparison and arithmetic.
    pub fn to_datetime2(self) -> DateTime2 {
        let time = match self.target {
            Target::DateTime => (i64::from(self.units) * SECOND + 150) / 300,
            Target::SmallDateTime => i64::from(self.units) * 60 * SECOND,
        };
        // Private fields are constructed only after range validation. The
        // widest result is below the end of 9999-12-31 and fits in i64.
        DateTime2::from_ticks((i64::from(self.days) + EPOCH_DAY) * DAY + time)
            .expect("validated legacy value lies inside DATETIME2 range")
    }
}

/// Convert an explicitly typed DATETIME2 value, with NULL preserved.
pub fn from_datetime2(target: Target, value: Option<DateTime2>) -> Result<Option<Value>, SqlError> {
    value
        .map(|value| convert(target, value, "datetime2", false))
        .transpose()
}

/// Convert ISO character input using legacy character-source rounding.
/// Other conversion styles and locale-dependent syntax are outside this API.
pub fn from_iso(
    target: Target,
    source: CharacterKind,
    text: Option<&str>,
) -> Result<Option<Value>, SqlError> {
    let Some(text) = text else { return Ok(None) };
    if text
        .split_once('.')
        .is_some_and(|(_, fraction)| fraction.len() > 3)
    {
        return Err(target.syntax());
    }
    let value = DateTime2::parse_iso(text).map_err(|_| target.syntax())?;
    let source = match source {
        CharacterKind::VarChar => "varchar",
        CharacterKind::NVarChar => "nvarchar",
    };
    convert(target, value, source, true).map(Some)
}

/// TRY conversion suppresses conversion failures, but does not choose a source
/// type, locale, style or unsupported conversion on behalf of the caller.
pub fn try_from_iso(target: Target, source: CharacterKind, text: Option<&str>) -> Option<Value> {
    from_iso(target, source, text).ok().flatten()
}

fn convert(
    target: Target,
    value: DateTime2,
    source: &str,
    character: bool,
) -> Result<Value, SqlError> {
    let mut days = value.ticks() / DAY - EPOCH_DAY;
    let time = value.ticks() % DAY;
    // Multiplication is bounded by one day of 100ns ticks, not the full date.
    let ticks = (time * 300 + SECOND / 2) / SECOND;
    let units = match target {
        Target::DateTime => {
            // Character conversion rejects the original date below 1753;
            // DATETIME2 conversion may round across that lower boundary.
            if character && days < DATETIME_MIN_DAY {
                return Err(target.range(source));
            }
            let mut units = ticks;
            if units == DATETIME_DAY_UNITS {
                if !character && days == DATETIME_MAX_DAY {
                    // The captured maximum DATETIME2 saturates at the final
                    // representable DATETIME tick rather than overflowing.
                    units -= 1;
                } else {
                    days += 1;
                    units = 0;
                }
            }
            if !(DATETIME_MIN_DAY..=DATETIME_MAX_DAY).contains(&days) {
                return Err(target.range(source));
            }
            units
        }
        Target::SmallDateTime => {
            let mut minutes = if character {
                // Legacy strings pass through 1/300-second rounding first.
                (ticks + 9_000) / 18_000
            } else {
                // A typed DATETIME2 source rounds directly at 30 seconds.
                (time + 30 * SECOND) / (60 * SECOND)
            };
            if minutes == 1_440 {
                days += 1;
                minutes = 0;
            }
            if !(0..=65_535).contains(&days) {
                return Err(target.range(source));
            }
            minutes
        }
    };
    Ok(Value {
        target,
        days: days as i32,
        units: units as u32,
    })
}
