# Rust cancellable materialized reads

The patched DuckDB Rust `Statement::query_arrow_cancellable_read` binds parameters
normally and returns `Some(Arrow)` for completion, `None` for a drained cancellation,
or a native error. It reuses the existing executed-result and Arrow conversion
path. A shared atomic flag is the only cross-thread cancellation input. Native
pending task calls, interruption, drain and result transfer stay on the worker.

Prepared statements must report SELECT and read-only properties before pending
execution starts. A pre-set flag returns cancellation without starting execution.
An observed flag before result transfer wins over completion; native errors from
a task poll remain errors. This policy is not yet a claim about TDS completion races.
The native drain repeats eligibility/active-result validation. An unexpected
eligibility/drain error requires the caller to discard the connection; it must
never be presented as successful cancellation or worker quiescence.

Pending handles are owned and destroyed on every return path; an active guard
attempts native drain before destruction on an unexpected exit. Successful result
ownership transfers to the existing Rust executed-result wrapper. Other threads
must not issue native interrupts or operate on the same connection during the call.

Backend read-only properties do not exclude volatile functions or external effects.
The server adapter must establish supported semantics from a bound plan before
using this method. Preparation, binding, writes, side-effecting SELECTs, streaming,
commit and blocked external I/O remain outside the cancellation guarantee. The API
is compiled only for bundled cc builds, matching the private native ABI. Server
execution does not use it yet.

The native regression compares full Arrow batches for completed/empty/NULL results,
checks parameter rebinding after pre-cancellation, preserves conversion errors and
rejects INSERT before any row is written. A volatile test marker sets the atomic
flag from actual native execution of a large read, with one/four threads; prior
transaction writes survive, further writes commit, and a second connection sees
correct visibility. A subprocess watchdog bounds a native hang. This test marker
is a synchronization aid, not permission to cancel arbitrary side-effecting SQL.

## Verification

At `5b27a61d1a96b8e6355fe6b4275e0066d9c84fb5`, Linux verification passed the focused
adapter test, full workspace tests, formatting, all-targets workspace build and
strict all-targets workspace Clippy. The workspace run also passed both corrected
native drain probes, including the gated background-error case. The final commit
only records this evidence. No production client behavior change or client rerun
is claimed; server integration remains separate.
