# Deterministic request lifecycle

`msduck_tds::request_lifecycle::Lifecycle` models one non-MARS request with a
buffered original response. It performs no I/O, native interruption, timekeeping
or global mutation. The root adapter supplies ordered events and executes returned
actions. This module is not yet wired into the server; active cancellation remains
unimplemented.

Admission allocates a checked, never-reused request ID. Worker entry either starts
execution or skips it when cancellation arrived while queued. Cancellation latches
until retirement. WorkerQuiesced means native work, all interrupt effects and
cleanup have finished; a mere query-return notification is insufficient.

Only then does QueueResponse release the worker's original response. Attention
does not fabricate or overwrite that response: the engine must preserve the
captured diagnostics, transaction effects and completion tokens. An Attention ACK
is queued as a separate message only after ResponseEomWritten. Retirement waits
for AttentionEomWritten when cancelled. A normal completion retires after its
original response EOM. Overlapping admission is rejected.

The [24 retained reference cases](../reference/attention.json) establish the two
message boundaries for active WAITFOR cancellation. They do not establish idle or
repeated-Attention wire policy; those events return explicit errors without
changing state. Adapter-side Cancel(id) is idempotent and differs from an incoming
wire Attention. Wire Attention must be bound immediately on receipt, not left in a
queue where it could accidentally target a later request. IDs belong to one
Lifecycle instance; they are not globally unique connection identifiers.

Disconnect closes transport admission and discards queued output. A live worker
receives cancellation and remains tracked until quiescence. A queued worker that
subsequently starts is told to skip execution and must still report quiescence.
No response or ACK is emitted after disconnect. Retired-ID events cannot alter a
successor. Invalid transitions leave the state unchanged.

This contract deliberately does not call DuckDB's interrupt handle. The root
adapter still needs the request/statement-epoch fence described in
[the cancellation design](attention-design.md), including the race between checking
a cancel flag and entering a native call. It must finish issued interrupts before
cleanup SQL or the next request. An Action is an instruction, never evidence that
its I/O or cleanup has completed.

The current model releases a complete buffered response after worker quiescence,
matching the root server's current execution/output boundary. Streaming results
before execution finishes will require explicit output commitment states and
additional reference evidence. Packet decoding, pre-EOM IGNORE, token creation,
TLS ownership, transaction policy, timers and MARS scheduling remain separate.
Do not claim full cancellation support from passing these pure tests.

Verification covers queued cancellation, both worker-completion/Attention orders,
separate response and ACK writes, stale events after reuse, disconnect while
queued/running, identity exhaustion and unchanged state after invalid transitions.
Bounded event enumeration checks quiescence/output/retirement invariants against
independent action history; reference-backed tests check all 24 captured response
boundaries. Run `cargo test -p msduck-tds --test request_lifecycle`, the complete
deterministic crate suite, formatting and strict Clippy. Native/server/client tests
are required when the module is integrated into runtime execution.
