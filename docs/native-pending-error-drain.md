# Explicit pending-read error drain

The private bundled-native ABI `msduck_pending_cancel_read_and_drain` accepts an
owned pending result, returning 0 for cancellation/drain, 1 for a native/cleanup
error, or 2 for an unsupported statement without changing it. Its retained error
is available through `duckdb_pending_error` until the normal pending destructor.
This is an internal backend primitive; the server does not call it yet.

Unlike the [public C API experiment](native-pending-drain.md), the operation locks
the context, validates the active result, interrupts and joins executor tasks,
copies the final error, then ends the query. Stale results are rejected before
interrupting anything. Preexisting errors are retained. A non-interrupt executor
error follows native transaction invalidation policy; fatal/internal errors also
invalidate the database, as the normal execution path does. Cleanup errors retain
the prior execution error text. Pure cancellation preserves explicit transactions.

Only SELECT statements with no backend-reported modified databases are eligible.
This is not sufficient to prove absence of volatile functions or external effects:
the eventual adapter must establish eligibility from its own bound execution plan.
Writes, commit, side-effecting SELECTs and streaming are not enabled by this API.
A single worker must own task polling, drain, result destruction and later queries;
no delayed interrupt or concurrent fetch may cross these boundaries. The caller
must close the connection on an unexpected drain/cleanup error until a specific
recovery policy has been verified. Null/invalid pointers are not a safe public API.

`msduck_pending_drain.rs` applies exact, single-match changes to the pinned archive's
pending result class and C API. The original archive is unchanged. Only the bundled
cc backend is covered; linked/system and bundled-cmake builds lack this private ABI.

Native tests cover explicit transaction commit/rollback and autocommit with one
and four threads, retained earlier writes, subsequent writes and cross-connection
visibility. Additional cases reject stale handles without cancelling a newer query,
reject an INSERT without preventing its ordinary completion, and retain a genuine
conversion error. The test process has a 20-second watchdog. A gated scalar callback establishes actual background execution before it is
released to fail concurrently with drain. No pending task poll transfers that error;
the explicit drain must retain the callback failure rather than replace it with
an interrupt. This is an additional check beyond preexisting-error coverage.

## Verification

At `993b420`, the focused native test, full workspace tests, formatting, all-targets
workspace build and strict all-targets workspace Clippy passed on linux.local.
The workspace run repeated the gated background-error and transaction probes.
The final commit only updates documentation. The original native archive remains
unchanged. Server client tests were not rerun for this unused private entry point;
production integration will require independent clients and reference comparisons.
