# Interrupt-assisted pending read drain

The SQL Server [computation capture](attention-compute-reference.md) requires
prior explicit-transaction writes to survive cancellation with XACT_ABORT OFF.
Ordinary native interrupt polling invalidates DuckDB transactions. The separate
[PR #155 experiment](https://github.com/mirek/msduck/pull/155) showed that simply
abandoning a pending result can stall while draining background workers.

This native-only probe changes the order: service pending execution until there
is evidence of unfinished work, stop polling on the owning worker, issue one
`duckdb_interrupt`, destroy the pending result, then execute a query which forces
context cleanup. The query must see the prior insert, accept a later insert,
and allow explicit commit or rollback with correct cross-connection visibility.
It covers one/four threads and autocommit/commit/rollback. A child-process watchdog
bounds native stalls at 20 seconds. No production server behavior changes.

## Source rationale

In the compiled vendored DuckDB source, `ClientContext::ExecuteTaskInternal`
invalidates a transaction when it observes a user interrupt. This probe does not
call it again after issuing the interrupt. The next query's `InitialCleanup`
calls `CleanupInternal` with default `invalidate_transaction=false`, drains the
old executor, and only then resets the interrupt flag. Background
`Executor::PushError` stores an error and interrupts sibling pipelines.
`duckdb_destroy_pending` alone only closes/deletes the pending result; the next
query remains an essential cleanup barrier.

## Constraints before production integration

This ordering is worker-owned: no concurrent pending task poll, fetch, next query,
or delayed interrupt is allowed across the cleanup boundary. The existing native
controller cannot be wired in unchanged because its pulses could race these phases.

Only a read computation is interrupted here. Writes, commit, external side effects,
volatile functions, streaming, preparation, blocked I/O and cancellation/completion
races remain unproven. This is not statement rollback and cannot justify replaying
prior work or masking an aborted transaction.

The test preserves errors returned during task polling and follow-up queries. The
public pending-destruction API does not return errors accumulated by background
workers during drain. A production design must preserve/classify those errors;
this experiment does not establish that discarding a racing native error is safe.
Ten-second elapsed assertions are failure bounds after native calls return, and
the subprocess watchdog contains a hang; neither establishes a latency guarantee.

## Verification

At `9dbec5d3aed57d5648266541021b7902df3695d7`, the focused six-scenario probe,
full `cargo test --workspace --locked`, `cargo fmt --all --check`, workspace
all-targets build, and strict workspace all-targets Clippy passed on linux.local.
The workspace run exercised the probe again successfully. No server/client
compatibility pass is inferred from this native-only evidence. The final change
after that revision only adds this verification paragraph.

A later workspace run exposed a synchronization failure: background work could
remain active without updating processed-row progress. The probe now registers a
volatile native marker that records actual callback execution before cancellation.
No deadline or expected transaction outcome was relaxed. The marker is test-only.
