//! CONVERT date and time styles, in both directions.
//!
//! Formatting follows the outputs captured from SQL Server in
//! reference/gaps-conversion.json for every style and source type. Parsing
//! accepts the shapes those styles produce, with SQL Server's flexible
//! separators, one- and two-digit fields, month names and two-digit years
//! (cutoff 2049), and returns ISO 8601 text for the built-in conversions.
use crate::{
    datetime2::{DateTime2, Parts},
    datetimeoffset::DateTimeOffset,
};

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const TICKS_PER_SECOND: i64 = 10_000_000;
const TICKS_PER_DAY: i64 = 86_400 * TICKS_PER_SECOND;

/// The SQL Server type of a date or time value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Source {
    DateTime,
    SmallDateTime,
    Date,
    /// time(scale).
    Time(u8),
    DateTime2(u8),
    DateTimeOffset(u8),
}

impl Source {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::DateTime => "datetime",
            Self::SmallDateTime => "smalldatetime",
            Self::Date => "date",
            Self::Time(_) => "time",
            Self::DateTime2(_) => "datetime2",
            Self::DateTimeOffset(_) => "datetimeoffset",
        }
    }
}

/// A local date and time with an optional offset in minutes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Value {
    pub local: DateTime2,
    pub offset: Option<i16>,
}

/// Why a value cannot be formatted.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum FormatError {
    /// 281: the style does not exist for this source type.
    InvalidStyle,
    /// 8114: the style exists but does not apply to this source type.
    NotApplicable,
    /// A style msduck does not implement (the Hijri calendar).
    Unsupported,
}

/// Hijri month names as SQL Server writes them in style 130.
const HIJRI_MONTHS: [&str; 12] = [
    "\u{645}\u{62d}\u{631}\u{645}",
    "\u{635}\u{641}\u{631}",
    "\u{631}\u{628}\u{64a}\u{639} \u{627}\u{644}\u{627}\u{648}\u{644}",
    "\u{631}\u{628}\u{64a}\u{639} \u{627}\u{644}\u{62b}\u{627}\u{646}\u{64a}",
    "\u{62c}\u{645}\u{627}\u{62f}\u{649} \u{627}\u{644}\u{627}\u{648}\u{644}\u{649}",
    "\u{62c}\u{645}\u{627}\u{62f}\u{649} \u{627}\u{644}\u{62b}\u{627}\u{646}\u{64a}\u{629}",
    "\u{631}\u{62c}\u{628}",
    "\u{634}\u{639}\u{628}\u{627}\u{646}",
    "\u{631}\u{645}\u{636}\u{627}\u{646}",
    "\u{634}\u{648}\u{627}\u{644}",
    "\u{630}\u{648} \u{627}\u{644}\u{642}\u{639}\u{62f}\u{629}",
    "\u{630}\u{648} \u{627}\u{644}\u{62d}\u{62c}\u{629}",
];

/// The tabular (Kuwaiti) Hijri date SQL Server uses for styles 130 and 131:
/// .NET's HijriCalendar without adjustment.
fn hijri(value: DateTime2) -> Option<(i64, u8, u8)> {
    const EPOCH: i64 = 227_013;
    const MONTH_DAYS: [i64; 13] = [0, 30, 59, 89, 118, 148, 177, 207, 236, 266, 295, 325, 355];
    let leap = |year: i64| (year * 11 + 14) % 30 < 11;
    let days_in_year = |year: i64| if leap(year) { 355 } else { 354 };
    let days_up_to = |year: i64| {
        let cycles = (year - 1) / 30 * 30;
        let mut days = cycles * 10631 / 30 + EPOCH;
        let mut left = year - cycles - 1;
        while left > 0 {
            days += days_in_year(left);
            left -= 1;
        }
        days
    };
    let mut days = value.ticks() / TICKS_PER_DAY + 1;
    if days < EPOCH {
        return None;
    }
    let mut year = (days - EPOCH) * 30 / 10631 + 1;
    let mut start = days_up_to(year);
    let length = days_in_year(year);
    if days < start {
        start -= length;
        year -= 1;
    } else if days == start {
        year -= 1;
        start -= days_in_year(year);
    } else if days > start + length {
        start += length;
        year += 1;
    }
    days -= start;
    let mut month = 1;
    while month <= 12 && days > MONTH_DAYS[month - 1] {
        month += 1;
    }
    month -= 1;
    let day = days - MONTH_DAYS[month - 1];
    Some((year, month as u8, day as u8))
}

/// Valid date and time styles.
pub(super) fn valid(style: i32) -> bool {
    matches!(style, 0..=14 | 20..=25 | 100..=115 | 120 | 121 | 126 | 127 | 130 | 131)
}

/// Round a DATETIME value to its 1/300 second precision and return the
/// displayed milliseconds, carrying into the seconds when needed.
fn datetime_parts(value: DateTime2) -> (Parts, u32) {
    let ticks = value.ticks();
    let day = ticks - ticks % TICKS_PER_DAY;
    let within = ticks % TICKS_PER_DAY;
    // 1/300 second units, rounded half up.
    let units = (within as i128 * 300 + TICKS_PER_SECOND as i128 / 2) / TICKS_PER_SECOND as i128;
    let total = day as i128 + units * TICKS_PER_SECOND as i128 / 300;
    let rounded = DateTime2::from_ticks(total as i64).unwrap_or(value);
    let mut parts = rounded.parts();
    let millis = ((units % 300) * 1000 + 150) / 300;
    parts.fraction = 0;
    (parts, millis as u32)
}

fn two(n: impl Into<u32>) -> String {
    format!("{:02}", n.into())
}

/// Format a value with a CONVERT style.
pub(super) fn format(value: Value, source: Source, style: i32) -> Result<String, FormatError> {
    if !valid(style) {
        return Err(FormatError::InvalidStyle);
    }
    let legacy = matches!(source, Source::DateTime | Source::SmallDateTime);
    let (parts, millis) = match source {
        Source::DateTime => datetime_parts(value.local),
        Source::SmallDateTime => {
            let mut parts = value.local.parts();
            parts.second = 0;
            parts.fraction = 0;
            (parts, 0)
        }
        _ => (value.local.parts(), 0),
    };
    let scale = match source {
        Source::Time(scale) | Source::DateTime2(scale) | Source::DateTimeOffset(scale) => scale,
        _ => 0,
    };
    // The fractional second: `:mmm` or `.mmm` for the legacy types and a
    // dot with the declared scale's digits for the others.
    let fraction = |colon: bool| -> String {
        if legacy {
            format!("{}{millis:03}", if colon { ':' } else { '.' })
        } else if scale == 0 {
            String::new()
        } else {
            let digits = format!("{:07}", parts.fraction);
            format!(".{}", &digits[..usize::from(scale)])
        }
    };
    let date_only = source == Source::Date;
    let time_only = matches!(source, Source::Time(_));
    let yy = two((parts.year % 100) as u32);
    let yyyy = format!("{:04}", parts.year);
    let mm = two(parts.month);
    let dd = two(parts.day);
    let mon = MONTHS[usize::from(parts.month - 1)];
    let hh = two(parts.hour);
    let mi = two(parts.minute);
    let ss = two(parts.second);
    let hour12 = match parts.hour % 12 {
        0 => 12,
        h => h,
    };
    let meridian = if parts.hour < 12 { "AM" } else { "PM" };
    let offset = |minutes: i16| {
        let sign = if minutes < 0 { '-' } else { '+' };
        let minutes = minutes.unsigned_abs();
        format!("{sign}{:02}:{:02}", minutes / 60, minutes % 60)
    };
    // Styles with a time portion append the offset after a space.
    let zone = |text: String| match value.offset {
        Some(minutes) if source == Source::DateTimeOffset(scale) => {
            format!("{text} {}", offset(minutes))
        }
        _ => text,
    };
    let date_style = |text: String| {
        if time_only {
            Err(FormatError::NotApplicable)
        } else {
            Ok(text)
        }
    };
    let time_style = |text: String| {
        if date_only {
            Err(FormatError::NotApplicable)
        } else {
            Ok(zone(text))
        }
    };
    let iso_fraction = || {
        if legacy {
            if millis == 0 {
                String::new()
            } else {
                format!(".{millis:03}")
            }
        } else if parts.fraction == 0 {
            String::new()
        } else {
            fraction(false)
        }
    };
    match style {
        0 | 100 => Ok(if date_only {
            format!("{mon} {:>2} {yyyy}", parts.day)
        } else if time_only {
            format!("{hour12}:{mi}{meridian}")
        } else {
            zone(format!(
                "{mon} {:>2} {yyyy} {hour12:>2}:{mi}{meridian}",
                parts.day
            ))
        }),
        1 => date_style(format!("{mm}/{dd}/{yy}")),
        101 => date_style(format!("{mm}/{dd}/{yyyy}")),
        2 => date_style(format!("{yy}.{mm}.{dd}")),
        102 => date_style(format!("{yyyy}.{mm}.{dd}")),
        3 => date_style(format!("{dd}/{mm}/{yy}")),
        103 => date_style(format!("{dd}/{mm}/{yyyy}")),
        4 => date_style(format!("{dd}.{mm}.{yy}")),
        104 => date_style(format!("{dd}.{mm}.{yyyy}")),
        5 => date_style(format!("{dd}-{mm}-{yy}")),
        105 => date_style(format!("{dd}-{mm}-{yyyy}")),
        6 => date_style(format!("{dd} {mon} {yy}")),
        106 => date_style(format!("{dd} {mon} {yyyy}")),
        7 => date_style(format!("{mon} {dd}, {yy}")),
        107 => date_style(format!("{mon} {dd}, {yyyy}")),
        8 | 24 | 108 => time_style(format!("{hh}:{mi}:{ss}")),
        9 | 109 => Ok(if date_only {
            format!("{mon} {:>2} {yyyy}", parts.day)
        } else if time_only {
            format!("{hour12}:{mi}:{ss}{}{meridian}", fraction(true))
        } else {
            zone(format!(
                "{mon} {:>2} {yyyy} {hour12:>2}:{mi}:{ss}{}{meridian}",
                parts.day,
                fraction(true)
            ))
        }),
        10 => date_style(format!("{mm}-{dd}-{yy}")),
        110 => date_style(format!("{mm}-{dd}-{yyyy}")),
        11 => date_style(format!("{yy}/{mm}/{dd}")),
        111 => date_style(format!("{yyyy}/{mm}/{dd}")),
        12 => date_style(format!("{yy}{mm}{dd}")),
        112 => date_style(format!("{yyyy}{mm}{dd}")),
        13 | 113 => Ok(if date_only {
            format!("{dd} {mon} {yyyy}")
        } else if time_only {
            format!("{hh}:{mi}:{ss}{}", fraction(true))
        } else {
            zone(format!(
                "{dd} {mon} {yyyy} {hh}:{mi}:{ss}{}",
                fraction(true)
            ))
        }),
        14 | 114 => {
            if date_only {
                Err(FormatError::InvalidStyle)
            } else {
                Ok(zone(format!("{hh}:{mi}:{ss}{}", fraction(true))))
            }
        }
        20 | 120 => Ok(if date_only {
            format!("{yyyy}-{mm}-{dd}")
        } else if time_only {
            format!("{hh}:{mi}:{ss}")
        } else {
            zone(format!("{yyyy}-{mm}-{dd} {hh}:{mi}:{ss}"))
        }),
        21 | 25 | 121 => Ok(if date_only {
            format!("{yyyy}-{mm}-{dd}")
        } else if time_only {
            format!("{hh}:{mi}:{ss}{}", fraction(false))
        } else {
            zone(format!(
                "{yyyy}-{mm}-{dd} {hh}:{mi}:{ss}{}",
                fraction(false)
            ))
        }),
        22 => Ok(if date_only {
            format!("{mm}/{dd}/{yy}")
        } else if time_only {
            format!("{hour12:>2}:{mi}:{ss} {meridian}")
        } else {
            zone(format!("{mm}/{dd}/{yy} {hour12:>2}:{mi}:{ss} {meridian}"))
        }),
        23 => date_style(format!("{yyyy}-{mm}-{dd}")),
        115 => {
            if time_only {
                Err(FormatError::NotApplicable)
            } else if date_only {
                Ok("000000".into())
            } else {
                Ok(format!("{hh}{mi}{ss}"))
            }
        }
        126 | 127 => {
            if date_only {
                return Ok(format!("{yyyy}-{mm}-{dd}"));
            }
            if time_only {
                return Ok(format!("{hh}:{mi}:{ss}{}", iso_fraction()));
            }
            match (style, value.offset) {
                (127, Some(minutes)) if matches!(source, Source::DateTimeOffset(_)) => {
                    let utc = DateTime2::from_ticks(
                        value.local.ticks() - i64::from(minutes) * 60 * TICKS_PER_SECOND,
                    )
                    .map_err(|_| FormatError::Unsupported)?;
                    let utc = Value {
                        local: utc,
                        offset: None,
                    };
                    let text = format(utc, Source::DateTime2(scale), 126)?;
                    Ok(format!("{text}Z"))
                }
                (_, Some(minutes)) if matches!(source, Source::DateTimeOffset(_)) => Ok(format!(
                    "{yyyy}-{mm}-{dd}T{hh}:{mi}:{ss}{}{}",
                    iso_fraction(),
                    offset(minutes)
                )),
                _ => Ok(format!("{yyyy}-{mm}-{dd}T{hh}:{mi}:{ss}{}", iso_fraction())),
            }
        }
        130 | 131 => {
            let time = format!("{hour12:>2}:{mi}:{ss}{}{meridian}", fraction(true));
            if time_only {
                return Ok(time);
            }
            let (year, month, day) = hijri(value.local).ok_or(FormatError::Unsupported)?;
            let date = if style == 130 {
                format!("{day:>2} {} {year}", HIJRI_MONTHS[usize::from(month - 1)])
            } else {
                format!("{day:>2}/{month:02}/{year}")
            };
            Ok(if date_only {
                date
            } else {
                zone(format!("{date} {time}"))
            })
        }
        _ => Err(FormatError::InvalidStyle),
    }
}

/// The order of numeric date fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Order {
    Mdy,
    Dmy,
    Ymd,
}

fn order(style: i32) -> Order {
    match style {
        3 | 103 | 4 | 104 | 5 | 105 | 6 | 106 | 13 | 113 | 131 => Order::Dmy,
        2 | 102 | 11 | 111 | 12 | 112 | 20 | 120 | 21 | 121 | 23 | 25 | 126 | 127 => Order::Ymd,
        _ => Order::Mdy,
    }
}

/// The SQL Server type a string converts to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Target {
    Date,
    DateTime,
    SmallDateTime,
    DateTime2,
    DateTimeOffset,
    Time,
}

impl Target {
    pub(super) fn from_code(code: i32) -> Option<Self> {
        Some(match code {
            0 => Self::Date,
            1 => Self::DateTime,
            2 => Self::SmallDateTime,
            3 => Self::DateTime2,
            4 => Self::DateTimeOffset,
            5 => Self::Time,
            _ => return None,
        })
    }
    fn legacy(self) -> bool {
        matches!(self, Self::DateTime | Self::SmallDateTime)
    }
}

/// One token of a date/time string.
#[derive(Clone, Debug, PartialEq)]
enum Token {
    Number(String),
    Month(u8),
    Separator(char),
    Space,
}

fn tokens(text: &str) -> Option<Vec<Token>> {
    let mut out = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_ascii_digit() {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            out.push(Token::Number(chars[start..i].iter().collect()));
        } else if c.is_ascii_alphabetic() {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_alphabetic() {
                i += 1;
            }
            let word: String = chars[start..i]
                .iter()
                .collect::<String>()
                .to_ascii_lowercase();
            let month = [
                "january",
                "february",
                "march",
                "april",
                "may",
                "june",
                "july",
                "august",
                "september",
                "october",
                "november",
                "december",
            ]
            .iter()
            .position(|name| word.len() >= 3 && name.starts_with(&word))?;
            out.push(Token::Month(month as u8 + 1));
        } else if c == ' ' || c == '\t' {
            while i < chars.len() && (chars[i] == ' ' || chars[i] == '\t') {
                i += 1;
            }
            out.push(Token::Space);
        } else if matches!(c, '/' | '-' | '.' | ',') {
            out.push(Token::Separator(c));
            i += 1;
        } else {
            return None;
        }
    }
    Some(out)
}

fn year(text: &str) -> Option<u16> {
    let value: u16 = text.parse().ok()?;
    Some(match text.len() {
        1 | 2 => {
            if value < 50 {
                2000 + value
            } else {
                1900 + value
            }
        }
        3 | 4 => value,
        _ => return None,
    })
}

/// Parsed fields before range checks.
#[derive(Debug, Default)]
struct Fields {
    date: Option<(u16, u8, u8)>,
    hour: u8,
    minute: u8,
    second: u8,
    fraction: u32,
    offset: Option<i16>,
}

/// Parse the date part (everything before the time).
fn parse_date(text: &str, order: Order, target: Target) -> Option<(u16, u8, u8)> {
    let text = text.trim();
    let all_digits = !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    if all_digits {
        return match text.len() {
            8 => Some((
                text[..4].parse().ok()?,
                text[4..6].parse().ok()?,
                text[6..].parse().ok()?,
            )),
            6 => Some((
                year(&text[..2])?,
                text[2..4].parse().ok()?,
                text[4..].parse().ok()?,
            )),
            4 => Some((text.parse().ok()?, 1, 1)),
            _ => None,
        };
    }
    let tokens = tokens(text)?;
    let numbers: Vec<&String> = tokens
        .iter()
        .filter_map(|t| match t {
            Token::Number(n) => Some(n),
            _ => None,
        })
        .collect();
    let month = tokens.iter().find_map(|t| match t {
        Token::Month(m) => Some(*m),
        _ => None,
    });
    if let Some(month) = month {
        // A month name: the four-digit (or later) number is the year.
        return match numbers.as_slice() {
            [first, second] => {
                if first.len() >= 3 {
                    Some((year(first)?, month, second.parse().ok()?))
                } else {
                    Some((year(second)?, month, first.parse().ok()?))
                }
            }
            [only] if only.len() >= 3 => Some((year(only)?, month, 1)),
            _ => None,
        };
    }
    // Numeric fields with one kind of separator.
    let separators: Vec<char> = tokens
        .iter()
        .filter_map(|t| match t {
            Token::Separator(c) => Some(*c),
            _ => None,
        })
        .collect();
    if tokens.iter().any(|t| matches!(t, Token::Space)) {
        return None;
    }
    if separators.len() != numbers.len().saturating_sub(1)
        || separators.windows(2).any(|w| w[0] != w[1])
    {
        return None;
    }
    match numbers.as_slice() {
        [a, b, c] => {
            if a.len() >= 3 {
                // A leading year: the legacy types read the remaining
                // fields in the style's day/month order.
                let (m, d) = if target.legacy() && order == Order::Dmy {
                    (c, b)
                } else {
                    (b, c)
                };
                Some((year(a)?, m.parse().ok()?, d.parse().ok()?))
            } else {
                match order {
                    Order::Mdy => Some((year(c)?, a.parse().ok()?, b.parse().ok()?)),
                    Order::Dmy => Some((year(c)?, b.parse().ok()?, a.parse().ok()?)),
                    Order::Ymd => Some((year(a)?, b.parse().ok()?, c.parse().ok()?)),
                }
            }
        }
        [a, b] if a.len() >= 3 => Some((year(a)?, b.parse().ok()?, 1)),
        [a, b] if b.len() >= 3 => Some((year(b)?, a.parse().ok()?, 1)),
        _ => None,
    }
}

/// Parse `hh:mi[:ss[.fffffff|:mmm]][ ][AM|PM]`.
fn parse_time(text: &str, fields: &mut Fields) -> Option<()> {
    let mut text = text.trim().to_ascii_uppercase();
    let mut meridian = None;
    for (suffix, pm) in [("AM", false), ("PM", true)] {
        if let Some(rest) = text.strip_suffix(suffix) {
            meridian = Some(pm);
            text = rest.trim_end().to_owned();
            break;
        }
    }
    if text.is_empty() {
        return None;
    }
    let (clock, fraction) = match text.find('.') {
        Some(at) => (&text[..at], Some((&text[at + 1..], false))),
        None => (text.as_str(), None),
    };
    let mut pieces: Vec<&str> = clock.split(':').collect();
    let mut fraction = fraction;
    if fraction.is_none() && pieces.len() == 4 {
        fraction = Some((pieces.pop()?, true));
    }
    if !(2..=3).contains(&pieces.len()) || pieces.iter().any(|p| p.is_empty() || p.len() > 2) {
        return None;
    }
    let number = |p: &str| -> Option<u8> {
        if p.bytes().all(|b| b.is_ascii_digit()) {
            p.parse().ok()
        } else {
            None
        }
    };
    let mut hour = number(pieces[0])?;
    fields.minute = number(pieces[1])?;
    fields.second = match pieces.get(2) {
        Some(p) => number(p)?,
        None => 0,
    };
    if let Some((digits, colon)) = fraction {
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        fields.fraction = if colon {
            // `:mmm` is a whole number of milliseconds.
            if digits.len() > 3 {
                return None;
            }
            digits.parse::<u32>().ok()? * 10_000
        } else {
            let mut padded: String = digits.chars().take(7).collect();
            while padded.len() < 7 {
                padded.push('0');
            }
            let mut value: u32 = padded.parse().ok()?;
            // Digits beyond the seventh round the value.
            if digits.len() > 7 && digits.as_bytes()[7] >= b'5' {
                value += 1;
            }
            value
        };
    }
    if let Some(pm) = meridian {
        if hour == 0 || hour > 12 {
            return None;
        }
        hour %= 12;
        if pm {
            hour += 12;
        }
    }
    fields.hour = hour;
    Some(())
}

/// Split a trailing `Z` or `[ ]+hh:mm` offset.
fn split_offset(text: &str) -> Option<(&str, Option<i16>)> {
    let trimmed = text.trim_end();
    if let Some(rest) = trimmed.strip_suffix(['Z', 'z']) {
        return Some((rest, Some(0)));
    }
    let bytes = trimmed.as_bytes();
    if bytes.len() >= 6 {
        let tail = &trimmed[trimmed.len() - 6..];
        let tail_bytes = tail.as_bytes();
        if matches!(tail_bytes[0], b'+' | b'-')
            && tail_bytes[3] == b':'
            && tail[1..3].bytes().all(|b| b.is_ascii_digit())
            && tail[4..].bytes().all(|b| b.is_ascii_digit())
        {
            // A leading '-' could be a date separator ("2024-01-02" ends in
            // "-01-02" only when the text is that short; require a time or
            // space before the offset).
            let before = &trimmed[..trimmed.len() - 6];
            if before.contains(':') || before.ends_with(' ') {
                let hours: i16 = tail[1..3].parse().ok()?;
                let minutes: i16 = tail[4..].parse().ok()?;
                if hours > 14 || minutes > 59 || hours * 60 + minutes > 840 {
                    return None;
                }
                let total = hours * 60 + minutes;
                return Some((
                    before,
                    Some(if tail_bytes[0] == b'-' { -total } else { total }),
                ));
            }
        }
    }
    Some((trimmed, None))
}

/// Why a string cannot be converted.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum ParseError {
    /// 241: the text does not match.
    Syntax,
    /// 242: valid syntax with an out-of-range field (legacy types only;
    /// the other types report 241).
    Range,
}

/// Parse text with a CONVERT style for a date or time target.
pub(super) fn parse(text: &str, style: i32, target: Target) -> Result<Value, ParseError> {
    if !valid(style) || (target.legacy() && matches!(style, 22 | 23 | 25 | 130 | 131)) {
        return Err(ParseError::Syntax);
    }
    if matches!(style, 130 | 131) {
        return Err(ParseError::Syntax);
    }
    let text = text.trim();
    let mut fields = Fields::default();
    if text.is_empty() {
        fields.date = Some((1900, 1, 1));
    } else {
        let (rest, offset) = split_offset(text).ok_or(ParseError::Syntax)?;
        fields.offset = offset;
        let rest = rest.trim();
        // ISO 8601 with a T separator.
        let (date_text, time_text) =
            if let Some(at) = rest
                .find(['T', 't'])
                .filter(|at| rest[..*at].contains('-') && rest[*at + 1..].contains(':'))
            {
                (Some(&rest[..at]), Some(&rest[at + 1..]))
            } else if let Some(colon) = rest.find(':') {
                // The time starts at the last space before the first colon.
                match rest[..colon].rfind(' ') {
                    Some(space) => (Some(&rest[..space]), Some(&rest[space + 1..])),
                    None => (None, Some(rest)),
                }
            } else if let Some(at) = rest.to_ascii_uppercase().rfind(['A', 'P']).filter(|at| {
                rest.to_ascii_uppercase()[*at..].ends_with('M') && rest.len() - at == 2
            }) {
                // "3PM": a bare hour with a meridian.
                let hour_start = rest[..at].trim_end().rfind(' ').map_or(0, |s| s + 1);
                let date = rest[..hour_start].trim();
                fields.hour = rest[hour_start..at]
                    .trim()
                    .parse()
                    .map_err(|_| ParseError::Syntax)?;
                let pm = rest.to_ascii_uppercase()[at..].starts_with('P');
                if fields.hour == 0 || fields.hour > 12 {
                    return Err(ParseError::Syntax);
                }
                fields.hour %= 12;
                if pm {
                    fields.hour += 12;
                }
                (if date.is_empty() { None } else { Some(date) }, None)
            } else {
                (Some(rest), None)
            };
        if let Some(date_text) = date_text {
            let date_text = date_text.trim();
            if !date_text.is_empty() {
                fields.date =
                    Some(parse_date(date_text, order(style), target).ok_or(ParseError::Syntax)?);
            }
        }
        if let Some(time_text) = time_text {
            parse_time(time_text, &mut fields).ok_or(ParseError::Syntax)?;
        }
    }
    if fields.offset.is_some() && !matches!(target, Target::DateTimeOffset) && style != 127 {
        return Err(ParseError::Syntax);
    }
    let (year, month, day) = fields.date.unwrap_or((1900, 1, 1));
    let range = if target.legacy() {
        ParseError::Range
    } else {
        ParseError::Syntax
    };
    if !(1..=12).contains(&month) || day == 0 || day > 31 {
        return Err(range);
    }
    if fields.hour > 23 || fields.minute > 59 || fields.second > 59 {
        return Err(ParseError::Syntax);
    }
    let local = DateTime2::from_parts(Parts {
        year,
        month,
        day,
        hour: fields.hour,
        minute: fields.minute,
        second: fields.second,
        fraction: fields.fraction,
    })
    .map_err(|_| range)?;
    if target == Target::DateTime && year < 1753 {
        return Err(ParseError::Range);
    }
    let offset = match (target, fields.offset) {
        (Target::DateTimeOffset, offset) => Some(offset.unwrap_or(0)),
        _ => None,
    };
    // 127 with Z or an offset converts other targets to UTC.
    let local = match (target, fields.offset) {
        (Target::DateTimeOffset, _) | (_, None) => local,
        (_, Some(minutes)) => {
            DateTime2::from_ticks(local.ticks() - i64::from(minutes) * 60 * TICKS_PER_SECOND)
                .map_err(|_| ParseError::Syntax)?
        }
    };
    Ok(Value { local, offset })
}

/// ISO 8601 text that the built-in conversion to `target` reads exactly.
pub(super) fn iso(value: Value, target: Target) -> Option<String> {
    let render = |local: DateTime2| -> String {
        let p = local.parts();
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:07}",
            p.year, p.month, p.day, p.hour, p.minute, p.second, p.fraction
        )
    };
    Some(match target {
        Target::Date => {
            let p = value.local.parts();
            format!("{:04}-{:02}-{:02}", p.year, p.month, p.day)
        }
        Target::Time => {
            let p = value.local.parts();
            format!(
                "{:02}:{:02}:{:02}.{:07}",
                p.hour, p.minute, p.second, p.fraction
            )
        }
        Target::DateTime => {
            let (parts, millis) = datetime_parts(value.local);
            format!(
                "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{millis:03}",
                parts.year, parts.month, parts.day, parts.hour, parts.minute, parts.second
            )
        }
        Target::SmallDateTime => {
            // Round to the minute: 29.998 seconds and later round up.
            let ticks = value.local.ticks();
            let minute = 60 * TICKS_PER_SECOND;
            let within = ticks % minute;
            let base = ticks - within;
            let rounded = if within >= 29_998 * 10_000 {
                base + minute
            } else {
                base
            };
            let p = DateTime2::from_ticks(rounded).ok()?.parts();
            format!(
                "{:04}-{:02}-{:02}T{:02}:{:02}:00",
                p.year, p.month, p.day, p.hour, p.minute
            )
        }
        Target::DateTime2 => render(value.local),
        Target::DateTimeOffset => {
            let minutes = value.offset.unwrap_or(0);
            DateTimeOffset::from_local(value.local, minutes).ok()?;
            let sign = if minutes < 0 { '-' } else { '+' };
            let abs = minutes.unsigned_abs();
            format!(
                "{}{sign}{:02}:{:02}",
                render(value.local),
                abs / 60,
                abs % 60
            )
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(text: &str, offset: Option<i16>) -> Value {
        Value {
            local: DateTime2::parse_iso(text).unwrap(),
            offset,
        }
    }

    #[test]
    fn styles_format_like_sql_server() {
        let morning = value("2024-01-02T03:04:05.123", None);
        let evening = value("2024-12-31T23:59:59.997", None);
        for (style, a, b) in [
            (0, "Jan  2 2024  3:04AM", "Dec 31 2024 11:59PM"),
            (
                9,
                "Jan  2 2024  3:04:05:123AM",
                "Dec 31 2024 11:59:59:997PM",
            ),
            (13, "02 Jan 2024 03:04:05:123", "31 Dec 2024 23:59:59:997"),
            (22, "01/02/24  3:04:05 AM", "12/31/24 11:59:59 PM"),
            (107, "Jan 02, 2024", "Dec 31, 2024"),
            (121, "2024-01-02 03:04:05.123", "2024-12-31 23:59:59.997"),
            (126, "2024-01-02T03:04:05.123", "2024-12-31T23:59:59.997"),
            (127, "2024-01-02T03:04:05.123", "2024-12-31T23:59:59.997"),
            (115, "030405", "235959"),
        ] {
            assert_eq!(
                format(morning, Source::DateTime, style).unwrap(),
                a,
                "{style}"
            );
            assert_eq!(
                format(evening, Source::DateTime, style).unwrap(),
                b,
                "{style}"
            );
        }
        assert_eq!(
            format(morning, Source::DateTime, 99),
            Err(FormatError::InvalidStyle)
        );
        let offset = value("2024-01-02T03:04:05.1234567", Some(330));
        assert_eq!(
            format(offset, Source::DateTimeOffset(7), 127).unwrap(),
            "2024-01-01T21:34:05.1234567Z"
        );
        assert_eq!(
            format(offset, Source::DateTimeOffset(7), 126).unwrap(),
            "2024-01-02T03:04:05.1234567+05:30"
        );
        assert_eq!(
            format(offset, Source::DateTimeOffset(7), 0).unwrap(),
            "Jan  2 2024  3:04AM +05:30"
        );
        assert_eq!(
            format(offset, Source::DateTimeOffset(7), 101).unwrap(),
            "01/02/2024"
        );
        let date = value("2024-01-02T00:00:00", None);
        assert_eq!(
            format(date, Source::Date, 8),
            Err(FormatError::NotApplicable)
        );
        assert_eq!(
            format(date, Source::Date, 14),
            Err(FormatError::InvalidStyle)
        );
        assert_eq!(format(date, Source::Date, 115).unwrap(), "000000");
        let time = value("1900-01-01T03:04:05.1234567", None);
        assert_eq!(format(time, Source::Time(7), 0).unwrap(), "3:04AM");
        assert_eq!(format(time, Source::Time(7), 22).unwrap(), " 3:04:05 AM");
        assert_eq!(
            format(time, Source::Time(7), 101),
            Err(FormatError::NotApplicable)
        );
        assert_eq!(format(time, Source::Time(3), 9).unwrap(), "3:04:05.123AM");
        assert_eq!(
            format(morning, Source::DateTime, 131).unwrap(),
            "21/06/1445  3:04:05:123AM"
        );
        assert_eq!(
            format(evening, Source::DateTime, 131).unwrap(),
            " 1/07/1446 11:59:59:997PM"
        );
        assert_eq!(
            format(evening, Source::Date, 130).unwrap(),
            format!(" 1 {} 1446", HIJRI_MONTHS[6])
        );
    }

    #[test]
    fn styles_parse_like_sql_server() {
        let iso =
            |text: &str, style, target| iso(parse(text, style, target).unwrap(), target).unwrap();
        assert_eq!(
            iso("20240102", 112, Target::DateTime),
            "2024-01-02T00:00:00.000"
        );
        assert_eq!(
            iso("02/01/2024", 103, Target::DateTime),
            "2024-01-02T00:00:00.000"
        );
        assert_eq!(
            iso("1/2/2024", 101, Target::DateTime),
            "2024-01-02T00:00:00.000"
        );
        assert_eq!(
            iso("2/1/24", 3, Target::DateTime),
            "2024-01-02T00:00:00.000"
        );
        assert_eq!(
            iso("Jan 02, 2024", 107, Target::DateTime),
            "2024-01-02T00:00:00.000"
        );
        assert_eq!(
            iso("02 Jan 2024", 106, Target::DateTime),
            "2024-01-02T00:00:00.000"
        );
        assert_eq!(
            iso("Jan  2 2024  3:04:05:123AM", 109, Target::DateTime),
            "2024-01-02T03:04:05.123"
        );
        assert_eq!(
            iso("2024-01-02T03:04:05.123Z", 127, Target::DateTime),
            "2024-01-02T03:04:05.123"
        );
        assert_eq!(
            iso("2024-01-02", 103, Target::DateTime),
            "2024-02-01T00:00:00.000"
        );
        assert_eq!(iso("2024-01-02", 103, Target::Date), "2024-01-02");
        assert_eq!(iso("", 120, Target::DateTime), "1900-01-01T00:00:00.000");
        assert_eq!(
            iso("03:04:05:123", 114, Target::DateTime),
            "1900-01-01T03:04:05.123"
        );
        assert_eq!(
            iso(
                "2024-01-02T03:04:05.1234567+05:30",
                127,
                Target::DateTimeOffset
            ),
            "2024-01-02T03:04:05.1234567+05:30"
        );
        assert_eq!(
            iso("2024-01-02 03:04:05 +05:30", 120, Target::DateTimeOffset),
            "2024-01-02T03:04:05.0000000+05:30"
        );
        assert_eq!(
            iso("2024-01-02 03:04:31", 120, Target::SmallDateTime),
            "2024-01-02T03:05:00"
        );
        assert_eq!(
            parse("2024-01-02", 23, Target::DateTime),
            Err(ParseError::Syntax)
        );
        assert_eq!(
            parse("2024-13-02", 120, Target::DateTime),
            Err(ParseError::Range)
        );
        assert_eq!(
            parse("garbage", 120, Target::DateTime),
            Err(ParseError::Syntax)
        );
        assert_eq!(
            parse("31/02/2024", 103, Target::Date),
            Err(ParseError::Syntax)
        );
        assert_eq!(
            parse("2024-01-02", 99, Target::DateTime),
            Err(ParseError::Syntax)
        );
    }
}
