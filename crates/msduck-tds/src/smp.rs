//! Bounded MC-SMP framing. Session ordering, credit and transport are separate.
//!
//! Header layout and control rules follow MC-SMP sections 2.2.1–2.2.5.
//! Tests adapt attributed fixtures from mirek/mssqlite; see THIRD_PARTY_NOTICES.md.
use anyhow::{Result, bail, ensure};

pub const HEADER_LENGTH: usize = 16;
const SIGNATURE: u8 = 0x53;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    Syn = 1,
    Ack = 2,
    Fin = 4,
    Data = 8,
}

/// An explicit resource policy, not a limit imposed by the SMP wire format.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    payload: usize,
}
impl Limits {
    pub fn new(max_payload: usize) -> Result<Self> {
        ensure!(
            max_payload <= crate::MAX_MESSAGE,
            "SMP payload limit exceeds server bound"
        );
        Ok(Self {
            payload: max_payload,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub kind: Kind,
    pub session_id: u16,
    pub sequence: u32,
    pub window: u32,
    payload_length: usize,
}
impl Header {
    pub fn decode(bytes: &[u8; HEADER_LENGTH], limits: Limits) -> Result<Self> {
        ensure!(bytes[0] == SIGNATURE, "invalid SMP signature");
        let kind = match bytes[1] {
            1 => Kind::Syn,
            2 => Kind::Ack,
            4 => Kind::Fin,
            8 => Kind::Data,
            _ => bail!("invalid SMP control flag"),
        };
        let length = u32::from_le_bytes(bytes[4..8].try_into()?) as usize;
        ensure!(
            length >= HEADER_LENGTH,
            "SMP length is smaller than its header"
        );
        let payload_length = length - HEADER_LENGTH;
        ensure!(
            payload_length <= limits.payload,
            "SMP payload exceeds configured limit"
        );
        ensure!(
            kind == Kind::Data || payload_length == 0,
            "SMP control packet has a payload"
        );
        Ok(Self {
            kind,
            session_id: u16::from_le_bytes(bytes[2..4].try_into()?),
            sequence: u32::from_le_bytes(bytes[8..12].try_into()?),
            window: u32::from_le_bytes(bytes[12..16].try_into()?),
            payload_length,
        })
    }

    pub fn payload_length(self) -> usize {
        self.payload_length
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Packet {
    pub header: Header,
    pub data: Vec<u8>,
}

/// DATA is opaque at this layer, including an empty DATA payload (MC-SMP 2.2.5).
/// A future TDS adapter must separately validate its embedded packet framing.
pub fn encode(
    kind: Kind,
    session_id: u16,
    sequence: u32,
    window: u32,
    data: &[u8],
    limits: Limits,
) -> Result<Vec<u8>> {
    ensure!(
        data.len() <= limits.payload,
        "SMP payload exceeds configured limit"
    );
    ensure!(
        kind == Kind::Data || data.is_empty(),
        "SMP control packet has a payload"
    );
    let length = HEADER_LENGTH + data.len(); // validated bounded policy prevents overflow
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(length)?;
    bytes.extend([SIGNATURE, kind as u8]);
    bytes.extend(session_id.to_le_bytes());
    bytes.extend((length as u32).to_le_bytes());
    bytes.extend(sequence.to_le_bytes());
    bytes.extend(window.to_le_bytes());
    bytes.extend(data);
    Ok(bytes)
}

/// Incremental decoder retaining at most one bounded packet, with no output queue.
/// Malformed framing is fatal; callers must close the transport, not resynchronize.
pub struct Decoder {
    limits: Limits,
    bytes: [u8; HEADER_LENGTH],
    header_bytes: usize,
    header: Option<Header>,
    data: Vec<u8>,
    closed: bool,
}
impl Decoder {
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            bytes: [0; HEADER_LENGTH],
            header_bytes: 0,
            header: None,
            data: Vec::new(),
            closed: false,
        }
    }

    /// Consume through at most one packet and return its exact consumed prefix.
    /// The caller owns the remaining input and chooses when to decode another
    /// packet. A nonempty input either makes progress, emits a packet, or errors.
    pub fn decode(&mut self, input: &[u8]) -> Result<(usize, Option<Packet>)> {
        ensure!(!self.closed, "SMP decoder is closed");
        let result = self.decode_inner(input);
        if result.is_err() {
            self.close();
        }
        result
    }

    fn decode_inner(&mut self, input: &[u8]) -> Result<(usize, Option<Packet>)> {
        let mut consumed = 0;
        if self.header.is_none() {
            let count = input.len().min(HEADER_LENGTH - self.header_bytes);
            self.bytes[self.header_bytes..self.header_bytes + count]
                .copy_from_slice(&input[..count]);
            self.header_bytes += count;
            consumed += count;
            if self.header_bytes != HEADER_LENGTH {
                return Ok((consumed, None));
            }
            let header = Header::decode(&self.bytes, self.limits)?;
            // Reserve only after validating the complete bounded header. This
            // avoids quadratic reallocation for one-byte socket fragments.
            self.data.try_reserve_exact(header.payload_length)?;
            self.header = Some(header);
        }
        let header = self.header.expect("complete header was decoded");
        let count = (input.len() - consumed).min(header.payload_length - self.data.len());
        self.data
            .extend_from_slice(&input[consumed..consumed + count]);
        consumed += count;
        if self.data.len() != header.payload_length {
            return Ok((consumed, None));
        }
        let packet = Packet {
            header,
            data: std::mem::take(&mut self.data),
        };
        self.header = None;
        self.header_bytes = 0;
        Ok((consumed, Some(packet)))
    }

    /// End of the underlying byte stream. A partial header/payload is truncation.
    /// A clean framing boundary does not prove a valid session FIN lifecycle.
    pub fn finish(&mut self) -> Result<()> {
        ensure!(!self.closed, "SMP decoder is closed");
        let complete = self.header_bytes == 0;
        self.close();
        ensure!(complete, "truncated SMP packet at end of stream");
        Ok(())
    }

    fn close(&mut self) {
        self.closed = true;
        self.header = None;
        self.header_bytes = 0;
        self.data = Vec::new();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(source: &str) -> Vec<u8> {
        source
            .split_whitespace()
            .map(|b| u8::from_str_radix(b, 16).unwrap())
            .collect()
    }
    fn limits() -> Limits {
        Limits::new(65535).unwrap()
    }
    fn frame(kind: Kind, sid: u16, sequence: u32, data: &[u8]) -> Vec<u8> {
        encode(kind, sid, sequence, 4, data, limits()).unwrap()
    }

    // Adapted from upstream smp.test.ts at 7f71f2081602f8e3051998f5c11f058e65fe24ec.
    #[test]
    fn upstream_syn_and_data_vectors() {
        let syn = hex("53 01 05 00 10 00 00 00 00 00 00 00 04 00 00 00");
        assert_eq!(frame(Kind::Syn, 5, 0, &[]), syn);
        let header = Header::decode(syn.as_slice().try_into().unwrap(), limits()).unwrap();
        assert_eq!(
            (
                header.kind,
                header.session_id,
                header.sequence,
                header.window
            ),
            (Kind::Syn, 5, 0, 4)
        );
        let tds = hex("01 01 00 0b 00 00 01 00 01 02 03");
        let data = encode(Kind::Data, 7, 12, 19, &tds, limits()).unwrap();
        assert_eq!(
            &data[..16],
            hex("53 08 07 00 1b 00 00 00 0c 00 00 00 13 00 00 00")
        );
        let (used, packet) = Decoder::new(limits()).decode(&data).unwrap();
        assert_eq!(used, data.len());
        assert_eq!(packet.unwrap().data, tds);
    }

    #[test]
    fn microsoft_ack_vector_and_opaque_empty_data() {
        // MC-SMP 4.2 ACK example; DATA minimum length is 16 per section 2.2.5.
        let ack = hex("53 02 05 00 10 00 00 00 10 00 00 00 12 00 00 00");
        assert_eq!(encode(Kind::Ack, 5, 16, 18, &[], limits()).unwrap(), ack);
        for data in [&[][..], &[0xff][..]] {
            let bytes = frame(Kind::Data, u16::MAX, u32::MAX, data);
            let (_, packet) = Decoder::new(limits()).decode(&bytes).unwrap();
            let packet = packet.unwrap();
            assert_eq!(packet.header.session_id, u16::MAX);
            assert_eq!(packet.header.sequence, u32::MAX);
            assert_eq!(packet.header.payload_length(), data.len());
            assert_eq!(packet.data, data);
        }
    }

    #[test]
    fn every_split_and_one_byte_fragments_preserve_interleaved_order() {
        let frames = [
            frame(Kind::Syn, 0, 0, &[]),
            frame(Kind::Syn, 1, 0, &[]),
            frame(Kind::Data, 1, 1, &[6, 1, 0, 8, 0, 0, 1, 0]),
            frame(Kind::Fin, 0, 0, &[]),
        ];
        let stream = frames.concat();
        for split in 0..=stream.len() {
            let mut decoder = Decoder::new(limits());
            let mut packets = Vec::new();
            for mut chunk in [&stream[..split], &stream[split..]] {
                while !chunk.is_empty() {
                    let (used, packet) = decoder.decode(chunk).unwrap();
                    assert!(used > 0 && used <= chunk.len());
                    chunk = &chunk[used..];
                    packets.extend(packet);
                }
            }
            assert_eq!(
                packets
                    .iter()
                    .map(|p| (p.header.kind, p.header.session_id))
                    .collect::<Vec<_>>(),
                [
                    (Kind::Syn, 0),
                    (Kind::Syn, 1),
                    (Kind::Data, 1),
                    (Kind::Fin, 0)
                ]
            );
            decoder.finish().unwrap();
        }
        let mut decoder = Decoder::new(limits());
        let mut count = 0;
        for byte in stream {
            let (used, packet) = decoder.decode(&[byte]).unwrap();
            assert_eq!(used, 1);
            count += usize::from(packet.is_some());
        }
        assert_eq!(count, 4);
        decoder.finish().unwrap();
    }

    #[test]
    fn malformed_headers_are_fatal_and_cannot_resynchronize() {
        let valid = frame(Kind::Syn, 5, 0, &[]);
        let mut invalid = Vec::new();
        for flag in 0..=255u8 {
            if ![1, 2, 4, 8].contains(&flag) {
                let mut bytes = valid.clone();
                bytes[1] = flag;
                invalid.push(bytes);
            }
        }
        let mut bad = valid.clone();
        bad[0] = 0x52;
        invalid.push(bad);
        for (kind, length) in [(1, 0u32), (1, 15), (1, 17), (2, 17), (4, 17), (8, u32::MAX)] {
            let mut bytes = valid.clone();
            bytes[1] = kind;
            bytes[4..8].copy_from_slice(&length.to_le_bytes());
            invalid.push(bytes);
        }
        for bytes in invalid {
            let mut decoder = Decoder::new(limits());
            assert!(decoder.decode(&bytes).is_err());
            assert!(decoder.decode(&valid).is_err());
            assert!(decoder.finish().is_err());
            assert_eq!(decoder.data.capacity(), 0);
        }
        for kind in [Kind::Syn, Kind::Ack, Kind::Fin] {
            assert!(encode(kind, 0, 0, 0, &[1], limits()).is_err());
        }
    }

    #[test]
    fn every_incomplete_prefix_fails_only_at_eof() {
        let packet = frame(Kind::Data, 2, 1, &[0x55; 100]);
        for length in 1..packet.len() {
            let mut decoder = Decoder::new(limits());
            assert_eq!(decoder.decode(&packet[..length]).unwrap(), (length, None));
            assert!(decoder.finish().is_err());
            assert!(decoder.decode(&packet).is_err());
        }
        let mut empty = Decoder::new(limits());
        assert_eq!(empty.decode(&[]).unwrap(), (0, None));
        empty.finish().unwrap();
        assert!(empty.decode(&packet).is_err());
    }

    #[test]
    fn limits_apply_before_allocation_and_outputs_are_one_packet_at_a_time() {
        let bound = Limits::new(32).unwrap();
        assert!(Limits::new(crate::MAX_MESSAGE + 1).is_err());
        assert!(encode(Kind::Data, 0, 1, 4, &[0; 33], bound).is_err());
        let oversized = frame(Kind::Data, 0, 1, &[0; 33]);
        let mut decoder = Decoder::new(bound);
        assert!(decoder.decode(&oversized[..16]).is_err());
        assert_eq!(decoder.data.capacity(), 0);
        let packet = encode(Kind::Data, 0, 1, 4, &[0; 32], bound).unwrap();
        let stream = packet.repeat(10000);
        let mut decoder = Decoder::new(bound);
        let (used, result) = decoder.decode(&stream).unwrap();
        assert_eq!(used, packet.len());
        assert_eq!(result.unwrap().data.len(), 32);
        assert_eq!(decoder.data.capacity(), 0);
        // Full u16-sized upper-layer payload remains valid under its configured limit.
        let payload = vec![0x55; 65535];
        let bytes = frame(Kind::Data, 65535, 0, &payload);
        assert_eq!(
            Decoder::new(limits())
                .decode(&bytes)
                .unwrap()
                .1
                .unwrap()
                .data,
            payload
        );
    }

    #[test]
    fn arbitrary_bytes_are_bounded_and_never_panic() {
        let mut seed = 0x1234_5678u32;
        for length in 0..512 {
            let bytes = (0..length)
                .map(|_| {
                    seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                    (seed >> 24) as u8
                })
                .collect::<Vec<_>>();
            let mut decoder = Decoder::new(Limits::new(64).unwrap());
            for chunk in bytes.chunks(7) {
                match decoder.decode(chunk) {
                    Ok((used, _)) => assert!(used > 0 && used <= chunk.len()),
                    Err(_) => break,
                }
                assert!(decoder.data.len() <= 64);
            }
            let _ = decoder.finish();
        }
    }
}
