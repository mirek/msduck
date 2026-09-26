# Deterministic SMP session state

`crates/msduck-tds/src/smp_session.rs` tracks the per-SID state needed above the
bounded frame codec. It has no socket, TDS message, database, clock or process
state. `Sessions::receive` consumes a validated `smp::Header` and returns one
adapter action; `send_data`, `release` and `send_fin` similarly return explicit
actions. `Opened` tells the adapter to allocate per-SID TDS state. A rejected
event does not mutate the session. The caller owns the DATA
payload, verifies it holds exactly one complete TDS packet, and must bound its
own outgoing packet queues. In particular, `send_data` returning `None` means
peer credit is exhausted; it does not authorize retaining more unbounded data.

The client opens a unique SID with SYN sequence 0. Both DATA counters start at
zero and only DATA increments them. The server initially advertises four receive
slots, and the SYN's WNDW establishes outbound credit. WNDW is the **inclusive**
maximum DATA sequence number: a SYN WNDW of 4 permits outgoing DATA 1–4, then
blocks DATA 5 until an ACK or DATA carries a later peer window. This follows the
[MC-SMP header](https://learn.microsoft.com/en-us/openspecs/windows_protocols/mc-smp/4ada62f7-33c2-45bb-980c-f566f5a6c11a)
and [ACK example](https://learn.microsoft.com/en-us/openspecs/windows_protocols/mc-smp/e1138e43-ba11-4c44-9173-09fb765890c2).
The copied upstream TDS skill describes an exclusive window; that note must not
be used for this state machine.

Logical counters are `u64` while wire sequences are their low 32 bits. This
accepts DATA `u32::MAX` followed by 0. Incoming peer-window deltas use serial
arithmetic: a forward change may wrap, but a delta over half the 32-bit space
is rejected as backward/ambiguous. The resource policy allows at most 65,535
credits and 65,536 active SIDs, so a legitimate forward jump is never ambiguous.
It rejects a zero **initial** peer window; zero after rollover can be valid.
Receive credit advances only when the upper layer calls `release` for delivered
packets. It emits an ACK with the new inclusive WNDW; merely receiving a packet
does not release capacity. These limits are server policy, not wire limits.

FIN carries the sender's last DATA sequence and does not increment it. The
machine accepts local-then-peer and peer-then-local FIN, rejects duplicate FIN,
and removes a SID only when both FINs have occurred. It chooses the stricter
permitted policy of rejecting a FIN whose sequence differs from the last
received DATA; [MC-SMP permits such an error](https://learn.microsoft.com/en-us/openspecs/windows_protocols/mc-smp/a238eeb3-5e51-48fb-8aaa-e592c96e9097).
It [ignores DATA received after local FIN](https://learn.microsoft.com/en-us/openspecs/windows_protocols/mc-smp/c59cde09-9a63-4be2-a955-b1f9b5bd7eb0)
without changing credit, and rejects DATA after peer FIN. On `PeerFin`, the
adapter must decide when to call `send_fin`, including cancellation and payload
cleanup. Unknown SID or duplicate control packets remain fatal to the caller.

The integration test imports this new module by path because `lib.rs` is owned
by another task; its eventual library export is deferred. Neither this module
nor its tests enable PRELOGIN MARS. Root integration still needs MARS negotiation,
per-SID request/response framing, bounded fair write scheduling, Attention,
shared physical-session and transaction rules, TLS transport ownership, and
real-client interleaving tests. Passing pure state tests does not establish MARS
interoperability.
