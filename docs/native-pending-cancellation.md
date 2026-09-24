# Pending native read cancellation probe

The [SQL Server computation reference](attention-compute-reference.md) requires
an explicit transaction's earlier work to survive Attention with XACT_ABORT OFF.
The [interrupt controller probe](native-cancellation-control.md) establishes that
`duckdb_interrupt` invalidates that transaction. This task probes another path;
it does not wire cancellation into the server.

`tests/native_pending_cancellation.rs` uses the pinned DuckDB C API directly,
with owned handles and lifetime-bound connections. It creates a table, optionally
begins an explicit transaction, inserts row 1, then executes 16 pending tasks of
a trillion-pair aggregate. At least one task must report unfinished work; early
completion and native errors fail with their actual outcome. The test destroys
the pending handle without issuing an interrupt, then starts a query on the same
connection to force cleanup of the abandoned executor.

The assertions require the original row to remain readable, a subsequent insert
to succeed, and another connection to see only committed rows. Cases cover one
and four DuckDB threads, autocommit, explicit commit and explicit rollback.
Ten-second bounds detect slow task-loop/cleanup completion after calls return;
they cannot preempt a blocked native call or establish production latency bounds.

## Source rationale

In the compiled vendored DuckDB source (`vendor/libduckdb-sys/duckdb.tar.gz`):

- `src/main/capi/pending-c.cpp`: `duckdb_destroy_pending` calls
  `PendingQueryResult::Close` and deletes the wrapper.
- `src/main/pending_query_result.cpp`: `Close` resets the result's context
  reference; it does not itself drain the executor.
- `src/main/client_context.cpp`: the next request's `InitialCleanup` calls
  `CleanupInternal`, whose `invalidate_transaction` parameter defaults to false
  in `src/include/duckdb/main/client_context.hpp`.
- `CleanupInternal` and `EndQueryInternal` cancel outstanding tasks, release query
  state, and roll back unsuccessful autocommit work. Explicit transaction
  invalidation is conditional on that parameter.
- `src/parallel/executor.cpp`: `CancelTasks` marks the executor cancelled and
  drains outstanding tasks before destroying their pipeline/event state.
  `ExecuteTask` uses partial task processing. Neither fact promises a bounded
  duration for arbitrary operators, functions or external I/O.

This differs from servicing pending tasks while also calling `duckdb_interrupt`,
which still enters the interrupt-error path and invalidates the transaction.

## Limits and next integration requirements

Destroying a pending handle is not a quiescence event. A worker must finish the
cleanup/drain barrier before reporting WorkerQuiesced or admitting another user
request. This probe uses a subsequent query as that barrier and checks its result;
a production adapter must make cleanup ownership and error reporting explicit.

Only a read computation is abandoned here. Pending writes can already have
modified transaction-local storage, so this technique must not be generalized to
statement rollback or atomic write cancellation. Volatile functions, external
side effects, commit, streaming, preparation, output buffering, cancellation races
and worker latency need separate evidence. No prior statements are replayed and
no backend transaction-validity flags are patched.
