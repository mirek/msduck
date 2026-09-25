//! Deterministic session identity state, independent of table allocation and I/O.
//!
//! The outer frame persists across ordinary batches. RPCs, procedures, and
//! triggers enter nested frames. A nested insert changes the session's last
//! identity, but leaving its frame restores the caller's scoped identity.
//! Allocation alone is not publication: call `publish_success` only after an
//! insert succeeds. Rollback and failed inserts do not rewind this state.

const DECIMAL38_LIMIT: i128 = 100_000_000_000_000_000_000_000_000_000_000_000_000;
pub const MAX_NESTED_SCOPES: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScopeError {
    IdentityOutOfRange,
    MaximumNesting,
    NoNestedScope,
    WrongScope,
    TokenExhausted,
}

/// An opaque entry receipt. Only the innermost live receipt can be exited.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScopeToken(u64);

#[derive(Clone, Debug, Eq, PartialEq)]
struct Frame {
    token: Option<ScopeToken>,
    last: Option<i128>,
}

/// The session-wide and current-scope identities for one SQL connection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IdentityScopes {
    session_last: Option<i128>,
    frames: Vec<Frame>,
    next_token: u64,
}

impl Default for IdentityScopes {
    fn default() -> Self {
        Self::new()
    }
}

impl IdentityScopes {
    pub fn new() -> Self {
        Self {
            session_last: None,
            frames: vec![Frame {
                token: None,
                last: None,
            }],
            next_token: 0,
        }
    }

    pub fn session_last(&self) -> Option<i128> {
        self.session_last
    }

    pub fn scope_last(&self) -> Option<i128> {
        self.frames.last().and_then(|frame| frame.last)
    }

    pub fn nested_depth(&self) -> usize {
        self.frames.len() - 1
    }

    /// Enter a child SQL scope without clearing the session-wide identity.
    pub fn enter(&mut self) -> Result<ScopeToken, ScopeError> {
        if self.nested_depth() == MAX_NESTED_SCOPES {
            return Err(ScopeError::MaximumNesting);
        }
        let serial = self
            .next_token
            .checked_add(1)
            .ok_or(ScopeError::TokenExhausted)?;
        let token = ScopeToken(serial);
        self.frames.push(Frame {
            token: Some(token),
            last: None,
        });
        self.next_token = serial;
        Ok(token)
    }

    /// Restore the caller scope. An invalid or stale receipt changes nothing.
    pub fn leave(&mut self, token: ScopeToken) -> Result<(), ScopeError> {
        if self.frames.len() == 1 {
            return Err(ScopeError::NoNestedScope);
        }
        if self.frames.last().and_then(|frame| frame.token) != Some(token) {
            return Err(ScopeError::WrongScope);
        }
        self.frames.pop();
        Ok(())
    }

    /// Publish a successful generated or explicit insert in the current scope.
    /// A failed or zero-row insert must not call this method; its table allocator
    /// can still advance independently. A later rollback does not call `leave`.
    pub fn publish_success(&mut self, value: i128) -> Result<(), ScopeError> {
        if value <= -DECIMAL38_LIMIT || value >= DECIMAL38_LIMIT {
            return Err(ScopeError::IdentityOutOfRange);
        }
        self.session_last = Some(value);
        self.frames.last_mut().expect("root frame exists").last = Some(value);
        Ok(())
    }
}
