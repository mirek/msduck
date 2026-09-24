# Request cancellation and transport ownership

Design inspected against `f31f14857e3be8005df1d0e875a0e9c32541ba36`.
This is a proposed implementation contract, not a claim that active cancellation
works. It covers one non-MARS session per connection. MARS remains disabled until
its independent scheduling, flow control and transaction semantics are verified.

## Evidence and current gaps

- [`serve_login`](../src/server.rs) reads a complete message, calls
  `Session::batch`, `rpc::State::execute` or `Session::transaction_request`
  synchronously, then writes the complete response. Its type-6 branch emits
  DONE_ATTN only after execution has returned. It cannot receive Attention while
  a query is executing or while a blocking response write is in progress.
- [`read_message` and `write_message_kind`](../src/tds.rs) own blocking whole-message
  I/O. Reads enforce packet length, type consistency, packet-ID conventions and
  `MAX_MESSAGE`; IGNORE is accumulated across fragments. A replacement must retain
  these checks, including the documented Tiberius repeated-ID convention.
- [`tls::accept`](../src/tls.rs) returns one `StreamOwned<ServerConnection,
  TcpStream>` after completing PRELOGIN-wrapped handshake output. Sharing that
  stream behind a mutex would let a blocked read or write prevent progress in
  the other direction. Cloning TCP handles does not clone TLS protocol state.
- [`Session::batch_response_context`](../src/engine.rs) restores RPC settings after
  execution; `batch_response_inner` loops through pending statements, handles
  TRY/CATCH, accumulates tokens and maintains transaction state. There is no
  cancellation input or distinct cancellation outcome. An interrupted native
  error must not accidentally become an ordinary catchable SQL error or allow
  subsequent statements to execute.
- [`Connection::interrupt_handle`](../vendor/duckdb/src/lib.rs) returns an
  `Arc<InterruptHandle>`. [`InterruptHandle::interrupt`](../vendor/duckdb/src/inner_connection.rs)
  is thread-safe and calls `duckdb_interrupt` on the same native connection;
  connection close clears its pointer under the same handle mutex. It has no
  request identity. The vendored `test_interrupt` uses a fixed sleep and checks
  one interrupted query; it does not prove cancellation before native entry,
  transactions, connection reuse or cross-request race safety. Those tests were
  inspected, not rerun for this document.
- [`attention_vector`](../crates/msduck-tds/src/lib.rs) establishes ACK bytes,
  not cancellation of active execution. Existing client tests do not establish
  in-flight cancellation. A passing idle Attention response is insufficient.

The local [TDS skill](../.agents/skills/tds-protocol/SKILL.md), its
[Attention notes](../.agents/skills/tds-protocol/messages.md#22-attention-signal)
and [response patterns](../.agents/skills/tds-protocol/response-patterns.md)
distinguish pre-EOM IGNORE from post-EOM Attention. Retain the original response
message boundary before a separate tabular Attention ACK: payload
`FD 20 00 00 00 00 00 00 00 00 00 00 00`.
Do not concatenate that ACK into the original response. Exact cancellation
response contents, repeated-Attention behavior and transaction effects still
require first-party SQL Server captures.

Inspected upstream `mirek/mssqlite` at
`7f71f2081602f8e3051998f5c11f058e65fe24ec`:
`packages/server/src/connection.ts` uses `schedule` with an `AbortController`,
clears unsent output on `CancellationError`, then calls `respond` separately for
normal response completion and Attention acknowledgement. `onMessage` handles
IGNORE without executing the partial payload; disconnect aborts active work.
Reuse these distinctions and fixture cases. TypeScript scheduling and SQLite
cancellation are not a DuckDB implementation or SQL Server ground truth. Preserve
attribution in [third-party notices](../THIRD_PARTY_NOTICES.md) if code or vectors
are copied; this document copies neither implementation nor test vectors.

## Deterministic lifecycle

Use monotonically increasing, checked request IDs within a connection. IDs never
wrap or get reused. Socket reads and native completions become explicit events;
state transitions return ordered effects without performing I/O, reading clocks,
sharing global state or depending on DuckDB types. Timers supply explicit events.

| State/event | Required transition and effects |
| --- | --- |
| Idle + first request fragment | Allocate ID; enter Receiving with bounded decoder state. Do not execute yet. |
| Receiving + valid EOM | Enter Queued; submit this complete request once. |
| Receiving + IGNORE/EOM | Discard payload without SQL decoding/execution; emit ordinary completion, then return Idle after its EOM is written. |
| Queued + worker start | Enter Running only for the matching ID and uncancelled token. |
| Queued + Attention | Latch cancellation; worker must report skipped execution; finish original response and ACK before reuse. |
| Running + Attention | Latch cancellation for this ID; stop future statements; request native interruption if execution is armed. Enter Cancelling. |
| Running + successful/error completion | Enter Responding; preserve the response unless an ordered cancellation event wins before output commitment. |
| Responding + Attention | Never interrupt another native request. Complete a valid original response boundary, then send a separate ACK. Sent bytes cannot be retracted. |
| Cancelling + worker quiesced | After cleanup and interrupt effects are drained, complete original response and queue ACK. |
| ACK EOM written + worker quiesced | Retire request and cancellation capability; only then admit the next execution. |
| Idle + Attention | No native interrupt. Emit ACK as its own response; retain captured idle semantics. |
| Any state + disconnect/fatal framing error | Stop admission, cancel active work, drain/join worker and drop session. Do not attempt replies after disconnect. |

The reactor imposes an explicit event order for simultaneous completion and
Attention. Test both orders. Completion arriving for a retired ID cannot publish
output or change state. Duplicate cancellation events are idempotent for native
execution; the required number and ordering of wire ACKs must come from captures,
not from an arbitrary one-ACK-per-packet policy. Attention while Receiving must
obey packet framing: reject malformed interleaving, and capture legal pre-EOM
client behavior rather than interpreting Attention bytes as SQL payload.

A response needs separate milestones: queued, first bytes committed, EOM written.
Cancellation cannot splice a token in half or remove already-written bytes. Keep
unsent data at known token boundaries, or finish an already-committed valid
response before ACK. Bound queued bytes and retain control-read progress under
write backpressure. Never emit a synthetic success for a statement with unknown
commit state. Exact ERROR/DONE/RETURNSTATUS behavior is a capture requirement.

## Imperative shell and cancellation fence

One reactor owns the socket, TLS state, incremental TDS input decoder and ordered
output queue. Use readiness-driven nonblocking transport or an equivalent design
that services reads, writes and worker events without holding a blocking TLS
stream lock. Preserve the existing handshake boundary. Apply bounded queues and
per-iteration work budgets so a large result cannot starve Attention reception.
Do not execute a pipelined request early; define rejection/buffering behavior
from reference evidence and enforce strict memory limits.

One worker owns the DuckDB connection, Session, prepared handles, transaction
state and statement diagnostics. It receives a complete request plus its ID and
cancellation capability, and returns a typed outcome: completed, failed,
cancelled or fatal. It never writes the socket. A clone from `try_clone` is a
separate connection and must not be substituted for the active query's interrupt
handle. Release diagnostic scopes and restore RPC settings on every exit path.

The effects interface should distinguish `Start(id)`, `Cancel(id)`,
`Interrupt(id, execution_epoch)`, `WorkerQuiesced(id, outcome)`,
`QueueResponse(id, message)`, `ResponseEomWritten(id)` and `Close`.
Native calls within a batch also need execution epochs: cancellation of one
statement must not race into cleanup SQL or a later statement.

An atomic cancel flag plus a single interrupt call is insufficient: Attention can
arrive after the worker checks the flag but before native execution begins, when
an interrupt may do nothing. Prototype and verify an entry/exit fence before
selecting the native adapter. The protocol must guarantee all of the following:

1. Cancellation remains latched until the request is retired. Check it before
   every execution phase and at batch-loop/result-processing safe points.
2. An interrupt effect validates both request ID and execution epoch while
   holding the short control fence. Native execution never holds that fence for
   its duration, and the reactor never waits for a query to finish under it.
3. If cancellation races native entry, either the worker skips entry, or a
   scoped interrupt driver continues to service the cancelled armed epoch until
   native execution returns. A one-shot interrupt before entry cannot count as
   acknowledged cancellation. A pending-task DuckDB API is an alternative only
   after its real capabilities have been inspected and tested.
4. Worker exit disarms that epoch through the same fence. All issued interrupt
   calls finish before cleanup SQL, response retirement or next-request entry.
   Stop any retry driver on disarm; a delayed timer carries the old epoch and
   becomes a no-op. A mutex around the raw handle alone cannot establish this.
5. Cancellation also stops Rust control-flow work between native calls. Do not
   run subsequent batch statements or TRY/CATCH bodies merely because native
   interruption surfaced as an ordinary DuckDB failure.

The public ACK must follow worker quiescence and required cleanup, not merely
receipt of Attention. If cleanup cannot restore a usable session, close it rather
than acknowledge successful reuse. Teardown cannot detach a still-running worker
and leak its database connection. Apply an explicit bounded shutdown policy;
measure native interruption responsiveness rather than assuming it is immediate.

## Transactions and observable session state

Do not implement cancellation as unconditional transaction rollback. Capture
SQL Server behavior for autocommit, explicit transactions, XACT_ABORT ON/OFF,
TRY/CATCH, prepared execution and transaction-manager requests. Inspect
`@@TRANCOUNT`, `XACT_STATE()`, visibility from a second connection, settings and
prepared-handle reuse after each cancellation. Separate statement atomicity from
transaction lifetime. Check interruption during a write before commit and the
race after commit; a response cancellation cannot undo an already committed write.

Map native interruption to an explicit engine outcome without classifying all
DuckDB errors as cancellation or matching only an error-message substring. A
cancel flag alone is also not evidence that an unrelated execution error was
caused by interruption. Preserve both observations and order them explicitly.
Any DuckDB/SQL Server transactional mismatch requires execution changes and tests;
it must not be hidden by editing captures or automatically resetting the session.

## Bounded successor tasks and acceptance

These proposals are not claimable until published with disjoint scopes. Existing
claims on shared exports, engine, server, RPC and client files must be coordinated;
this design grants no permission to edit them.

1. **Reference capture:** new `scripts/capture-attention.mjs`,
   `reference/attention.json`, `docs/attention-reference.md`. Capture raw TDS
   packets/message boundaries and client events for idle, queued/running,
   completion races, repeated Attention, IGNORE, prepared reuse, TLS and the
   transaction matrix above. Run twice in fresh pinned SQL Server databases.
   For running cancellation, synchronize on observable execution (for example a
   second connection inspecting the known session's active request), not a sleep
   alone. Preserve timing-sensitive observations rather than sorting them.
2. **Pure lifecycle and incremental framing:** proposed
   `crates/msduck-tds/src/request_lifecycle.rs` and `framing.rs` plus dedicated
   tests; export wiring needs its existing owner or an approved successor.
   Enumerate event permutations, stale IDs/epochs, all fragmentation boundaries,
   malformed/oversized input, repeated packet IDs, EOF and IGNORE. Verify ordered
   response/EOM effects, bounded storage and no execution before request EOM.
3. **Native cancellation adapter:** new root adapter module and focused Rust
   tests; coordinate engine/export wiring. Test entry races with explicit barriers,
   active long queries, multi-statement stop, result materialization, cancellation
   during writes, unrelated errors, cleanup and connection drop. Prove stale
   effects cannot interrupt the next query and that other connections still run.
4. **Transport/engine integration:** coordinate current server, TLS, engine and
   RPC owners; implement the reactor/worker boundary and cancellation outcomes.
   Keep non-MARS wire behavior and authentication/handshake guarantees. Use
   transport tests with partial packets, TLS records, backpressure and disconnect
   while execution is active. Assert no concurrent socket/TLS writers.
5. **Independent acceptance:** new cancellation client suite plus test-runner
   registration coordinated with its owner. Cancel genuinely running batch and
   prepared RPC requests; assert client cancellation result, original response
   EOM then exact ACK EOM, bounded completion, and successful follow-up queries.
   Cover both completion race orders, repeated cancellation, IGNORE without
   Attention, transaction observations, disconnect and a concurrent unaffected
   connection. Include plain TCP and required TLS. Keep timeout values explicit
   and distinguish a safety timeout from evidence of running execution.

Pure tests and formatting/Clippy establish only the deterministic contract.
Native integration requires workspace tests, strict Clippy, full independent
clients and the diagnostic audit on an immutable Linux build. Compare exact
reference metadata, errors, return statuses and packet boundaries. Record the
revision and raw differences. Passing this scope does not establish MARS, bulk
cancellation, all stored-procedure semantics or complete SQL Server compatibility.

## Validation of this document

Reviewed source symbols, local skill notes and the pinned upstream implementation;
checked relative links against the checkout. No runtime code changed and no native
build or shared-port test was launched. Cancellation implementation and SQL Server
reference capture remain outstanding.
