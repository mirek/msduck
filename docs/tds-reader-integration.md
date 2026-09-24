# Root TDS reader integration

`src/tds.rs::read_message` now delegates packet assembly and validation to
`msduck_tds::framing::Decoder`. It retains the existing blocking `Read` API used
by PRELOGIN, TLS handshake framing, LOGIN7 and session requests. The duplicate
packet parser is removed.

The pure decoder's `read_size()` reports the next header/body boundary. The adapter
reads at most that amount into an 8 KiB buffer, so it cannot consume bytes from the
following TDS message. This matters because the caller lends a `Read` without a
persistent lookahead buffer. Short reads make progress normally; Interrupted
errors retry with the same decoder state. Other I/O errors propagate. Clean EOF
returns None; EOF inside a header, packet body or non-final message fails.

Decoder input errors report their typed framing category through anyhow. There is
no stream resynchronization after malformed input. Packet-size configuration is
validated before reading. Framing retains repeated/incrementing packet IDs,
wraparound, bounded payload, status/IGNORE accumulation and EOM requirements.
Negotiated sizes must be 512–32767, as already required by the writer/login path.

Root tests prove exact reader positions between two messages across interrupted
and short reads of different sizes, accumulated IGNORE flags and every truncated
prefix of a fragmented message. The existing round-trip, malformed-input and
native wire tests remain. Pure tests exercise the read hint independently across
several chunk sizes. Full workspace, strict Clippy and independent TCP/TLS clients
must pass before this integration is considered verified.

This is a real server integration of deterministic framing, not yet a responsive
transport reactor. The calling thread still blocks while reading and executing.
Active Attention cancellation requires the worker/transport split and native
interrupt fence described in [the design](attention-design.md). No MARS or new
cancellation behavior is advertised by this change.
