//! Exact SQL Server DATETIME2 values and bare TDS value encoding.
use anyhow::{Result, bail, ensure};

const SECOND: i64 = 10_000_000;
const DAY: i64 = 86_400 * SECOND;
const LAST_DAY: i64 = 3_652_058;
const EPOCH_DAY: i64 = 719_162;

/// A date and time in 100ns ticks since 0001-01-01, without a timezone.
/// The complete SQL Server range fits in a signed 64-bit integer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DateTime2(i64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Parts {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    /// Fraction of a second in 100ns units.
    pub fraction: u32,
}

fn year_days(year: u16) -> i64 {
    let previous = i64::from(year) - 1;
    previous * 365 + previous / 4 - previous / 100 + previous / 400
}
fn month_days(year: u16, month: u8) -> u8 {
    match month {
        2 if year.is_multiple_of(400) || (year.is_multiple_of(4) && !year.is_multiple_of(100)) => {
            29
        }
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}
fn quantum(scale: u8) -> Result<i64> {
    ensure!(scale <= 7, "DATETIME2 scale must be between 0 and 7");
    Ok(10i64.pow(u32::from(7 - scale)))
}
fn time_width(scale: u8) -> Result<usize> {
    quantum(scale)?;
    Ok(match scale {
        0..=2 => 3,
        3..=4 => 4,
        _ => 5,
    })
}

impl DateTime2 {
    pub fn from_ticks(ticks: i64) -> Result<Self> {
        ensure!(
            (0..(LAST_DAY + 1) * DAY).contains(&ticks),
            "DATETIME2 outside 0001-01-01 through 9999-12-31"
        );
        Ok(Self(ticks))
    }
    pub fn ticks(self) -> i64 {
        self.0
    }
    pub fn from_parts(parts: Parts) -> Result<Self> {
        let Parts {
            year,
            month,
            day,
            hour,
            minute,
            second,
            fraction,
        } = parts;
        ensure!(
            (1..=9999).contains(&year) && (1..=12).contains(&month),
            "invalid DATETIME2 date"
        );
        ensure!(
            day >= 1 && day <= month_days(year, month),
            "invalid DATETIME2 date"
        );
        ensure!(
            hour < 24 && minute < 60 && second < 60 && fraction < SECOND as u32,
            "invalid DATETIME2 time"
        );
        let days = year_days(year)
            + (1..month)
                .map(|m| i64::from(month_days(year, m)))
                .sum::<i64>()
            + i64::from(day)
            - 1;
        Self::from_ticks(
            days * DAY
                + (i64::from(hour) * 3600 + i64::from(minute) * 60 + i64::from(second)) * SECOND
                + i64::from(fraction),
        )
    }
    pub fn parts(self) -> Parts {
        let days = self.0 / DAY;
        let (mut low, mut high) = (1u16, 10000u16);
        while low + 1 < high {
            let middle = low + (high - low) / 2;
            if year_days(middle) <= days {
                low = middle;
            } else {
                high = middle;
            }
        }
        let mut remaining = days - year_days(low);
        let mut month = 1;
        while remaining >= i64::from(month_days(low, month)) {
            remaining -= i64::from(month_days(low, month));
            month += 1;
        }
        let time = self.0 % DAY;
        Parts {
            year: low,
            month,
            day: (remaining + 1) as u8,
            hour: (time / SECOND / 3600) as u8,
            minute: (time / SECOND / 60 % 60) as u8,
            second: (time / SECOND % 60) as u8,
            fraction: (time % SECOND) as u32,
        }
    }
    /// Round half up at the requested scale, including day/year carry.
    pub fn round(self, scale: u8) -> Result<Self> {
        let q = quantum(scale)?;
        Self::from_ticks((self.0 + q / 2) / q * q)
    }
    /// Convert a Unix timestamp without narrowing to a nanosecond i64.
    pub fn from_unix_nanos(nanos: i128) -> Result<Self> {
        let rounded = nanos
            .checked_add(50)
            .ok_or_else(|| anyhow::anyhow!("timestamp overflow"))?
            .div_euclid(100);
        let absolute = rounded
            .checked_add(i128::from(EPOCH_DAY) * i128::from(DAY))
            .ok_or_else(|| anyhow::anyhow!("timestamp overflow"))?;
        Self::from_ticks(i64::try_from(absolute)?)
    }
    /// Parse ISO date or date-time text with at most seven fractional digits.
    /// Locale-dependent SQL conversion formats are intentionally not handled here.
    pub fn parse_iso(text: &str) -> Result<Self> {
        // A time-only literal supplies SQL Server's default calendar date.
        // The bounded prefix prevents malformed/unbounded input allocation;
        // the ordinary parser below validates every clock field and fraction.
        if text.is_ascii() && text.as_bytes().get(2) == Some(&b':') && text.len() <= 16 {
            let seconds = if text.len() == 5 { ":00" } else { "" };
            return Self::parse_iso(&format!("1900-01-01T{text}{seconds}"));
        }
        let bytes = text.as_bytes();
        ensure!(
            text.is_ascii() && (bytes.len() == 10 || bytes.len() >= 19),
            "invalid ISO DATETIME2"
        );
        ensure!(
            bytes[4] == b'-' && bytes[7] == b'-',
            "invalid ISO DATETIME2 date"
        );
        fn number(text: &str) -> Result<u32> {
            ensure!(
                !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()),
                "invalid ISO DATETIME2 number"
            );
            Ok(text.parse()?)
        }
        let mut parts = Parts {
            year: number(&text[..4])? as u16,
            month: number(&text[5..7])? as u8,
            day: number(&text[8..10])? as u8,
            hour: 0,
            minute: 0,
            second: 0,
            fraction: 0,
        };
        if bytes.len() > 10 {
            ensure!(
                matches!(bytes[10], b'T' | b' ') && bytes[13] == b':' && bytes[16] == b':',
                "invalid ISO DATETIME2 time"
            );
            parts.hour = number(&text[11..13])? as u8;
            parts.minute = number(&text[14..16])? as u8;
            parts.second = number(&text[17..19])? as u8;
            if bytes.len() > 19 {
                ensure!(
                    bytes[19] == b'.' && (21..=27).contains(&bytes.len()),
                    "invalid ISO DATETIME2 fraction"
                );
                parts.fraction = number(&text[20..])? * 10u32.pow((27 - bytes.len()) as u32);
            }
        }
        Self::from_parts(parts)
    }
    pub fn format_iso(self, scale: u8) -> Result<String> {
        let p = self.round(scale)?.parts();
        let mut result = format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
            p.year, p.month, p.day, p.hour, p.minute, p.second
        );
        if scale > 0 {
            result.push_str(&format!(
                ".{:0width$}",
                i64::from(p.fraction) / quantum(scale)?,
                width = usize::from(scale)
            ));
        }
        Ok(result)
    }
    /// Bare DATETIME2N bytes, excluding the value-length prefix and metadata.
    pub fn encode(self, scale: u8) -> Result<Vec<u8>> {
        let width = time_width(scale)?;
        let value = self.round(scale)?;
        let time = (value.0 % DAY) / quantum(scale)?;
        let days = value.0 / DAY;
        let mut result = Vec::with_capacity(width + 3);
        result.extend_from_slice(&time.to_le_bytes()[..width]);
        result.extend_from_slice(&days.to_le_bytes()[..3]);
        Ok(result)
    }
    pub fn decode(bytes: &[u8], scale: u8) -> Result<Self> {
        let width = time_width(scale)?;
        ensure!(bytes.len() == width + 3, "invalid DATETIME2 value width");
        let mut time = [0u8; 8];
        time[..width].copy_from_slice(&bytes[..width]);
        let time = u64::from_le_bytes(time);
        let mut days = [0u8; 4];
        days[..3].copy_from_slice(&bytes[width..]);
        let days = i64::from(u32::from_le_bytes(days));
        let q = quantum(scale)?;
        if days > LAST_DAY || time >= (DAY / q) as u64 {
            bail!("invalid DATETIME2 wire value");
        }
        Self::from_ticks(days * DAY + time as i64 * q)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn time_only_literals_supply_the_base_date() {
        for second in 0..86400 {
            let text = format!(
                "{:02}:{:02}:{:02}.1234567",
                second / 3600,
                second / 60 % 60,
                second % 60
            );
            let value = DateTime2::parse_iso(&text).unwrap();
            let parts = value.parts();
            assert_eq!((parts.year, parts.month, parts.day), (1900, 1, 1));
            assert_eq!(value.format_iso(7).unwrap(), format!("1900-01-01T{text}"));
        }
        assert_eq!(
            DateTime2::parse_iso("12:34")
                .unwrap()
                .format_iso(0)
                .unwrap(),
            "1900-01-01T12:34:00"
        );
        assert_eq!(
            DateTime2::parse_iso("23:59:59.9999999")
                .unwrap()
                .format_iso(3)
                .unwrap(),
            "1900-01-02T00:00:00.000"
        );
        for bad in [
            "24:00:00",
            "12:60:00",
            "12:00:60",
            "12:00:00.",
            "12:00:00.12345678",
            "12:xx:00",
            "12:34junk",
            "１２:34:56",
        ] {
            assert!(DateTime2::parse_iso(bad).is_err(), "{bad}");
        }
    }
    #[test]
    fn complete_calendar_and_wire_roundtrips() {
        for day in 0..=LAST_DAY {
            let value = DateTime2::from_ticks(day * DAY + 1234567).unwrap();
            assert_eq!(DateTime2::from_parts(value.parts()).unwrap(), value);
        }
        for text in [
            "0001-01-01T00:00:00.0000000",
            "1600-02-29T12:34:56.1234567",
            "1900-03-01T00:00:00.0000001",
            "2000-02-29T23:59:59.0000000",
            "9999-12-31T23:59:59.0000000",
        ] {
            let value = DateTime2::parse_iso(text).unwrap();
            for scale in 0..=7 {
                let encoded = value.encode(scale).unwrap();
                assert_eq!(encoded.len(), [6, 6, 6, 7, 7, 8, 8, 8][scale as usize]);
                assert_eq!(
                    DateTime2::decode(&encoded, scale).unwrap(),
                    value.round(scale).unwrap()
                );
                assert_eq!(
                    DateTime2::parse_iso(&value.format_iso(scale).unwrap()).unwrap(),
                    value.round(scale).unwrap()
                );
            }
        }
    }
    #[test]
    fn independent_wire_vectors_and_rounding_carry() {
        assert_eq!(
            DateTime2::parse_iso("0001-01-01T00:00:00.0000001")
                .unwrap()
                .encode(7)
                .unwrap(),
            [1, 0, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(
            DateTime2::parse_iso("0001-01-02T00:00:01")
                .unwrap()
                .encode(0)
                .unwrap(),
            [1, 0, 0, 1, 0, 0]
        );
        let last = DateTime2::parse_iso("9999-12-31T23:59:59.9999999").unwrap();
        assert_eq!(
            last.encode(7).unwrap(),
            [255, 191, 105, 42, 201, 218, 185, 55]
        );
        assert!(last.round(6).is_err());
        assert_eq!(
            DateTime2::parse_iso("1999-12-31T23:59:59.9999995")
                .unwrap()
                .format_iso(6)
                .unwrap(),
            "2000-01-01T00:00:00.000000"
        );
        assert_eq!(
            DateTime2::parse_iso("2000-02-28T23:59:59.5000000")
                .unwrap()
                .format_iso(0)
                .unwrap(),
            "2000-02-29T00:00:00"
        );
        assert_eq!(
            DateTime2::from_unix_nanos(-150)
                .unwrap()
                .format_iso(7)
                .unwrap(),
            "1969-12-31T23:59:59.9999999"
        );
        assert_eq!(
            DateTime2::from_unix_nanos(-151)
                .unwrap()
                .format_iso(7)
                .unwrap(),
            "1969-12-31T23:59:59.9999998"
        );
    }
    #[test]
    fn invalid_calendar_scales_and_wire_values_are_rejected() {
        for text in [
            "0000-01-01",
            "1900-02-29",
            "2000-02-30",
            "9999-13-01",
            "2026-01-01T24:00:00",
            "2026-01-01T12:60:00",
            "2026-01-01T12:00:60",
            "2026-01-01T00:00:00.",
            "2026-01-01T00:00:00.12345678",
            "2026-01-01T00:00:00Z",
            "é000-01-01",
        ] {
            assert!(DateTime2::parse_iso(text).is_err(), "{text}");
        }
        assert!(DateTime2::decode(&[0; 8], 8).is_err());
        for scale in 0..=7 {
            assert!(DateTime2::decode(&[0; 5], scale).is_err());
            assert!(DateTime2::decode(&vec![255; time_width(scale).unwrap() + 3], scale).is_err());
        }
        assert!(DateTime2::from_unix_nanos(i128::MAX).is_err());
        assert!(DateTime2::from_ticks(-1).is_err());
    }
}
