//! Exact DATETIMEOFFSET values: UTC ticks plus a retained fixed offset.
use crate::datetime2::DateTime2;
use anyhow::{Result, ensure};
const MINUTE: i64 = 600_000_000;

/// UTC and local civil time must both fit SQL Server's years 0001 through 9999.
/// Offset preservation is separate from instant comparison (`utc()`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DateTimeOffset {
    utc: DateTime2,
    offset: i16,
}
impl DateTimeOffset {
    pub fn from_utc(utc: DateTime2, offset: i16) -> Result<Self> {
        ensure!(
            (-840..=840).contains(&offset),
            "DATETIMEOFFSET offset outside -14:00 through +14:00"
        );
        DateTime2::from_ticks(utc.ticks() + i64::from(offset) * MINUTE)
            .map_err(|_| anyhow::anyhow!("DATETIMEOFFSET local date outside 0001 through 9999"))?;
        Ok(Self { utc, offset })
    }
    pub fn from_local(local: DateTime2, offset: i16) -> Result<Self> {
        ensure!(
            (-840..=840).contains(&offset),
            "DATETIMEOFFSET offset outside -14:00 through +14:00"
        );
        let utc = DateTime2::from_ticks(local.ticks() - i64::from(offset) * MINUTE)
            .map_err(|_| anyhow::anyhow!("DATETIMEOFFSET UTC date outside 0001 through 9999"))?;
        Self::from_utc(utc, offset)
    }
    pub fn utc(self) -> DateTime2 {
        self.utc
    }
    pub fn local(self) -> DateTime2 {
        // Constructors establish both ranges; fields cannot be mutated externally.
        DateTime2::from_ticks(self.utc.ticks() + i64::from(self.offset) * MINUTE)
            .expect("validated DATETIMEOFFSET local date")
    }
    pub fn offset_minutes(self) -> i16 {
        self.offset
    }
    pub fn round(self, scale: u8) -> Result<Self> {
        Self::from_utc(self.utc.round(scale)?, self.offset)
    }
    /// Parse the ISO forms with Z or a signed hh:mm offset; absent offset is UTC.
    pub fn parse_iso(text: &str) -> Result<Self> {
        let (local, offset) = Self::parse_parts(text, false)?;
        Self::from_local(local, offset)
    }
    /// Parse local fields and validate the zone without imposing a UTC range.
    pub fn parse_local(text: &str) -> Result<(DateTime2, i16)> {
        Self::parse_parts(text, true)
    }
    fn parse_parts(text: &str, require_time_for_zone: bool) -> Result<(DateTime2, i16)> {
        let text = text.trim();
        ensure!(text.is_ascii(), "invalid ISO DATETIMEOFFSET");
        let (civil, offset) =
            if let Some(civil) = text.strip_suffix('Z').or_else(|| text.strip_suffix('z')) {
                (civil, 0)
            } else if text.len() >= 6
                && matches!(text.as_bytes()[text.len() - 6], b'+' | b'-')
                && text.as_bytes()[text.len() - 3] == b':'
            {
                let split = text.len() - 6;
                let zone = &text.as_bytes()[split..];
                ensure!(
                    zone[3] == b':'
                        && [zone[1], zone[2], zone[4], zone[5]]
                            .iter()
                            .all(u8::is_ascii_digit),
                    "invalid DATETIMEOFFSET zone"
                );
                let hours = i16::from(zone[1] - b'0') * 10 + i16::from(zone[2] - b'0');
                let minutes = i16::from(zone[4] - b'0') * 10 + i16::from(zone[5] - b'0');
                ensure!(
                    minutes < 60 && hours <= 14 && (hours < 14 || minutes == 0),
                    "invalid DATETIMEOFFSET zone"
                );
                (
                    &text[..split],
                    (hours * 60 + minutes) * if zone[0] == b'-' { -1 } else { 1 },
                )
            } else {
                (text, 0)
            };
        ensure!(
            !require_time_for_zone || civil.len() == text.len() || civil.contains(':'),
            "offset requires a time component"
        );
        Ok((DateTime2::parse_iso(civil.trim_end())?, offset))
    }
    pub fn format_iso(self, scale: u8) -> Result<String> {
        let value = self.round(scale)?;
        let magnitude = value.offset.unsigned_abs();
        Ok(format!(
            "{} {}{:02}:{:02}",
            value.local().format_iso(scale)?,
            if value.offset < 0 { '-' } else { '+' },
            magnitude / 60,
            magnitude % 60
        ))
    }
    /// Bare TDS 7.3 DATETIMEOFFSETN bytes (without metadata or length prefix).
    pub fn encode(self, scale: u8) -> Result<Vec<u8>> {
        let value = self.round(scale)?;
        let mut bytes = value.utc.encode(scale)?;
        bytes.extend_from_slice(&value.offset.to_le_bytes());
        Ok(bytes)
    }
    pub fn decode(bytes: &[u8], scale: u8) -> Result<Self> {
        ensure!(scale <= 7, "invalid DATETIMEOFFSET scale");
        let width = match scale {
            0..=2 => 8,
            3..=4 => 9,
            _ => 10,
        };
        ensure!(bytes.len() == width, "invalid DATETIMEOFFSET value width");
        let offset = i16::from_le_bytes(bytes[width - 2..].try_into()?);
        Self::from_utc(DateTime2::decode(&bytes[..width - 2], scale)?, offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn upstream_wire_vectors_preserve_utc_and_offset() {
        let value = DateTimeOffset::parse_iso("2026-07-01T02:30:00.1234567 +05:30").unwrap();
        assert_eq!(
            value.utc().format_iso(7).unwrap(),
            "2026-06-30T21:00:00.1234567"
        );
        let bytes = vec![0x87, 0x5e, 0x2f, 0x05, 0xb0, 0xd4, 0x49, 0x0b, 0x4a, 0x01];
        assert_eq!(value.encode(7).unwrap(), bytes);
        assert_eq!(DateTimeOffset::decode(&bytes, 7).unwrap(), value);
        let rounded = DateTimeOffset::parse_iso("2026-12-31T23:59:59.9999999Z").unwrap();
        assert_eq!(
            rounded.encode(3).unwrap(),
            [0, 0, 0, 0, 0x8d, 0x4a, 0x0b, 0, 0]
        );
        assert_eq!(
            rounded.format_iso(3).unwrap(),
            "2027-01-01T00:00:00.000 +00:00"
        );
    }
    #[test]
    fn datetimeoffset_absent_offsets_use_utc() {
        for text in [
            "2024-01-01",
            "2024-01-01Z",
            "2024-01-01T00:00:00",
            "2024-01-01T00:00:00+00:00",
        ] {
            let value = DateTimeOffset::parse_iso(text).unwrap();
            assert_eq!(value.offset_minutes(), 0);
            assert_eq!(value.format_iso(0).unwrap(), "2024-01-01T00:00:00 +00:00");
        }
        assert_eq!(
            DateTimeOffset::parse_iso("12:00:00+05:30")
                .unwrap()
                .format_iso(0)
                .unwrap(),
            "1900-01-01T12:00:00 +05:30"
        );
    }
    #[test]
    fn all_offsets_and_scales_roundtrip() {
        for offset in -840..=840 {
            for civil in [
                "0001-01-02T00:00:00.0000000",
                "2024-02-29T23:59:59.1234567",
                "9999-12-30T23:59:59.9999999",
            ] {
                let value =
                    DateTimeOffset::from_local(DateTime2::parse_iso(civil).unwrap(), offset)
                        .unwrap();
                for scale in 0..=7 {
                    let rounded = value.round(scale).unwrap();
                    let encoded = value.encode(scale).unwrap();
                    assert_eq!(DateTimeOffset::decode(&encoded, scale).unwrap(), rounded);
                    assert_eq!(
                        DateTimeOffset::parse_iso(&value.format_iso(scale).unwrap()).unwrap(),
                        rounded
                    );
                    assert_eq!(rounded.offset_minutes(), offset);
                }
            }
        }
    }
    #[test]
    fn ranges_rounding_and_malformed_payloads() {
        for text in [
            "2026-02-29T00:00:00Z",
            "2026-01-01T00:00:00+12:60",
            "2026-01-01T00:00:00+14:01",
            "0001-01-01T00:00:00+14:00",
            "9999-12-31T23:59:00-14:00",
            "2026-01-01T00:00:00+1:00",
            "2026-01-01T00:00:00+00:0x",
            "🦆",
        ] {
            assert!(DateTimeOffset::parse_iso(text).is_err(), "{text}");
        }
        for text in [
            "9999-12-31T23:59:59.9999999+14:00",
            "9999-12-31T09:59:59.9999999-14:00",
        ] {
            let value = DateTimeOffset::parse_iso(text).unwrap();
            assert!(value.encode(3).is_err());
            assert!(value.format_iso(3).is_err());
        }
        for text in [
            "0001-01-01T14:00:00+14:00",
            "9999-12-31T09:59:59.9999999-14:00",
        ] {
            let value = DateTimeOffset::parse_iso(text).unwrap();
            assert_eq!(
                DateTimeOffset::decode(&value.encode(7).unwrap(), 7).unwrap(),
                value
            );
        }
        for scale in 0..=7 {
            let value = DateTimeOffset::parse_iso("2024-01-01T12:00:00-05:30").unwrap();
            let bytes = value.encode(scale).unwrap();
            for len in 0..bytes.len() {
                assert!(DateTimeOffset::decode(&bytes[..len], scale).is_err());
            }
            let mut extra = bytes.clone();
            extra.push(0);
            assert!(DateTimeOffset::decode(&extra, scale).is_err());
            for offset in [i16::MIN, -841, 841, i16::MAX] {
                let mut bad = bytes.clone();
                let n = bad.len();
                bad[n - 2..].copy_from_slice(&offset.to_le_bytes());
                assert!(DateTimeOffset::decode(&bad, scale).is_err());
            }
            let mut bad = bytes.clone();
            bad[..bytes.len() - 2].fill(255);
            assert!(DateTimeOffset::decode(&bad, scale).is_err());
        }
        assert!(DateTimeOffset::decode(&[0; 10], 8).is_err());
        let first = DateTime2::from_ticks(0).unwrap();
        assert!(DateTimeOffset::from_utc(first, -1).is_err());
        let last = DateTime2::parse_iso("9999-12-31T23:59:59.9999999").unwrap();
        assert!(DateTimeOffset::from_utc(last, 1).is_err());
    }
}
