//! Deterministic MC-SMP session flow control. The caller owns TDS payloads and I/O.
//! This module is imported by path until its library export has a separate owner.

use std::collections::BTreeMap;

use anyhow::{Result, ensure};

use crate::smp::{Header, Kind};

/// A bounded resource policy. Each session starts with four inbound packet slots.
#[derive(Clone, Copy, Debug)]
pub struct Config {
    max_sessions: usize,
    max_credit: u32,
}

impl Config {
    pub fn new(max_sessions: usize, max_credit: u32) -> Result<Self> {
        ensure!(
            (1..=u16::MAX as usize + 1).contains(&max_sessions),
            "invalid SMP session limit"
        );
        ensure!(
            (4..=65_535).contains(&max_credit),
            "invalid SMP credit limit"
        );
        Ok(Self {
            max_sessions,
            max_credit,
        })
    }
}

/// An adapter instruction, never an I/O operation. `Frame` fields are passed to
/// `smp::encode`; DATA payload bytes are supplied separately by the adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Opened {
        session_id: u16,
    },
    Frame {
        kind: Kind,
        session_id: u16,
        sequence: u32,
        window: u32,
    },
    DeliverData {
        session_id: u16,
    },
    IgnoreData {
        session_id: u16,
    },
    PeerFin {
        session_id: u16,
    },
    Closed {
        session_id: u16,
    },
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Established,
    FinSent,
    FinReceived,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Session {
    pub phase: Phase,
    /// Logical packet counters; wire sequence numbers are their low 32 bits.
    pub sent: u64,
    pub received: u64,
    pub consumed: u64,
    /// Inclusive high-water marks, lifted into the logical counter space.
    pub peer_window: u64,
    pub local_window: u64,
}

pub struct Sessions {
    config: Config,
    sessions: BTreeMap<u16, Session>,
}

impl Sessions {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            sessions: BTreeMap::new(),
        }
    }

    pub fn get(&self, session_id: u16) -> Option<Session> {
        self.sessions.get(&session_id).copied()
    }

    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn sessions_for_test_mut(&mut self, sid: u16) -> &mut Session {
        self.sessions.get_mut(&sid).unwrap()
    }

    /// Apply an already framed/validated SMP header. On error, state and action
    /// output are unchanged. The adapter separately verifies DATA's TDS packet.
    pub fn receive(&mut self, header: Header) -> Result<Action> {
        let sid = header.session_id;
        if header.kind == Kind::Syn {
            ensure!(header.sequence == 0, "SMP SYN sequence must be zero");
            ensure!(
                header.window != 0 && header.window <= self.config.max_credit,
                "SMP SYN window exceeds credit policy"
            );
            ensure!(!self.sessions.contains_key(&sid), "duplicate SMP session");
            ensure!(
                self.sessions.len() < self.config.max_sessions,
                "SMP session limit reached"
            );
            self.sessions.insert(
                sid,
                Session {
                    phase: Phase::Established,
                    sent: 0,
                    received: 0,
                    consumed: 0,
                    peer_window: u64::from(header.window),
                    local_window: 4,
                },
            );
            return Ok(Action::Opened { session_id: sid });
        }

        let previous = *self
            .sessions
            .get(&sid)
            .ok_or_else(|| anyhow::anyhow!("unknown SMP session"))?;
        if header.kind == Kind::Data && previous.phase == Phase::FinSent {
            // MC-SMP 3.1.5.1.1: discard DATA after our FIN, without effects.
            return Ok(Action::IgnoreData { session_id: sid });
        }
        ensure!(
            previous.phase != Phase::FinReceived || header.kind != Kind::Data,
            "SMP DATA after peer FIN"
        );
        let mut next = previous;
        let action = match header.kind {
            Kind::Syn => unreachable!(),
            Kind::Ack | Kind::Data => {
                if header.kind == Kind::Ack {
                    ensure!(
                        header.sequence == previous.received as u32,
                        "SMP ACK sequence changed"
                    );
                } else {
                    ensure!(
                        header.sequence == previous.received.wrapping_add(1) as u32,
                        "out-of-sequence SMP DATA"
                    );
                    ensure!(
                        previous.received < previous.local_window,
                        "SMP DATA exceeds receive window"
                    );
                    next.received = previous
                        .received
                        .checked_add(1)
                        .ok_or_else(|| anyhow::anyhow!("SMP receive counter exhausted"))?;
                }
                // Serial-number comparison: forward deltas can cross u32 wrap;
                // backward windows occupy the other half of sequence space.
                let delta = header.window.wrapping_sub(previous.peer_window as u32);
                ensure!(delta <= u32::MAX / 2, "SMP peer window moved backwards");
                next.peer_window = previous
                    .peer_window
                    .checked_add(u64::from(delta))
                    .ok_or_else(|| anyhow::anyhow!("SMP peer window counter exhausted"))?;
                ensure!(
                    next.peer_window >= next.sent
                        && next.peer_window - next.sent <= u64::from(self.config.max_credit),
                    "SMP peer window exceeds credit policy"
                );
                if header.kind == Kind::Data {
                    Action::DeliverData { session_id: sid }
                } else {
                    Action::None
                }
            }
            Kind::Fin => {
                ensure!(
                    header.sequence == previous.received as u32,
                    "out-of-sequence SMP FIN"
                );
                match previous.phase {
                    Phase::Established => {
                        next.phase = Phase::FinReceived;
                        Action::PeerFin { session_id: sid }
                    }
                    Phase::FinSent => Action::Closed { session_id: sid },
                    Phase::FinReceived => anyhow::bail!("duplicate SMP FIN"),
                }
            }
        };
        if matches!(action, Action::Closed { .. }) {
            self.sessions.remove(&sid);
        } else {
            self.sessions.insert(sid, next);
        }
        Ok(action)
    }

    /// Reserve and number one outgoing DATA packet. The caller must not queue
    /// unbounded payloads while this returns a blocked result.
    pub fn send_data(&mut self, sid: u16) -> Result<Option<Action>> {
        let session = self
            .sessions
            .get_mut(&sid)
            .ok_or_else(|| anyhow::anyhow!("unknown SMP session"))?;
        ensure!(session.phase != Phase::FinSent, "SMP DATA after local FIN");
        if session.sent >= session.peer_window {
            return Ok(None);
        }
        let sent = session
            .sent
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("SMP send counter exhausted"))?;
        session.sent = sent;
        Ok(Some(Action::Frame {
            kind: Kind::Data,
            session_id: sid,
            sequence: sent as u32,
            window: session.local_window as u32,
        }))
    }

    /// The upper layer has released `count` previously delivered packet slots.
    /// Credit is advertised only now, not when a DATA packet merely arrives.
    pub fn release(&mut self, sid: u16, count: u32) -> Result<Action> {
        let session = self
            .sessions
            .get_mut(&sid)
            .ok_or_else(|| anyhow::anyhow!("unknown SMP session"))?;
        ensure!(
            count != 0 && u64::from(count) <= session.received - session.consumed,
            "SMP release exceeds pending packets"
        );
        let consumed = session
            .consumed
            .checked_add(u64::from(count))
            .ok_or_else(|| anyhow::anyhow!("SMP consumed counter exhausted"))?;
        let window = session
            .local_window
            .checked_add(u64::from(count))
            .ok_or_else(|| anyhow::anyhow!("SMP local window counter exhausted"))?;
        session.consumed = consumed;
        session.local_window = window;
        Ok(Action::Frame {
            kind: Kind::Ack,
            session_id: sid,
            sequence: session.sent as u32,
            window: window as u32,
        })
    }

    pub fn send_fin(&mut self, sid: u16) -> Result<Action> {
        let session = self
            .sessions
            .get_mut(&sid)
            .ok_or_else(|| anyhow::anyhow!("unknown SMP session"))?;
        ensure!(session.phase != Phase::FinSent, "duplicate local SMP FIN");
        let action = Action::Frame {
            kind: Kind::Fin,
            session_id: sid,
            sequence: session.sent as u32,
            window: session.local_window as u32,
        };
        if session.phase == Phase::FinReceived {
            self.sessions.remove(&sid);
        } else {
            session.phase = Phase::FinSent;
        }
        Ok(action)
    }
}
