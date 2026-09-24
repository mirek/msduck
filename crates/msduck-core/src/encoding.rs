//! Deterministic character encodings shared by SQL rules and wire codecs.
use anyhow::Result;

/// SQL_Latin1_General_CP1's single-byte code page. Undefined extension bytes
/// retain their C1 code points, matching the reference mssqlite codec.
const CP1252_HIGH: [u32; 32] = [
    0x20ac, 0x81, 0x201a, 0x192, 0x201e, 0x2026, 0x2020, 0x2021, 0x2c6, 0x2030, 0x160, 0x2039,
    0x152, 0x8d, 0x17d, 0x8f, 0x90, 0x2018, 0x2019, 0x201c, 0x201d, 0x2022, 0x2013, 0x2014, 0x2dc,
    0x2122, 0x161, 0x203a, 0x153, 0x9d, 0x17e, 0x178,
];

pub fn decode_cp1252(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| {
            let code = if (0x80..=0x9f).contains(byte) {
                CP1252_HIGH[(*byte - 0x80) as usize]
            } else {
                *byte as u32
            };
            char::from_u32(code).expect("Windows-1252 table contains valid Unicode scalars")
        })
        .collect()
}

/// Encode representable values exactly; best-fit/lossy conversion belongs in the
/// SQL conversion layer, not the TDS value writer.
pub fn encode_cp1252(value: &str) -> Result<Vec<u8>> {
    value
        .chars()
        .map(|c| {
            let code = u32::from(c);
            if code < 0x80 || (0xa0..=0xff).contains(&code) {
                Ok(code as u8)
            } else if let Some(index) = CP1252_HIGH.iter().position(|candidate| *candidate == code)
            {
                Ok(0x80 + index as u8)
            } else {
                anyhow::bail!("VARCHAR value is not representable in Windows-1252")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cp1252_vectors_and_complete_byte_roundtrip() {
        assert_eq!(encode_cp1252("A€éŒ").unwrap(), [0x41, 0x80, 0xe9, 0x8c]);
        assert_eq!(decode_cp1252(&[0x41, 0x80, 0xe9, 0x8c]), "A€éŒ");
        let bytes = (0..=255).collect::<Vec<u8>>();
        assert_eq!(encode_cp1252(&decode_cp1252(&bytes)).unwrap(), bytes);
        assert!(encode_cp1252("🦆").is_err());
    }
}
