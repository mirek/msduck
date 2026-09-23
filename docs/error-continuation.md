# Arithmetic query error continuation

Default-session query errors 8115 and 8134 now end the failed query and allow
remaining batch statements to execute. The failed query retains its metadata
and error token, followed by an error completion token with the correct MORE
flag. @@ERROR contains the error number and @@ROWCOUNT becomes zero. Later
successful statements reset @@ERROR while the batch's internal success result
remains false, so a later success does not hide an earlier uncaught failure.

RPC execution uses DONEINPROC and final DONEPROC tokens. The procedure return
status retains a terminal arithmetic error number and resets to zero after a
subsequent successful statement. Explicit RETURN still chooses its own status.
Failed execution does not publish a new prepared handle.

The current implementation recognizes errors from the described native query
path. It does not establish continuation for every statement or every error
with the same number. Unsupported result shapes may lack that context. Full
XACT_ABORT, arithmetic session options, statement rollback inside explicit
DuckDB transactions and general error disposition remain unfinished. In
particular, continuing interpretation cannot repair a backend transaction that
DuckDB has already aborted; see `docs/transaction-recovery.md`.

`reference/error-continuation.json` contains 11 live SQL Server batch probes.
`reference/error-continuation-rpc.json` contains three direct RPC probes. Raw
results and differences are in
`artifacts/compatibility/error-continuation-comparison.json`:

- All three RPC captures match completely.
- Five batch captures match completely: integer division by zero, decimal
  division by zero, decimal AVG overflow, repeated arithmetic errors followed
  by a successful SELECT, and THROW's existing batch termination.
- A write-before/error/write-after probe now preserves both writes and the
  following result. Its CREATE TABLE completion still has an incorrect count.
- ABS overflow continues correctly but retains existing flags/message/state
  differences. A conversion error stops the batch but retains backend text.
- The literal invalid JSON probe stops the batch but differs in message detail
  and emits metadata absent from SQL Server's capture. More precise compile
  versus runtime error phase handling remains necessary.
- RAISERROR syntax in the probe and XACT_ABORT ON remain unsupported.

A native regression checks later writes, multiple errors, overall failure and
conversion-error termination. Focused Linux client tests cover counters,
metadata/completion ordering and write preservation. The prior four-case
error-metadata matrix now matches its uncaught-error continuation case exactly;
TRY/CATCH completion sequences and textual sp_executesql binding remain open.

This snapshot passed all 364 workspace Rust tests, strict Clippy, formatting
and both focused client tests. Full remote verification subsequently passed all
364 Rust tests and 368 client/harness tests, with zero failures, cancellations or
skips; all 303 audit cases completed. The remote source snapshot was fixed for
the entire run. These checks do not resolve the reference differences above.
