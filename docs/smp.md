# SMP framing implementation

`msduck-tds::smp` implements deterministic, bounded Session Multiplex Protocol
framing as a prerequisite for MARS. It has no sockets, database handles, clocks,
unsafe code or session scheduler. PRELOGIN still advertises MARS disabled.
A codec passing tests does not establish MARS interoperability.

The header preserves packet kind, session ID, sequence number and receive window.
Its 16-byte layout, signature and exclusive control flags follow Microsoft's
[MC-SMP header](https://learn.microsoft.com/en-us/openspecs/windows_protocols/mc-smp/4ada62f7-33c2-45bb-980c-f566f5a6c11a)
and [control flags](https://learn.microsoft.com/en-us/openspecs/windows_protocols/mc-smp/7fe38277-39de-4592-88cb-19709d998569).
SYN, ACK and FIN contain no payload. DATA length can equal 16: an empty payload
is legal at this framing layer according to
[MC-SMP DATA](https://learn.microsoft.com/en-us/openspecs/windows_protocols/mc-smp/6797c9a3-e1de-4c4e-bc3a-cc5cd0b6efcf).
The codec retains DATA bytes without imposing a TDS packet structure on them.

## API and resource boundary

Construct explicit `Limits` with a maximum payload size, capped by this server's
16 MiB resource limit. This is application policy, not the protocol's u32 length
limit. A future TDS adapter must select an appropriate bound and separately
validate its carried packets against negotiated TDS limits.

`encode` validates the kind/payload combination and limit before allocation, then
returns one complete frame. `Header::decode` takes exactly 16 bytes and rejects
invalid signatures, combined/unknown flags, undersized frames, control payloads
and payload lengths over the configured bound.

`Decoder::decode` returns `(consumed_bytes, optional_packet)` and stops after at
most one complete packet. The caller retains any unconsumed input. This permits
backpressure without an unbounded output queue or copying an entire supplied
socket chunk. Keep calling with the unconsumed suffix only when the consumer can
accept another packet; do not drop that suffix.

The decoder retains one partial header/payload. It reserves payload capacity only
after the header validates, avoiding repeated copying for one-byte fragments.
Complete payload ownership transfers to the caller. The caller must also bound
its own queues; a bounded codec cannot bound data retained by its consumer.

Framing errors close the decoder and release its payload buffer. Further input is
rejected; scanning for another signature would be unsafe because payload bytes
can contain that signature. `finish` closes the decoder and rejects an incomplete
header or payload. Clean end-of-stream framing is not proof of correct FIN/session
lifecycle. There is no implicit decoder reset on an existing connection.

## Reused evidence and verification

SYN/DATA bytes and incremental/malformed-frame scenarios are adapted from
`packages/tds/src/smp.test.ts` in mirek/mssqlite at
`7f71f2081602f8e3051998f5c11f058e65fe24ec`. Attribution and its retained MIT notice
are recorded in [THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md). The ACK vector
also follows Microsoft's [ACK example](https://learn.microsoft.com/en-us/openspecs/windows_protocols/mc-smp/42e6a28f-6384-41b1-a9c6-e06dcd8c5b02).

Seven tests cover independent byte vectors, every two-chunk stream split,
one-byte fragments and interleaved session IDs, all invalid flag bytes, malformed
lengths/control payloads, fatal decoder reuse, every incomplete packet prefix,
empty DATA, explicit size boundaries, a maximum u16-sized payload, a large input
containing many frames, and deterministic arbitrary-byte inputs. The one-frame
API intentionally differs from upstream's whole-chunk packet-vector return.

Formatting, strict Clippy for the three deterministic crates and all 218 tests in
those crates passed locally. Full workspace verification is separate and must be
recorded by revision; this does not reuse the concurrently running result-alignment
branch's checks as evidence for the SMP change. No MARS client/server test can
pass through this codec yet because transport integration is not implemented.

## Remaining session work

A later deterministic state machine must validate session establishment, DATA
sequence progression/wrap, ACK/window updates, FIN races and session limits.
It must produce explicit actions for root adapters to perform. Do not validate
these rules by inspecting a header without the relevant per-session state.

The copied upstream notes describe the window as exclusive, but Microsoft's
header definition makes it the maximum permitted sequence number. Treat it as
inclusive when implementing flow control and account for sequence wrap explicitly.
The current codec preserves the field without interpreting it.

Root integration still needs per-session request framing, shared physical
connection/transaction rules, cancellation, bounded outgoing queues, fair writes
and TLS transport ownership. Preserve original-response completion and separate
Attention acknowledgement. Enable MARS only after negotiated transport and real
client interleaving/reuse tests pass. See the protocol inventory in PR #14 for
those separately scoped tasks.
