//! Incremental, bounded TDS message assembly. Socket/TLS reads remain root effects.
use crate::{MAX_MESSAGE, Message};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidConfiguration,
    PacketLength,
    ShortNonFinalPacket,
    ChangedType,
    PacketOrder,
    MessageLimit,
    Allocation,
    IncompleteEof,
    Poisoned,
    Finished,
    Busy,
}

pub struct Progress {
    pub consumed: usize,
    pub message: Option<Message>,
}

pub struct Decoder {
    packet_size: usize,
    message_limit: usize,
    header: [u8; 8],
    header_used: usize,
    body_remaining: Option<usize>,
    kind: Option<u8>,
    previous_id: Option<u8>,
    status: u8,
    payload: Vec<u8>,
    poisoned: bool,
    finished: bool,
}

impl Decoder {
    pub fn new(packet_size: usize, message_limit: usize) -> Result<Self, Error> {
        if !(512..=32767).contains(&packet_size) || message_limit > MAX_MESSAGE {
            return Err(Error::InvalidConfiguration);
        }
        Ok(Self {
            packet_size,
            message_limit,
            header: [0; 8],
            header_used: 0,
            body_remaining: None,
            kind: None,
            previous_id: None,
            status: 0,
            payload: Vec::new(),
            poisoned: false,
            finished: false,
        })
    }

    /// Change negotiated packet size only between complete messages.
    pub fn set_packet_size(&mut self, packet_size: usize) -> Result<(), Error> {
        self.available()?;
        if !(512..=32767).contains(&packet_size) {
            return Err(Error::InvalidConfiguration);
        }
        if self.header_used != 0 || self.kind.is_some() {
            return Err(Error::Busy);
        }
        self.packet_size = packet_size;
        Ok(())
    }

    /// Stops at the first message boundary, leaving remaining input with caller.
    /// Malformed input is fatal; never resynchronize untrusted bytes after error.
    pub fn feed(&mut self, input: &[u8]) -> Result<Progress, Error> {
        self.available()?;
        match self.feed_inner(input) {
            Ok(progress) => Ok(progress),
            Err(error) => {
                self.poison();
                Err(error)
            }
        }
    }

    /// Clean EOF is terminal too; a decoder never resumes after transport EOF.
    pub fn finish(&mut self) -> Result<(), Error> {
        self.available()?;
        if self.header_used != 0 || self.kind.is_some() {
            self.poison();
            return Err(Error::IncompleteEof);
        }
        self.finished = true;
        Ok(())
    }

    fn available(&self) -> Result<(), Error> {
        if self.poisoned {
            Err(Error::Poisoned)
        } else if self.finished {
            Err(Error::Finished)
        } else {
            Ok(())
        }
    }

    fn poison(&mut self) {
        self.poisoned = true;
        self.payload = Vec::new();
        self.header = [0; 8];
    }

    fn feed_inner(&mut self, input: &[u8]) -> Result<Progress, Error> {
        let mut consumed = 0;
        loop {
            if self.body_remaining.is_none() {
                let n = (8 - self.header_used).min(input.len() - consumed);
                self.header[self.header_used..self.header_used + n]
                    .copy_from_slice(&input[consumed..consumed + n]);
                self.header_used += n;
                consumed += n;
                if self.header_used != 8 {
                    break;
                }
                let h = self.header;
                let length = u16::from_be_bytes([h[2], h[3]]) as usize;
                if !(8..=self.packet_size).contains(&length) {
                    return Err(Error::PacketLength);
                }
                if h[1] & 1 == 0 && length != self.packet_size {
                    return Err(Error::ShortNonFinalPacket);
                }
                if self.kind.is_some_and(|kind| kind != h[0]) {
                    return Err(Error::ChangedType);
                }
                // Preserve the root reader's Tiberius repeated-ID convention.
                if self
                    .previous_id
                    .is_some_and(|id| id != h[6] && id.wrapping_add(1) != h[6])
                {
                    return Err(Error::PacketOrder);
                }
                let body = length - 8;
                if body > self.message_limit.saturating_sub(self.payload.len()) {
                    return Err(Error::MessageLimit);
                }
                // Reserve only this packet's validated body, not an advertised
                // whole message or the size of the caller's input buffer.
                self.payload
                    .try_reserve_exact(body)
                    .map_err(|_| Error::Allocation)?;
                self.kind = Some(h[0]);
                self.previous_id = Some(h[6]);
                self.status |= h[1];
                self.body_remaining = Some(body);
            }
            let remaining = self.body_remaining.expect("validated packet header");
            let n = remaining.min(input.len() - consumed);
            self.payload
                .extend_from_slice(&input[consumed..consumed + n]);
            consumed += n;
            self.body_remaining = Some(remaining - n);
            if remaining != n {
                break;
            }
            self.header_used = 0;
            self.body_remaining = None;
            if self.header[1] & 1 != 0 {
                let message = Message {
                    kind: self.kind.take().expect("validated message type"),
                    status: self.status,
                    payload: std::mem::take(&mut self.payload),
                };
                self.previous_id = None;
                self.status = 0;
                return Ok(Progress {
                    consumed,
                    message: Some(message),
                });
            }
            if consumed == input.len() {
                break;
            }
        }
        Ok(Progress {
            consumed,
            message: None,
        })
    }
}
