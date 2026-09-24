//! Imperative TDS streaming I/O and DuckDB adapter integration tests.
//! Pure payload codecs are re-exported to preserve the existing public API.
use anyhow::{Result, ensure};
pub use msduck_tds::*;
use std::io::{Read, Write};

pub fn read_message(reader: &mut impl Read, packet_size: usize) -> Result<Option<Message>> {
    let framing_error = |error| anyhow::anyhow!("invalid TDS framing: {error:?}");
    let mut decoder = framing::Decoder::new(packet_size, MAX_MESSAGE).map_err(framing_error)?;
    let mut buffer = [0u8; 8192];
    loop {
        // The borrowed Read has no retained lookahead buffer. Never take bytes
        // from the next message: its next read_message call must still see them.
        let limit = decoder
            .read_size()
            .map_err(framing_error)?
            .min(buffer.len());
        let count = match reader.read(&mut buffer[..limit]) {
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        };
        if count == 0 {
            decoder.finish().map_err(framing_error)?;
            return Ok(None);
        }
        let progress = decoder.feed(&buffer[..count]).map_err(framing_error)?;
        ensure!(
            progress.consumed == count,
            "TDS read exceeded decoder boundary"
        );
        if let Some(message) = progress.message {
            return Ok(Some(message));
        }
    }
}

pub fn write_message(writer: &mut impl Write, data: &[u8], packet_size: usize) -> Result<()> {
    write_message_kind(writer, data, packet_size, 4)
}

pub(crate) fn write_message_kind(
    writer: &mut impl Write,
    data: &[u8],
    packet_size: usize,
    kind: u8,
) -> Result<()> {
    ensure!(
        (512..=32767).contains(&packet_size),
        "invalid negotiated packet size"
    );
    let capacity = packet_size - 8;
    let count = data.len().div_ceil(capacity).max(1);
    for i in 0..count {
        let start = i * capacity;
        let end = (start + capacity).min(data.len());
        let chunk = &data[start..end];
        let length = ((chunk.len() + 8) as u16).to_be_bytes();
        writer.write_all(&[
            kind,
            u8::from(i + 1 == count),
            length[0],
            length[1],
            0,
            0,
            (i + 1) as u8,
            0,
        ])?;
        writer.write_all(chunk)?;
    }
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod transport_tests {
    use super::*;
    #[test]
    fn read_message_keeps_following_message_and_retries_interrupted_short_reads() {
        struct Choppy {
            bytes: std::io::Cursor<Vec<u8>>,
            interrupt: bool,
            width: usize,
        }
        impl Read for Choppy {
            fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                self.interrupt = !self.interrupt;
                if self.interrupt {
                    return Err(std::io::ErrorKind::Interrupted.into());
                }
                let size = self.width.min(out.len());
                self.bytes.read(&mut out[..size])
            }
        }
        for width in [1, 7, 512, 8192] {
            let data = vec![0xab; 1400];
            let mut wire = vec![];
            write_message(&mut wire, &data, 512).unwrap();
            wire[1] |= 2; // IGNORE on a non-final packet must survive assembly.
            let boundary = wire.len() as u64;
            write_message_kind(&mut wire, &[], 512, 6).unwrap();
            let mut reader = Choppy {
                bytes: std::io::Cursor::new(wire),
                interrupt: false,
                width,
            };
            let first = read_message(&mut reader, 512).unwrap().unwrap();
            assert_eq!(first.payload, data);
            assert_eq!(first.status, 3);
            assert_eq!(reader.bytes.position(), boundary);
            let second = read_message(&mut reader, 512).unwrap().unwrap();
            assert_eq!((second.kind, second.status), (6, 1));
            assert!(second.payload.is_empty());
            assert!(read_message(&mut reader, 512).unwrap().is_none());
        }
    }

    #[test]
    fn read_message_distinguishes_clean_eof_from_every_truncated_prefix() {
        let mut wire = vec![];
        write_message(&mut wire, &vec![0x77; 600], 512).unwrap();
        assert!(read_message(&mut &[][..], 512).unwrap().is_none());
        for length in 1..wire.len() {
            assert!(
                read_message(&mut &wire[..length], 512).is_err(),
                "prefix {length}"
            );
        }
        assert!(read_message(&mut &wire[..], 512).unwrap().is_some());
    }

    #[test]
    fn fragmented_round_trip_and_id_wrap() {
        for size in [0, 1, 504, 505, 150000] {
            let data = vec![0xab; size];
            let mut wire = vec![];
            write_message(&mut wire, &data, 512).unwrap();
            let result = read_message(&mut &wire[..], 512).unwrap().unwrap();
            assert_eq!(result.payload, data);
        }
    }
    #[test]
    fn malformed_inputs_do_not_panic() {
        for len in 0..256 {
            let bytes = vec![0; len];
            assert!(
                std::panic::catch_unwind(|| {
                    let _ = prelogin(&bytes);
                    let _ = login(&bytes);
                    let _ = batch_body(&bytes);
                    let _ = read_message(&mut &bytes[..], 4096);
                })
                .is_ok()
            );
        }
    }
    #[test]
    fn reject_packet_type_change() {
        let mut wire = vec![];
        write_message(&mut wire, &vec![0; 600], 512).unwrap();
        wire[512] = 3;
        assert!(read_message(&mut &wire[..], 512).is_err());
    }
    #[test]
    fn malformed_decoder_fuzz_sweep() {
        let mut state = 0x12345678u32;
        for len in 0..512 {
            let bytes: Vec<u8> = (0..len)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 17;
                    state ^= state << 5;
                    state as u8
                })
                .collect();
            let _ = prelogin(&bytes);
            let _ = login(&bytes);
            let _ = batch_body(&bytes);
            let _ = read_message(&mut &bytes[..], 4096);
        }
    }
}

#[cfg(test)]
mod varchar_tests {
    use super::*;
    #[test]
    fn bounded_varchar_metadata_values_nulls_and_cp1252_round_trip() {
        let mut metadata_bytes = Vec::new();
        metadata(
            &mut metadata_bytes,
            &[Column {
                collation: None,
                properties: Default::default(),
                name: String::new(),
                kind: Type::Varchar(3),
            }],
        )
        .unwrap();
        assert_eq!(&metadata_bytes[9..12], &[0xa7, 3, 0]);
        let mut row = Vec::new();
        crate::engine::encode_value(
            &mut row,
            &Type::Varchar(3),
            &duckdb::types::Value::Text("€A".into()),
        )
        .unwrap();
        crate::engine::encode_value(
            &mut row,
            &Type::Varchar(3),
            &duckdb::types::Value::Text(String::new()),
        )
        .unwrap();
        crate::engine::encode_value(&mut row, &Type::Varchar(3), &duckdb::types::Value::Null)
            .unwrap();
        assert_eq!(row, [2, 0, 0x80, b'A', 0, 0, 0xff, 0xff]);
        assert!(
            crate::engine::encode_value(
                &mut Vec::new(),
                &Type::Varchar(1),
                &duckdb::types::Value::Text("ab".into())
            )
            .is_err()
        );
        assert!(encode_cp1252("🦆").is_err());
        let bytes = (0u8..=255).collect::<Vec<_>>();
        assert_eq!(encode_cp1252(&decode_cp1252(&bytes)).unwrap(), bytes);
    }
}

#[cfg(test)]
mod nvarchar_tests {
    use super::*;
    #[test]
    fn fixed_unicode_metadata_and_values() {
        let mut bytes = vec![];
        metadata(
            &mut bytes,
            &[Column {
                collation: None,
                properties: Default::default(),
                name: String::new(),
                kind: Type::Nchar(1),
            }],
        )
        .unwrap();
        assert_eq!(&bytes[9..12], &[0xef, 2, 0]);
        let mut row = vec![];
        crate::engine::encode_value(
            &mut row,
            &Type::Nchar(1),
            &duckdb::types::Value::Text("€".into()),
        )
        .unwrap();
        crate::engine::encode_value(&mut row, &Type::Nchar(1), &duckdb::types::Value::Null)
            .unwrap();
        assert_eq!(row, [2, 0, 0xac, 0x20, 0xff, 0xff]);
        for invalid in ["", "ab", "🦆"] {
            assert!(
                crate::engine::encode_value(
                    &mut vec![],
                    &Type::Nchar(1),
                    &duckdb::types::Value::Text(invalid.into())
                )
                .is_err()
            );
        }
    }
    #[test]
    fn bounded_utf16_metadata_values_and_surrogate_width() {
        let mut bytes = vec![];
        metadata(
            &mut bytes,
            &[Column {
                collation: None,
                properties: Default::default(),
                name: String::new(),
                kind: Type::Nvarchar(3),
            }],
        )
        .unwrap();
        assert_eq!(&bytes[9..12], &[0xe7, 6, 0]);
        let mut row = vec![];
        crate::engine::encode_value(
            &mut row,
            &Type::Nvarchar(3),
            &duckdb::types::Value::Text("A🦆".into()),
        )
        .unwrap();
        crate::engine::encode_value(
            &mut row,
            &Type::Nvarchar(3),
            &duckdb::types::Value::Text(String::new()),
        )
        .unwrap();
        crate::engine::encode_value(&mut row, &Type::Nvarchar(3), &duckdb::types::Value::Null)
            .unwrap();
        assert_eq!(
            row,
            [6, 0, 0x41, 0, 0x3e, 0xd8, 0x86, 0xdd, 0, 0, 0xff, 0xff]
        );
        assert!(
            crate::engine::encode_value(
                &mut vec![],
                &Type::Nvarchar(1),
                &duckdb::types::Value::Text("🦆".into())
            )
            .is_err()
        );
        assert!(
            metadata(
                &mut vec![],
                &[Column {
                    collation: None,
                    properties: Default::default(),
                    name: String::new(),
                    kind: Type::Nvarchar(4001)
                }]
            )
            .is_err()
        );
        let mut maximum = vec![];
        crate::engine::encode_value(
            &mut maximum,
            &Type::Nvarchar(4000),
            &duckdb::types::Value::Text("a".repeat(4000)),
        )
        .unwrap();
        assert_eq!(&maximum[..2], &8000u16.to_le_bytes());
        assert_eq!(maximum.len(), 8002);
    }
}
