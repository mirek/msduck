//! Bounded text from explicit, already-stored SQL temporal/GUID values.
//! Source construction, result declarations and session acquisition are adapters.
use msduck_core::{
    datetime2::{DateTime2, Parts},
    datetimeoffset::DateTimeOffset,
    types::Type,
};

const SECOND: i64 = 10_000_000;
const DAY: i64 = 86_400 * SECOND;
const EPOCH_1900: i64 = 693_595;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    DefaultCast,
    ExplicitStyle(i32),
    ConcatWs,
    Translate,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    UsEnglish,
    French,
    German,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Domain {
    Cp1252,
    Utf16,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Contract {
    pub source: Type,
    pub profile: Profile,
    pub language: Language,
    pub domain: Domain,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stored {
    Date {
        days: u32,
    },
    Time {
        ticks: u64,
    },
    DateTime2(DateTime2),
    DateTimeOffset(DateTimeOffset),
    /// Signed days since 1900-01-01 and 1/300-second units within the day.
    DateTime {
        days: i32,
        ticks_300: u32,
    },
    /// Unsigned days since 1900-01-01 and minutes within the day.
    SmallDateTime {
        days: u32,
        minutes: u16,
    },
    /// SQL/TDS mixed-endian bytes, not canonical UUID hex byte order.
    Guid([u8; 16]),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Text {
    Ansi(String),
    Unicode(Vec<u16>),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    UnknownSource,
    UnknownProfile,
    UnknownLanguage,
    UnknownDomain,
    TypeMismatch,
    InvalidStoredValue,
}

/// Validate even a NULL value's original declaration and explicit context.
/// Modern/GUID non-English contexts are unmeasured in the retained reference.
pub fn contract(
    source: Type,
    profile: Profile,
    language: Language,
    domain: Domain,
) -> Result<Contract, Error> {
    if !matches!(
        source,
        Type::Date
            | Type::Time(_)
            | Type::DateTime2(_)
            | Type::DateTimeOffset(_)
            | Type::DateTime
            | Type::SmallDateTime
            | Type::UniqueIdentifier
    ) {
        return Err(Error::UnknownSource);
    }
    if matches!(profile, Profile::ExplicitStyle(style) if !matches!(style, 0 | 121)) {
        return Err(Error::UnknownProfile);
    }
    if language == Language::Unknown
        || (language != Language::UsEnglish
            && !matches!(source, Type::DateTime | Type::SmallDateTime))
    {
        return Err(Error::UnknownLanguage);
    }
    if domain == Domain::Unknown {
        return Err(Error::UnknownDomain);
    }
    Ok(Contract {
        source,
        profile,
        language,
        domain,
    })
}
fn day_value(days: i64) -> Result<DateTime2, Error> {
    let ticks = days.checked_mul(DAY).ok_or(Error::InvalidStoredValue)?;
    DateTime2::from_ticks(ticks).map_err(|_| Error::InvalidStoredValue)
}
fn aligned(value: i64, scale: u8) -> Result<(), Error> {
    if value % 10i64.pow(u32::from(7 - scale)) != 0 {
        return Err(Error::InvalidStoredValue);
    }
    Ok(())
}
fn components(source: Type, stored: Stored) -> Result<(Parts, Option<i16>, u8), Error> {
    Ok(match (source, stored) {
        (Type::Date, Stored::Date { days }) => (day_value(i64::from(days))?.parts(), None, 0),
        (Type::Time(scale), Stored::Time { ticks }) => {
            if ticks >= DAY as u64 {
                return Err(Error::InvalidStoredValue);
            }
            aligned(ticks as i64, scale.get())?;
            (
                DateTime2::from_ticks(ticks as i64)
                    .map_err(|_| Error::InvalidStoredValue)?
                    .parts(),
                None,
                scale.get(),
            )
        }
        (Type::DateTime2(scale), Stored::DateTime2(value)) => {
            aligned(value.ticks(), scale.get())?;
            (value.parts(), None, scale.get())
        }
        (Type::DateTimeOffset(scale), Stored::DateTimeOffset(value)) => {
            aligned(value.utc().ticks(), scale.get())?;
            (
                value.local().parts(),
                Some(value.offset_minutes()),
                scale.get(),
            )
        }
        (Type::DateTime, Stored::DateTime { days, ticks_300 }) => {
            if ticks_300 >= 86_400 * 300 {
                return Err(Error::InvalidStoredValue);
            }
            let mut parts = day_value(EPOCH_1900 + i64::from(days))?.parts();
            if parts.year < 1753 {
                return Err(Error::InvalidStoredValue);
            }
            let seconds = ticks_300 / 300;
            parts.hour = (seconds / 3600) as u8;
            parts.minute = (seconds / 60 % 60) as u8;
            parts.second = (seconds % 60) as u8;
            // Rendering an already-stored 1/300-second fraction as milliseconds,
            // not re-rounding the value or converting through an imprecise date.
            parts.fraction = ((ticks_300 % 300 * 1000 + 150) / 300) * 10_000;
            (parts, None, 3)
        }
        (Type::SmallDateTime, Stored::SmallDateTime { days, minutes }) => {
            if days > u32::from(u16::MAX) || minutes >= 1440 {
                return Err(Error::InvalidStoredValue);
            }
            let mut parts = day_value(EPOCH_1900 + i64::from(days))?.parts();
            parts.hour = (minutes / 60) as u8;
            parts.minute = (minutes % 60) as u8;
            (parts, None, 3)
        }
        _ => return Err(Error::TypeMismatch),
    })
}
fn month(language: Language, index: u8) -> &'static str {
    let names = match language {
        Language::UsEnglish => [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ],
        Language::French => [
            "janv", "févr", "mars", "avr", "mai", "juin", "juil", "août", "sept", "oct", "nov",
            "déc",
        ],
        Language::German => [
            "Jan", "Feb", "Mär", "Apr", "Mai", "Jun", "Jul", "Aug", "Sep", "Okt", "Nov", "Dez",
        ],
        Language::Unknown => unreachable!("contract validates explicit language"),
    };
    names[usize::from(index - 1)]
}
fn guid(mut bytes: [u8; 16]) -> String {
    bytes[..4].reverse();
    bytes[4..6].reverse();
    bytes[6..8].reverse();
    let mut text = String::with_capacity(36);
    use std::fmt::Write;
    for (index, byte) in bytes.iter().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            text.push('-');
        }
        write!(text, "{byte:02X}").expect("writing to bounded String");
    }
    text
}

/// NULL remains NULL. CONCAT_WS skipping and function composition belong outside.
/// Output is at most 36 Unicode code units for all supported profiles.
pub fn format(plan: Contract, value: Option<Stored>) -> Result<Option<Text>, Error> {
    contract(plan.source, plan.profile, plan.language, plan.domain)?;
    let Some(value) = value else { return Ok(None) };
    let text = if plan.source == Type::UniqueIdentifier {
        let Stored::Guid(bytes) = value else {
            return Err(Error::TypeMismatch);
        };
        guid(bytes)
    } else {
        let (parts, offset, scale) = components(plan.source, value)?;
        let month_clock = plan.profile == Profile::ExplicitStyle(0)
            || (matches!(plan.source, Type::DateTime | Type::SmallDateTime)
                && plan.profile != Profile::ExplicitStyle(121));
        let date_only = plan.source == Type::Date;
        let time_only = matches!(plan.source, Type::Time(_));
        let mut text = if month_clock {
            let hour = if parts.hour % 12 == 0 {
                12
            } else {
                parts.hour % 12
            };
            let meridian = if parts.hour < 12 { "AM" } else { "PM" };
            let clock = format!("{hour}:{:02}{meridian}", parts.minute);
            if time_only {
                clock
            } else {
                let date = format!(
                    "{} {:>2} {:04}",
                    month(plan.language, parts.month),
                    parts.day,
                    parts.year
                );
                if date_only {
                    date
                } else {
                    format!("{date} {hour:>2}:{:02}{meridian}", parts.minute)
                }
            }
        } else {
            let date = format!("{:04}-{:02}-{:02}", parts.year, parts.month, parts.day);
            let mut clock = format!("{:02}:{:02}:{:02}", parts.hour, parts.minute, parts.second);
            if scale > 0 {
                let fraction = parts.fraction / 10u32.pow(u32::from(7 - scale));
                clock.push_str(&format!(".{fraction:0width$}", width = usize::from(scale)));
            }
            if date_only {
                date
            } else if time_only {
                clock
            } else {
                format!("{date} {clock}")
            }
        };
        if let Some(offset) = offset {
            let sign = if offset < 0 { '-' } else { '+' };
            let magnitude = offset.unsigned_abs();
            text.push_str(&format!(
                " {sign}{:02}:{:02}",
                magnitude / 60,
                magnitude % 60
            ));
        }
        text
    };
    Ok(Some(match plan.domain {
        Domain::Cp1252 => {
            msduck_core::encoding::encode_cp1252(&text).map_err(|_| Error::UnknownDomain)?;
            Text::Ansi(text)
        }
        Domain::Utf16 => Text::Unicode(text.encode_utf16().collect()),
        Domain::Unknown => unreachable!("validated domain"),
    }))
}
