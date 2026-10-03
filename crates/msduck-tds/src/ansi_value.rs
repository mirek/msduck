//! Result-value framing for already SQL-converted native ANSI bytes.
//! No TYPE_INFO, character conversion, padding or collation support is inferred.
use anyhow::{Result, bail, ensure};

/// A validated BIGCHAR (0xAF) or BIGVARCHAR (0xA7) declaration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Declaration {
    fixed: bool,
    capacity: Option<u16>,
}

impl Declaration {
    /// `None` is VARCHAR(MAX); bounded capacities count bytes, not characters.
    pub fn new(type_id: u8, capacity: Option<u16>) -> Result<Self> {
        let fixed = match type_id {
            0xaf => true,
            0xa7 => false,
            _ => bail!("unsupported native ANSI result family"),
        };
        ensure!(!fixed || capacity.is_some(), "CHAR(MAX) is invalid");
        if let Some(bytes) = capacity {
            ensure!((1..=8000).contains(&bytes), "invalid ANSI byte capacity");
        }
        Ok(Self { fixed, capacity })
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Value<'a> {
    Null,
    Bytes(&'a [u8]),
    /// Explicit nonempty PLP chunks. An empty list encodes a non-NULL empty value.
    /// Chunk boundaries may fall inside a multibyte character.
    Chunks(&'a [&'a [u8]]),
}

/// Append one value, bounded by both `output_limit` (including existing output)
/// and the server message limit. Every error leaves output bytes unchanged.
/// PLP emits the known total length, original chunks and one zero terminator.
pub fn encode(
    out: &mut Vec<u8>,
    declaration: Declaration,
    value: Value<'_>,
    output_limit: usize,
) -> Result<()> {
    let plp = declaration.capacity.is_none();
    let (bytes, chunk_count) = match value {
        Value::Null => (0, 0),
        Value::Bytes(bytes) => (bytes.len(), usize::from(!bytes.is_empty())),
        Value::Chunks(chunks) => {
            ensure!(plp, "explicit chunks require VARCHAR(MAX)");
            let mut length = 0usize;
            for chunk in chunks {
                ensure!(!chunk.is_empty(), "empty PLP data chunk is a terminator");
                ensure!(u32::try_from(chunk.len()).is_ok(), "PLP chunk too large");
                length = length
                    .checked_add(chunk.len())
                    .ok_or_else(|| anyhow::anyhow!("ANSI payload length overflow"))?;
            }
            (length, chunks.len())
        }
    };
    if !matches!(value, Value::Null) {
        if let Some(capacity) = declaration.capacity {
            ensure!(
                bytes <= usize::from(capacity),
                "ANSI byte capacity exceeded"
            );
            ensure!(
                !declaration.fixed || bytes == usize::from(capacity),
                "CHAR requires caller-supplied exact-width bytes"
            );
        } else {
            ensure!(u64::try_from(bytes).is_ok(), "PLP total length too large");
            if matches!(value, Value::Bytes(_)) {
                ensure!(u32::try_from(bytes).is_ok(), "PLP chunk too large");
            }
        }
    }
    let framing = if !plp {
        2
    } else if matches!(value, Value::Null) {
        8
    } else {
        chunk_count
            .checked_mul(4)
            .and_then(|n| n.checked_add(12))
            .ok_or_else(|| anyhow::anyhow!("PLP framing length overflow"))?
    };
    let growth = bytes
        .checked_add(framing)
        .ok_or_else(|| anyhow::anyhow!("ANSI frame length overflow"))?;
    let final_length = out
        .len()
        .checked_add(growth)
        .ok_or_else(|| anyhow::anyhow!("ANSI output length overflow"))?;
    ensure!(
        final_length <= output_limit.min(crate::MAX_MESSAGE),
        "ANSI output exceeds resource limit"
    );
    // Reserve the entire growth before the first byte is appended. No fallible
    // checks or allocations remain after this point.
    out.try_reserve(growth)?;
    if matches!(value, Value::Null) {
        if plp {
            out.extend(u64::MAX.to_le_bytes());
        } else {
            out.extend(u16::MAX.to_le_bytes());
        }
        return Ok(());
    }
    if plp {
        out.extend((bytes as u64).to_le_bytes());
        match value {
            Value::Bytes(bytes) if !bytes.is_empty() => append_chunk(out, bytes),
            Value::Chunks(chunks) => {
                for chunk in chunks {
                    append_chunk(out, chunk);
                }
            }
            _ => {}
        }
        out.extend(0u32.to_le_bytes());
    } else {
        out.extend((bytes as u16).to_le_bytes());
        if let Value::Bytes(bytes) = value {
            out.extend_from_slice(bytes);
        }
    }
    Ok(())
}

fn append_chunk(out: &mut Vec<u8>, chunk: &[u8]) {
    out.extend((chunk.len() as u32).to_le_bytes());
    out.extend_from_slice(chunk);
}
