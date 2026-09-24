# Native cancellation control

`request_control::Control` is an imperative adapter for one admitted request on
one DuckDB connection. It owns the connection's thread-safe interrupt handle and
an explicit lifecycle request ID. The server does not yet create these controls;
transport and engine integration remain outstanding.

`run_native` arms a scoped native operation, invokes the caller's closure without
holding the control mutex, then disarms before returning its exact result. A guard
disarms during unwinding too. Overlapping entry and retirement while armed fail.
`cancel` latches cancellation and attempts an interrupt when armed. Later native
statements are rejected once cancelled. `pulse` services a cancelled armed call;
it is harmless before cancellation, between calls, or after retirement.

An interrupt effect runs under the short control mutex. Disarm obtains that same
mutex, so it cannot return to cleanup SQL while an issued interrupt is still in
flight. Retirement permanently revokes the capability. A delayed callback retaining
an old controller cannot interrupt a successor after the controller was retired.
The root adapter must enforce one request per connection and retire its controller
before admitting another; constructing overlapping controllers for the same
connection is outside this API's contract.

This design does not require reusable statement-epoch tokens: cancellation is
permanently latched, so the cancelled controller can never arm a later operation.
Between uncancelled operations a pulse is inert. New requests get new controller
instances; never reset a controller's flag or reuse it for another request.

## Native-entry race and error fidelity

Cancellation can arrive after arming but before DuckDB actually starts the query.
The first interrupt may therefore do nothing. The root reactor or timer driver
must continue calling pulse while cancellation is pending and native work is armed.
It must remain responsive while the worker executes. This module starts no hidden
thread and does not claim one-shot interruption solves the race. Stop the pulse
driver when the worker quiesces; already delayed pulses are harmless after disarm.

The original closure result, including native errors, is returned unchanged inside
the control Result. Control errors identify rejected entry or poisoned coordination;
`is_cancelled` is a separate observation. Do not classify every native error as
cancellation just because a cancel flag raced it. The engine must retain both
observations and use its ordered request state and captured SQL Server semantics.
A panic or poisoned controller requires connection teardown, not silent reuse.

WorkerQuiesced in the deterministic lifecycle must follow disarm and required
cleanup, not merely query return or receipt of Attention. Cleanup SQL must run
outside the cancelled execution scope and only after disarm. Transaction rollback,
RPC setting restoration, final response tokens and ACK ordering still belong to
engine/transport integration; this adapter does not infer those policies.

## Verification

Tests cover cancellation before native entry, blocked subsequent entry, retirement,
stale pulses, nested-entry rejection and preservation of a distinct native error.
A barrier-controlled interrupt callback holds the effect fence while the worker
tries to leave its call; cleanup cannot proceed until the callback is released.

The real DuckDB test deliberately cancels after arming but before the native call,
using channels and a barrier. It then services pulses while the worker executes a
large query, verifies an interruption error, retires the old control, sends delayed
cancellation/pulses and successfully reuses the same connection. Its ten-second
limit is a failure bound, not proof that a fixed sleep caught a running query.
The fixture establishes the native-entry race and reuse behavior, not production
cancellation latency, transaction atomicity, streaming or transport integration.

Run formatting, focused request-control tests, strict workspace Clippy and full
workspace tests on the Linux builder. Independent client/SQL Server differential
checks are required when the server starts using this module.

## Explicit transaction probe

A further native regression begins an explicit transaction, inserts one row,
then interrupts a long read query using the same entry-race synchronization.
DuckDB reports `Current transaction is aborted (please ROLLBACK)` on the next
read. The other connection sees no committed row. The test keeps delayed pulse
callbacks running while the worker executes ROLLBACK and a reuse query, before
retiring the control; neither operation is interrupted and no row becomes visible.

This records a backend limitation, not desired SQL Server behavior. The retained
SQL Server WAITFOR captures preserve the explicit transaction and preceding insert
with XACT_ABORT OFF, whereas this DuckDB computing-query probe aborts its native
transaction. Matching SQL Server computation/write captures are still needed to
establish the exact corresponding differences. Do not implement cancellation by
unconditionally rolling back the whole transaction or replaying prior statements:
that could discard or repeat observable work. Engine integration needs a deliberate
transaction-preserving strategy where reference behavior requires it.

The compiled vendored DuckDB source corroborates this observation:
`ClientContext::ExecuteTaskInternal` in `src/main/client_context.cpp` explicitly
sets `invalidate_transaction = true` for a user-generated INTERRUPT, then passes
that flag to `EndQueryInternal`. Using the pending-query API while still issuing
`duckdb_interrupt` does not by itself avoid this path. A different cancellation
strategy or carefully justified backend change needs separate reference evidence
and transaction/atomicity tests; resetting the interrupt flag cannot restore a
transaction that was already invalidated.
