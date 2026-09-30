//! Deterministic ORDER tokens. Callers supply resolved output ordinals and
//! decide whether to emit a token; query syntax alone does not establish that.
use anyhow::{Result, ensure};

pub const TOKEN: u8 = 0xa9;
/// The length word counts bytes, so the largest even payload is 65,534 bytes.
pub const MAX_ORDINALS: usize = u16::MAX as usize / 2;

/// Append one token, leaving `out` unchanged on error. Zero is SQL Server's
/// captured nonprojected-key ordinal. Direction is absent from this token;
/// neither zero nor duplicate ordinals may be discarded by the codec.
pub fn encode(out: &mut Vec<u8>, ordinals: &[u16]) -> Result<()> {
    let bytes = ordinals
        .len()
        .checked_mul(2)
        .and_then(|bytes| u16::try_from(bytes).ok())
        .ok_or_else(|| anyhow::anyhow!("ORDER payload exceeds USHORT byte length"))?;
    // Reserve the complete token before changing the caller-owned output.
    out.try_reserve(3 + usize::from(bytes))?;
    out.push(TOKEN);
    out.extend(bytes.to_le_bytes());
    for ordinal in ordinals {
        out.extend(ordinal.to_le_bytes());
    }
    Ok(())
}

/// Decode exactly one complete token. The transport caller supplies its token
/// boundary; trailing bytes are rejected rather than silently consumed.
pub fn decode(bytes: &[u8]) -> Result<Vec<u16>> {
    let mut cursor = crate::Cursor::new(bytes);
    ensure!(cursor.u8()? == TOKEN, "not an ORDER token");
    let length = usize::from(cursor.u16()?);
    ensure!(length.is_multiple_of(2), "odd ORDER payload length");
    let payload = cursor.take(length)?;
    ensure!(cursor.remaining() == 0, "trailing ORDER bytes");
    Ok(payload
        .chunks_exact(2)
        .map(|word| u16::from_le_bytes([word[0], word[1]]))
        .collect())
}
