//! Deterministic lifecycle for one non-MARS request with a buffered response.
//! Effects are instructions to the owning adapter, not completed I/O operations.
//! Native interrupt fencing and packet/token construction belong to that adapter.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestId(u64);

impl RequestId {
    pub fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// A complete, validated request; partial/IGNORE messages stay in framing.
    Admit,
    WorkerStarted(RequestId),
    /// Native work, interrupts and cleanup have all stopped for this request.
    WorkerQuiesced(RequestId),
    /// Bind incoming Attention immediately to the currently admitted request.
    Attention,
    /// Adapter-side cancellation already bound to a request, possibly delayed.
    Cancel(RequestId),
    ResponseEomWritten(RequestId),
    AttentionEomWritten(RequestId),
    Disconnect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Dispatch(RequestId),
    Execute(RequestId),
    SkipExecution(RequestId),
    /// Latch cancellation; never directly interrupt an unverified native epoch.
    CancelWorker(RequestId),
    /// Release the worker's response, retaining its exact execution outcome.
    QueueResponse(RequestId),
    /// A separate TDS message after the original response's EOM was written.
    QueueAttentionAck(RequestId),
    Retire(RequestId),
    DiscardOutput,
    CloseTransport,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Closed,
    Busy,
    IdentityExhausted,
    UnknownRequest,
    InvalidTransition,
    /// Wire policy is not established by the active-request reference captures.
    IdleAttentionUncaptured,
    RepeatedAttentionUncaptured,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Worker {
    Queued,
    Started,
    Quiesced,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Response {
    Pending,
    Queued,
    Written,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Active {
    id: RequestId,
    worker: Worker,
    response: Response,
    cancel: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lifecycle {
    last_id: u64,
    active: Option<Active>,
    closed: bool,
}

impl Lifecycle {
    pub fn active(&self) -> Option<RequestId> {
        self.active.map(|a| a.id)
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// Apply one event in the reactor's order. Errors leave state unchanged.
    /// Retired IDs are harmless; IDs from another lifecycle must never be used.
    pub fn advance(&mut self, event: Event) -> Result<Vec<Action>, Error> {
        if event == Event::Disconnect {
            if self.closed {
                return Ok(vec![]);
            }
            self.closed = true;
            let mut actions = vec![Action::CloseTransport, Action::DiscardOutput];
            if let Some(a) = self.active.as_mut() {
                if a.worker == Worker::Quiesced {
                    actions.push(Action::Retire(a.id));
                    self.active = None;
                } else if !a.cancel {
                    a.cancel = true;
                    actions.push(Action::CancelWorker(a.id));
                }
            }
            return Ok(actions);
        }
        if event == Event::Admit {
            if self.closed {
                return Err(Error::Closed);
            }
            if self.active.is_some() {
                return Err(Error::Busy);
            }
            let next = self
                .last_id
                .checked_add(1)
                .ok_or(Error::IdentityExhausted)?;
            let id = RequestId(next);
            self.last_id = next;
            self.active = Some(Active {
                id,
                worker: Worker::Queued,
                response: Response::Pending,
                cancel: false,
            });
            return Ok(vec![Action::Dispatch(id)]);
        }
        let id = match event {
            Event::Attention => {
                if self.closed {
                    return Err(Error::Closed);
                }
                let a = self.active.ok_or(Error::IdleAttentionUncaptured)?;
                if a.cancel {
                    return Err(Error::RepeatedAttentionUncaptured);
                }
                a.id
            }
            Event::WorkerStarted(id)
            | Event::WorkerQuiesced(id)
            | Event::Cancel(id)
            | Event::ResponseEomWritten(id)
            | Event::AttentionEomWritten(id) => id,
            Event::Admit | Event::Disconnect => unreachable!(),
        };
        if id.0 == 0 || id.0 > self.last_id {
            return Err(Error::UnknownRequest);
        }
        let Some(a) = self.active.as_mut().filter(|a| a.id == id) else {
            return Ok(vec![]); // Retired request; cannot affect its successor.
        };
        match event {
            Event::WorkerStarted(_) => {
                if a.worker != Worker::Queued {
                    return Err(Error::InvalidTransition);
                }
                a.worker = Worker::Started;
                Ok(vec![if a.cancel || self.closed {
                    Action::SkipExecution(id)
                } else {
                    Action::Execute(id)
                }])
            }
            Event::WorkerQuiesced(_) => {
                if a.worker != Worker::Started {
                    return Err(Error::InvalidTransition);
                }
                a.worker = Worker::Quiesced;
                if self.closed {
                    self.active = None;
                    Ok(vec![Action::Retire(id)])
                } else {
                    a.response = Response::Queued;
                    Ok(vec![Action::QueueResponse(id)])
                }
            }
            Event::Attention | Event::Cancel(_) => {
                if a.cancel || self.closed {
                    return Ok(vec![]);
                }
                a.cancel = true;
                // Once quiesced, cancellation cannot touch DuckDB or replace an
                // execution outcome. The already queued response still drains.
                Ok(if a.worker == Worker::Quiesced {
                    vec![]
                } else {
                    vec![Action::CancelWorker(id)]
                })
            }
            Event::ResponseEomWritten(_) => {
                if self.closed {
                    return Ok(vec![]);
                }
                if a.response != Response::Queued || a.worker != Worker::Quiesced {
                    return Err(Error::InvalidTransition);
                }
                a.response = Response::Written;
                if a.cancel {
                    Ok(vec![Action::QueueAttentionAck(id)])
                } else {
                    self.active = None;
                    Ok(vec![Action::Retire(id)])
                }
            }
            Event::AttentionEomWritten(_) => {
                if self.closed {
                    return Ok(vec![]);
                }
                if !a.cancel || a.response != Response::Written || a.worker != Worker::Quiesced {
                    return Err(Error::InvalidTransition);
                }
                self.active = None;
                Ok(vec![Action::Retire(id)])
            }
            Event::Admit | Event::Disconnect => unreachable!(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exhausted_identity_cannot_wrap_or_mutate_state() {
        let mut state = Lifecycle {
            last_id: u64::MAX,
            ..Default::default()
        };
        let before = state.clone();
        assert_eq!(state.advance(Event::Admit), Err(Error::IdentityExhausted));
        assert_eq!(state, before);
    }
}
