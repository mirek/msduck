# Incremental TDS framing

`msduck_tds::framing::Decoder` assembles messages without reading sockets or
blocking. `feed(input)` returns consumed bytes and at most one complete message.
The caller retains and later resubmits any unconsumed input. This avoids building
an unbounded list of messages when a read contains many complete requests and
lets the reactor interleave framing with worker events and output progress.

Incomplete state contains one eight-byte header and a payload bounded by the
configured message limit (at most MAX_MESSAGE). Allocation reserves only validated
packet body lengths. The caller's whole input slice is never copied into retained
state. EOF must be passed to `finish`: truncated headers, packet bodies and
non-final messages fail. Fatal input or allocation errors poison the decoder and
release retained payload storage; subsequent input cannot resynchronize it.
Clean EOF is terminal as well.

The decoder preserves the existing root `read_message` semantics: packet lengths
from 8 through negotiated packet size, full non-final packets, consistent packet
type, repeated or incrementing packet IDs (including wraparound and the existing
Tiberius convention), bounded assembled payload, accumulated status/IGNORE bits,
and completion only at EOM. Configuration accepts negotiated sizes 512–32767;
packet size changes are allowed only between messages. Reserved status bits and
message-type-specific payload validation remain separate protocol validation,
as in the existing reader. An IGNORE message is returned with its flag intact;
framing does not execute or interpret its payload.

The decoder does not yet replace root blocking I/O. Transport integration must
retain unread tails, call finish on EOF, reject poisoned connections and preserve
TLS handshake boundaries. Attention receipt still needs the request lifecycle,
worker cancellation fence and responsive duplex transport from
[the design](attention-design.md). This module alone cannot cancel a running query.

Tests cover every split position and bytewise delivery, multiple messages in one
input, zero-body Attention, exact/overflow limits, packet-ID wrap and repeated IDs,
type changes, short non-final packets, malformed advertised lengths before their
bodies arrive, every incomplete EOF prefix, fatal-state reuse and packet-size
changes. All 48 captured outbound Attention packets in
[the retained reference](../reference/attention.json) are replayed across every
split boundary. The captured packets supplement malformed and fragmented fixtures;
they are not evidence of runtime integration or complete TDS compatibility.
