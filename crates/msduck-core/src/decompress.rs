//! Bounded, deterministic decoding of SQL Server DECOMPRESS input bytes.
//!
//! This module intentionally has no SQL binding, TDS, or database effects. Its
//! observed behavior comes from `reference/compress-decompress.json`.

use flate2::{Decompress, FlushDecompress, Status};

/// Error 9826 is the captured SQL Server response to a corrupt GZIP member.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecodeError {
    Corrupt,
    OutputLimitExceeded,
    /// No retained observation defines the SQL Server response for this case.
    Unsupported(&'static str),
}

/// Decode the first GZIP member. A `None` input is SQL NULL. Incomplete members
/// and valid members with empty deflate output also produce SQL NULL; a zero-byte
/// input alone produces an empty binary value. `max_output` is a caller-supplied
/// memory bound, checked before appending decompressed data.
pub fn decompress(input: Option<&[u8]>, max_output: usize) -> Result<Option<Vec<u8>>, DecodeError> {
    let Some(input) = input else { return Ok(None) };
    if input.is_empty() {
        return Ok(Some(Vec::new()));
    }
    const PREFIX: [u8; 3] = [0x1f, 0x8b, 8];
    for (offset, expected) in PREFIX.iter().enumerate().take(input.len().min(3)) {
        if input[offset] != *expected {
            return Err(DecodeError::Corrupt);
        }
    }
    if input.len() < 10 {
        return Ok(None);
    }
    let flags = input[3];
    let mut offset = 10;
    if flags & 0x04 != 0 {
        let Some(length_bytes) = input.get(offset..offset + 2) else {
            return Ok(None);
        };
        let length = u16::from_le_bytes([length_bytes[0], length_bytes[1]]) as usize;
        offset += 2;
        if input.get(offset..offset + length).is_none() {
            return Ok(None);
        }
        offset += length;
    }
    for flag in [0x08, 0x10] {
        if flags & flag != 0 {
            let Some(ending) = input[offset..].iter().position(|&byte| byte == 0) else {
                return Ok(None);
            };
            offset += ending + 1;
        }
    }
    if flags & 0x02 != 0 {
        if input.get(offset..offset + 2).is_none() {
            return Ok(None);
        }
        offset += 2; // SQL Server does not validate FHCRC.
    }

    let mut inflater = Decompress::new(false);
    let mut output = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let before_in = inflater.total_in();
        let before_out = inflater.total_out();
        let status = inflater
            .decompress(&input[offset..], &mut chunk, FlushDecompress::None)
            .map_err(|_| DecodeError::Unsupported("invalid DEFLATE payload was not captured"))?;
        let consumed = (inflater.total_in() - before_in) as usize;
        let produced = (inflater.total_out() - before_out) as usize;
        offset += consumed;
        if produced > max_output.saturating_sub(output.len()) {
            return Err(DecodeError::OutputLimitExceeded);
        }
        output.extend_from_slice(&chunk[..produced]);
        if status == Status::StreamEnd {
            break;
        }
        if consumed == 0 && produced == 0 || offset == input.len() && produced < chunk.len() {
            return Ok(None);
        }
    }

    if let Some(trailer) = input.get(offset..offset + 8) {
        let crc = u32::from_le_bytes(trailer[..4].try_into().expect("four bytes"));
        let size = u32::from_le_bytes(trailer[4..].try_into().expect("four bytes"));
        if crc != crc32fast::hash(&output) || size != output.len() as u32 {
            return Err(DecodeError::Corrupt);
        }
    }
    if output.is_empty() {
        Ok(None)
    } else {
        Ok(Some(output))
    }
}
