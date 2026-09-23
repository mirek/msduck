# Transaction recovery compatibility gap

The live SQL Server probes in
`artifacts/compatibility/sql-server-xact-state.json` and their local counterparts
in `xact-state-before.json` established three separate gaps before the declaration/preflight correction below:

- XACT_STATE returns nullable SMALLINT wire metadata (`IntN`, width 2, flags 33),
  including when no transaction is active. The previous adapter emitted width 4.
- Invalid arguments must fail compilation before earlier batch writes execute.
  The previous local implementation accepted `XACT_STATE(1)` and committed the preceding
  insert in the probe; SQL Server emits error 174 and leaves the table empty.
  Wildcard, DISTINCT and OVER forms have separate diagnostics in the captures.
- A caught divide-by-zero with XACT_ABORT OFF leaves SQL Server's explicit
  transaction committable (state 1), with its nesting count intact. DuckDB aborts
  the transaction, so msduck cannot execute the CATCH SELECT and instead emits
  an aborted-transaction error. XACT_ABORT ON is currently rejected; SQL Server
  supports it and reports state -1 in the corresponding CATCH block.

The upstream TypeScript engine keeps `transactionDoomed` in its session and reads
it through `mssqlite_xact_state`. Its tests cover error 3930 on COMMIT of a doomed
transaction and restoration to state 0 after ROLLBACK. That state model alone
cannot repair DuckDB's behavior: SQLite and DuckDB have different error recovery
boundaries.

Inspection of the bundled DuckDB source confirms that
`ClientContext::EndQueryInternal` invalidates an explicit transaction after an
invalidating runtime error. `Exception::InvalidatesTransaction` exempts a small
set of compilation/access errors, not general execution errors. The inspected
`UndoBuffer` exposes full rollback but no public statement savepoint interface.
Simply suppressing transaction invalidation would not establish rollback of a
partially executed INSERT/UPDATE/DELETE and must not be treated as SQL Server
statement atomicity.

Required recovery behavior includes preserving prior writes, undoing every
partial effect of the failing statement, preserving visibility and transaction
isolation, permitting CATCH reads, enforcing doomed-transaction restrictions,
and retaining rollback/disconnect behavior. Validation must cover multi-chunk
DML and native callbacks as well as scalar SELECT failures. A backend recovery
mechanism must establish those invariants before session state flags can claim
full XACT_ABORT/XACT_STATE support.

The syntax/metadata corrections can be implemented independently, but do not
close the transaction recovery gap. The captured differences remain evidence of
unfinished SQL Server compatibility.

Additional compiler probes in `sql-server-xact-state-preflight.json` confirm that
invalid XACT_STATE calls in an unreachable IF branch still reject the batch, and
a same-level TRY/CATCH cannot intercept the compilation error. ALL/DISTINCT forms
produce error 195, state 10, severity 15. OVER produces error 4113, severity 15,
with state 6 for ordinary arguments and state 1 for the wildcard form. OVER takes
precedence over ordinary arity validation; ALL/DISTINCT takes precedence over
OVER. Preserving those distinctions requires a typed compilation diagnostic
through batch preflight and preparation, rather than inferring state from the
message text alone. The shared diagnostic contract now carries explicit severity, so these compiler
errors retain their identity through the runtime adapters.


### XACT_STATE declaration and compilation checks

The deterministic SQL crate validates XACT_STATE signatures during batch
preflight, including unreachable branches, and declares its SMALLINT result.
The root supplies transaction activity at execution time; prepared statements
therefore do not capture the state at preparation. Typed syntax diagnostics now
carry severity through both batch and preparation RPC responses.

Eight saved SQL Server probes match the new local capture exactly, including
metadata, errors, completion events and a write guard. Evidence is retained in
`artifacts/compatibility/xact-state-after.json`. The two transaction-error
recovery probes still differ: XACT_ABORT OFF cannot yet preserve a usable
transaction after a backend runtime error, and XACT_ABORT ON remains unsupported.
This change does not implement the uncommittable state or statement undo.


All six additional compiler probes also match exactly in
`artifacts/compatibility/xact-state-preflight-after.json`.

### Native storage requirements for statement recovery

Inspection of the bundled DuckDB implementation adds a second recovery boundary:
`src/storage/local_storage.cpp` updates and deletes transaction-local rows using
`TransactionData(0, 0)`. Deletes also modify append indexes and a deleted-row
counter. `LocalStorage::Rollback` clears all transaction-local table entries,
while `DuckTransaction::Rollback` invokes both local-storage rollback and the
main undo buffer. A position in the main undo buffer alone therefore cannot
restore the start of a failed statement. A native savepoint implementation must
also restore local row contents, deletion state and indexes, and account for
optimistic row collections and schema changes. Earlier successful writes must
remain visible to the transaction and survive a subsequent successful commit.


Two additional live SQL Server probes establish statement atomicity over 5,000
rows, both for previously committed data and data inserted earlier in the same
transaction. A failing UPDATE raises 8134; CATCH reads state 1 and nesting 1.
All 5,000 rows retain their original value, and an earlier log insert survives
COMMIT. The local runs instead abort before CATCH can read, for both storage
paths. Evidence: `artifacts/compatibility/sql-server-transaction-recovery.json`
and `transaction-recovery-before.json`. Even a cleanup guarded by
`IF @@TRANCOUNT>0` currently fails because evaluating the predicate invokes the
aborted backend; these captures preserve that additional recovery limitation.


Validation of the declaration/preflight correction: both focused client tests
pass, and Linux formatting, strict Clippy and all 335 Rust tests pass. The local
audit completed all 282 captures. Among the 279 preceding cases, each reuse probe
changes only the XACT_STATE width from 4 to 2; the existing system-scalar execution
has the same width correction. One unordered APPLY execution reverses two rows,
retained as four raw cell differences in `xact-state-audit-diff.json`. No other
existing capture fields change. All 345 remote client/harness tests also passed with no failures, cancellations
or skips. All 282 remote audit captures completed. Their raw comparison against macOS
differs only in four cells from two reversed rows in the unordered APPLY query;
`artifacts/remote/linux.local/xact-state-baseline-comparison.json` preserves those
differences. These results do not establish transaction recovery compatibility.


`src/storage/table/update_segment.cpp` also merges subsequent updates by the
same transaction into its existing update-info node. A statement boundary must
preserve intermediate versions inside those nodes as well as newly allocated
undo entries; truncating the undo allocator cannot recover repeated writes to
the same committed row. This is an additional invariant for native savepoints.
