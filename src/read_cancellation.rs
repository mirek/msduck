//! Opt-in batch read cancellation. Transport still owns response EOM/ACK ordering.
#[derive(Clone, Copy, Debug)]
pub enum Mode {
    Batch,
    Rpc,
    Prepared,
}
#[derive(Debug)]
pub enum Outcome {
    Finished {
        tokens: Vec<u8>,
        success: bool,
    },
    Cancelled {
        tokens: Vec<u8>,
        attention_ack: [u8; 13],
    },
    /// Native work drained, but session cleanup failed. Close the connection;
    /// this must never become an Attention acknowledgement or reusable session.
    CleanupFailed {
        tokens: Vec<u8>,
    },
}
#[derive(Debug)]
pub(crate) struct CancelledRead {
    pub metadata: Vec<u8>,
}
impl std::fmt::Display for CancelledRead {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("active read cancelled after native drain")
    }
}
impl std::error::Error for CancelledRead {}
