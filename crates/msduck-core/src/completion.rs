//! Completion visibility from explicit execution outcomes and session settings.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    Statement,
    ResultSet,
    Error,
}

/// NOCOUNT suppresses ordinary RPC statement completions, but neither result
/// boundaries (including empty sets) nor errors. SQL batches retain all three.
pub fn visible(rpc: bool, nocount: bool, kind: Kind) -> bool {
    !rpc || !nocount || kind != Kind::Statement
}
