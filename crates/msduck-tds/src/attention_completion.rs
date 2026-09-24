//! Completion ordering captured for cancellation of active computing SELECTs.
//! The caller drains native work and performs any rollback before emitting ACK.
#[derive(Clone, Copy, Debug)]
pub struct ActiveRead {
    pub rollback: bool,
    rpc: bool,
    try_boundary: bool,
}
impl ActiveRead {
    pub fn new(rpc: bool, explicit_transaction: bool, xact_abort: bool, in_try: bool) -> Self {
        let rollback = explicit_transaction && xact_abort;
        Self {
            rollback,
            rpc,
            try_boundary: rollback && in_try,
        }
    }
    pub fn before_rollback(self, out: &mut Vec<u8>) {
        if self.try_boundary {
            crate::done(out, if self.rpc { 0xff } else { 0xfd }, 3, 193, 0);
        }
    }
    pub fn finish(self, out: &mut Vec<u8>) {
        let command = if self.rpc {
            224
        } else if self.try_boundary {
            253
        } else {
            193
        };
        crate::done(out, if self.rpc { 0xfe } else { 0xfd }, 2, command, 0);
    }
}
/// Separate response message, emitted only after the original response EOM.
pub const ACK: [u8; 13] = [0xfd, 0x20, 0, 0xfd, 0, 0, 0, 0, 0, 0, 0, 0, 0];
