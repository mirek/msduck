//! Token streams of trigger bodies. A body runs as a procedure-style batch
//! (DONEINPROC completions); its own RETURNSTATUS and DONEPROC are dropped so
//! the triggering statement's completion follows the body's output.
use msduck_core::diagnostic::SqlError;

const DONE_LENGTH: usize = 13;

/// How a trigger body ended.
#[derive(Debug, PartialEq)]
pub(super) enum End {
    /// The body ran to the end or to RETURN; non-fatal errors such as
    /// RAISERROR remain in the tokens.
    Completed(Vec<u8>),
    /// The body stopped at an error. The tokens exclude the error itself.
    Aborted { tokens: Vec<u8>, error: SqlError },
}

/// Split a body's response into its output and how it ended.
pub(super) fn finish(mut out: Vec<u8>) -> End {
    let Some(done) = out.len().checked_sub(DONE_LENGTH) else {
        return End::Completed(out);
    };
    if out[done] != 0xfe {
        return End::Completed(out);
    }
    let status = u16::from_le_bytes([out[done + 1], out[done + 2]]);
    if status & 2 == 0 {
        // RETURNSTATUS precedes the final DONEPROC of a completed body.
        let end = if done >= 5 && out[done - 5] == 0x79 {
            done - 5
        } else {
            done
        };
        out.truncate(end);
        return End::Completed(out);
    }
    out.truncate(done);
    match last_error(&out) {
        Some((range, error)) => {
            out.drain(range);
            End::Aborted { tokens: out, error }
        }
        None => End::Aborted {
            tokens: out,
            error: SqlError::new(
                50000,
                1,
                "The trigger body ended with an error that could not be decoded.",
            ),
        },
    }
}

/// The last ERROR token, followed only by complete ENVCHANGE or INFO tokens.
fn last_error(out: &[u8]) -> Option<(std::ops::Range<usize>, SqlError)> {
    let mut position = out.len();
    while position > 0 {
        position -= 1;
        if out[position] != 0xaa {
            continue;
        }
        let Some(error) = decode(out, position) else {
            continue;
        };
        let end =
            position + 3 + usize::from(u16::from_le_bytes([out[position + 1], out[position + 2]]));
        if trailing_tokens(&out[end..]) {
            return Some((position..end, error));
        }
    }
    None
}

/// Whether bytes are a sequence of complete length-prefixed ENVCHANGE (0xE3)
/// and INFO (0xAB) tokens.
fn trailing_tokens(mut bytes: &[u8]) -> bool {
    while !bytes.is_empty() {
        if !matches!(bytes[0], 0xe3 | 0xab) || bytes.len() < 3 {
            return false;
        }
        let length = 3 + usize::from(u16::from_le_bytes([bytes[1], bytes[2]]));
        if length > bytes.len() {
            return false;
        }
        bytes = &bytes[length..];
    }
    true
}

/// Decode an ERROR token at `start` if its fields fill its length exactly.
fn decode(out: &[u8], start: usize) -> Option<SqlError> {
    let length = usize::from(u16::from_le_bytes([
        *out.get(start + 1)?,
        *out.get(start + 2)?,
    ]));
    let body = out.get(start + 3..start + 3 + length)?;
    let number = i32::from_le_bytes(body.get(0..4)?.try_into().ok()?);
    let state = *body.get(4)?;
    let severity = *body.get(5)?;
    let characters = usize::from(u16::from_le_bytes(body.get(6..8)?.try_into().ok()?));
    let mut at = 8 + characters * 2;
    let message: Vec<u16> = body
        .get(8..at)?
        .chunks_exact(2)
        .map(|unit| u16::from_le_bytes([unit[0], unit[1]]))
        .collect();
    for _ in 0..2 {
        // Server and procedure names (B_VARCHAR).
        at += 1 + usize::from(*body.get(at)?) * 2;
    }
    if at + 4 != body.len() {
        return None;
    }
    Some(SqlError::from_utf16(number, state, severity, message))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn completed_body() -> Vec<u8> {
        let mut out = Vec::new();
        crate::tds::done(&mut out, 0xff, 0x11, 195, 2);
        out.push(0x79);
        out.extend(0i32.to_le_bytes());
        crate::tds::done(&mut out, 0xfe, 0, 0xe0, 0);
        out
    }

    #[test]
    fn completed_bodies_drop_their_return_status_and_doneproc() {
        let mut expected = Vec::new();
        crate::tds::done(&mut expected, 0xff, 0x11, 195, 2);
        assert_eq!(finish(completed_body()), End::Completed(expected));
    }

    #[test]
    fn aborted_bodies_return_their_error_separately() {
        let mut out = Vec::new();
        crate::tds::done(&mut out, 0xff, 0x11, 195, 2);
        let prefix = out.clone();
        crate::tds::sql_error(
            &mut out,
            &SqlError::new(8134, 1, "Divide by zero error encountered."),
        );
        crate::tds::transaction_env(&mut out, 10, 7);
        let mut rest = Vec::new();
        crate::tds::transaction_env(&mut rest, 10, 7);
        crate::tds::done(&mut out, 0xfe, 2, 0, 0);
        let End::Aborted { tokens, error } = finish(out) else {
            panic!("expected an aborted body");
        };
        assert_eq!((error.number, error.state, error.severity), (8134, 1, 16));
        assert_eq!(error.message, "Divide by zero error encountered.");
        let mut expected = prefix;
        expected.extend(rest);
        assert_eq!(tokens, expected);
    }
}
