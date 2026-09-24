# Transaction recovery boundary

The server must distinguish a failed statement from a failed transaction. A SQL
Server transaction can retain earlier writes and remain readable after an error;
with XACT_ABORT OFF it can also remain committable. The current DuckDB execution
path cannot provide that behavior after a native runtime exception.

This contract belongs to task #83. Runtime integration remains under the existing
engine and transaction adapter claims; this document does not authorize edits to
those scopes. New implementation tasks must use the protected owner-approved
queue and disjoint scopes described in [agent-work.md](agent-work.md).

## Evidence and current limits

At server revision `6b4dcae`, the unchanged-result replay of
`tests/reference/storage-diagnostic.json` matches 54 of 61 cases. Seven remain:
constraint and arithmetic errors with XACT_ABORT ON/OFF, plus uncaught OFF and
caught ON/OFF condition errors. Their raw differences are retained in
`artifacts/compatibility/storage-diagnostic.json`; running the replay regenerates
that ignored artifact. The fixture records SQL Server 17.0.4065.4, compatibility
170, and its pinned container image. All 61 cases were captured identically in
two fresh containers. These results cover that matrix, not all transaction rules.

`tests/backend_transaction_recovery.rs` probes the pinned DuckDB dependency
independently of SQL Server semantics:

- Runtime conversion, constraint and explicit error exceptions make the active
  transaction unusable. ROLLBACK restores usability but discards earlier writes.
- SAVEPOINT is rejected by the parser. The parser failure itself leaves the
  transaction committable; it does not establish a recoverable statement boundary.
- TRY preserves the transaction for a conversion error but returns the same NULL
  as a successful SQL NULL. It does not retain error number, state or message.
- TRY rejects volatile expressions and scalar subqueries before execution.
  Rejection does not consume the tested sequence value.

Run `cargo test --workspace --test backend_transaction_recovery` to recheck these
capabilities after a backend upgrade. A changed result calls for reassessment,
not automatic relaxation of the assertions.

The bundled DuckDB source behind `libduckdb-sys 1.10505.0` explains these results:
`src/common/exception.cpp::Exception::InvalidatesTransaction` excludes parser,
binder, catalog, connection, permission and parameter errors from invalidation;
constraint and invalid-input exceptions are not excluded.
`src/main/client_context.cpp::ExecuteTaskInternal` applies that classification.
The TRY branch in `src/execution/expression_executor/execute_operator.cpp` first
executes a vector, catches execution errors, then retries individual rows. Its
binder in `src/planner/binder/expression/bind_operator_expression.cpp` rejects
volatile expressions and scalar subqueries. Removing only that binder guard
would expose repeated evaluation on failures.

The inspected owner-authored mssqlite revision
`7f71f2081602f8e3051998f5c11f058e65fe24ec` provides useful session policy, not a
portable recovery implementation. `packages/engine/src/execute.ts` records doomed
state in TRY/CATCH and separately checks `honorsXactAbort` at the outer batch
boundary. Its trigger paths and `alter-column.ts` use SQLite SAVEPOINT / ROLLBACK
TO / RELEASE. Reusing those SQL statements against DuckDB cannot preserve their
semantics. Its test that leaves a doomed transaction open after a batch also
conflicts with the retained SQL Server 3998 batch-end capture; do not import that
expectation. See [reference-review.md](reference-review.md) for provenance.

## Required execution contract

Each evaluated value needs a distinct success/NULL/diagnostic outcome. A diagnostic
must retain number, state, severity and exact message units. NULL is never an
error sentinel. Result metadata remains based on logical declarations even when
an execution fails; diagnostics are not public result columns.

Evaluate volatile operands once. Capture operands before applying checked
operations, propagate the first relevant diagnostic through dependent operations,
and do not evaluate an unselected CASE branch merely to find possible errors.
Joins, filters, subqueries, grouping and projection each need explicit outcome
handling. A projection-only preflight does not protect arithmetic in a predicate
or aggregate. Preserve the captured descriptor and completion behavior for a query
that produces metadata before failing.

A write must validate the complete selected input before modifying its target.
Earlier successful statements must remain visible to the same transaction. On a
rejected multirow statement, no prefix may survive. Replaying the transaction is
not recovery: sequence/identity allocations, nondeterministic inputs, snapshots,
external effects and concurrent observations cannot generally be replayed.

Session policy decides whether a returned diagnostic leaves the transaction
committable, dooms it for CATCH, or aborts it. Backend adapters report outcomes;
they must not store a diagnostic in mutable process-global state. Uncaught
RAISERROR and caught RAISERROR have different captured behavior on the pinned
reference build. Keep that policy separate from physical transaction usability.

## Implementation order and acceptance gates

1. Add deterministic checked integer operations over explicitly declared widths
   and bound operand values. Return success, SQL NULL or typed arithmetic failure;
   cover zero divisors, minimum-value division, overflow, signed remainder and
   NULL precedence using captured SQL Server results. Extend the existing
   `reference/integer-overflow.json` evidence where it lacks a boundary. Keep AST,
   backend and session access out of the value rules.
2. Add bounded vector adapters that return value/diagnostic fields without raising
   DuckDB exceptions for expected SQL errors. Validate multi-chunk inputs,
   dictionary/selection vectors, NULLs and volatile operands with call counts.
   Malformed native inputs and allocation failures remain separate engine errors.
   The storage diagnostic adapter is a working pattern, not a complete arithmetic
   implementation.
3. Bind checked outcomes through expression trees and relational plans. Begin with
   a documented supported plan surface, then extend it without hiding fallback
   failures. Prove identical success metadata and exact diagnostic/completion
   streams, as well as transaction reuse. Do not mark arithmetic recovery complete
   while WHERE, JOIN, CASE, aggregate, subquery or DML consumers can still throw
   expected native errors into an explicit transaction.
4. Extend staged writes with constraint-aware validation using converted target
   values, declared collation/NULL uniqueness and complete multirow input. Cover
   existing-row collisions, within-input duplicates, composite keys, defaults,
   updates, foreign keys and CHECK expressions. Expected SQL violations should
   return typed diagnostics before the target mutation. Keep the final native
   constraints enabled: a preflight query alone cannot close concurrency races.
5. Establish a physical statement-recovery guarantee for remaining native write
   failures and explicit SQL savepoints. This requires a separately proven backend
   facility or transaction/storage adapter; it is not supplied by steps 1–4.
   Review backend undo/local-storage/index/catalog cleanup before considering any
   change to exception invalidation. Prove rollback of partial writes, retention
   of earlier writes, concurrent conflict behavior, catalog consistency and reads
   of a doomed transaction. Never merely clear DuckDB's invalid flag.

Every step must retain the failing reference cases until their full rows,
descriptors, diagnostics, completions and next-batch state match. The current
character staging path demonstrates expected-error prevention; it does not prove
constraint recovery, general arithmetic recovery, savepoints or full SQL Server
transaction compatibility.

## Retained historical investigation

The following notes predate the session-state and character-staging work above.
Their statements that XACT_ABORT ON is unsupported describe that older revision.
They retain earlier compiler captures, multi-chunk recovery probes and native
undo-storage findings; none of those recovery failures is claimed resolved here.

> # Transaction recovery compatibility gap
>
> The live SQL Server probes in
> `artifacts/compatibility/sql-server-xact-state.json` and their local counterparts
> in `xact-state-before.json` established three separate gaps before the declaration/preflight correction below:
>
> - XACT_STATE returns nullable SMALLINT wire metadata (`IntN`, width 2, flags 33),
>   including when no transaction is active. The previous adapter emitted width 4.
> - Invalid arguments must fail compilation before earlier batch writes execute.
>   The previous local implementation accepted `XACT_STATE(1)` and committed the preceding
>   insert in the probe; SQL Server emits error 174 and leaves the table empty.
>   Wildcard, DISTINCT and OVER forms have separate diagnostics in the captures.
> - A caught divide-by-zero with XACT_ABORT OFF leaves SQL Server's explicit
>   transaction committable (state 1), with its nesting count intact. DuckDB aborts
>   the transaction, so msduck cannot execute the CATCH SELECT and instead emits
>   an aborted-transaction error. XACT_ABORT ON is currently rejected; SQL Server
>   supports it and reports state -1 in the corresponding CATCH block.
>
> The upstream TypeScript engine keeps `transactionDoomed` in its session and reads
> it through `mssqlite_xact_state`. Its tests cover error 3930 on COMMIT of a doomed
> transaction and restoration to state 0 after ROLLBACK. That state model alone
> cannot repair DuckDB's behavior: SQLite and DuckDB have different error recovery
> boundaries.
>
> Inspection of the bundled DuckDB source confirms that
> `ClientContext::EndQueryInternal` invalidates an explicit transaction after an
> invalidating runtime error. `Exception::InvalidatesTransaction` exempts a small
> set of compilation/access errors, not general execution errors. The inspected
> `UndoBuffer` exposes full rollback but no public statement savepoint interface.
> Simply suppressing transaction invalidation would not establish rollback of a
> partially executed INSERT/UPDATE/DELETE and must not be treated as SQL Server
> statement atomicity.
>
> Required recovery behavior includes preserving prior writes, undoing every
> partial effect of the failing statement, preserving visibility and transaction
> isolation, permitting CATCH reads, enforcing doomed-transaction restrictions,
> and retaining rollback/disconnect behavior. Validation must cover multi-chunk
> DML and native callbacks as well as scalar SELECT failures. A backend recovery
> mechanism must establish those invariants before session state flags can claim
> full XACT_ABORT/XACT_STATE support.
>
> The syntax/metadata corrections can be implemented independently, but do not
> close the transaction recovery gap. The captured differences remain evidence of
> unfinished SQL Server compatibility.
>
> Additional compiler probes in `sql-server-xact-state-preflight.json` confirm that
> invalid XACT_STATE calls in an unreachable IF branch still reject the batch, and
> a same-level TRY/CATCH cannot intercept the compilation error. ALL/DISTINCT forms
> produce error 195, state 10, severity 15. OVER produces error 4113, severity 15,
> with state 6 for ordinary arguments and state 1 for the wildcard form. OVER takes
> precedence over ordinary arity validation; ALL/DISTINCT takes precedence over
> OVER. Preserving those distinctions requires a typed compilation diagnostic
> through batch preflight and preparation, rather than inferring state from the
> message text alone. The shared diagnostic contract now carries explicit severity, so these compiler
> errors retain their identity through the runtime adapters.
>
>
> ### XACT_STATE declaration and compilation checks
>
> The deterministic SQL crate validates XACT_STATE signatures during batch
> preflight, including unreachable branches, and declares its SMALLINT result.
> The root supplies transaction activity at execution time; prepared statements
> therefore do not capture the state at preparation. Typed syntax diagnostics now
> carry severity through both batch and preparation RPC responses.
>
> Eight saved SQL Server probes match the new local capture exactly, including
> metadata, errors, completion events and a write guard. Evidence is retained in
> `artifacts/compatibility/xact-state-after.json`. The two transaction-error
> recovery probes still differ: XACT_ABORT OFF cannot yet preserve a usable
> transaction after a backend runtime error, and XACT_ABORT ON remains unsupported.
> This change does not implement the uncommittable state or statement undo.
>
>
> All six additional compiler probes also match exactly in
> `artifacts/compatibility/xact-state-preflight-after.json`.
>
> ### Native storage requirements for statement recovery
>
> Inspection of the bundled DuckDB implementation adds a second recovery boundary:
> `src/storage/local_storage.cpp` updates and deletes transaction-local rows using
> `TransactionData(0, 0)`. Deletes also modify append indexes and a deleted-row
> counter. `LocalStorage::Rollback` clears all transaction-local table entries,
> while `DuckTransaction::Rollback` invokes both local-storage rollback and the
> main undo buffer. A position in the main undo buffer alone therefore cannot
> restore the start of a failed statement. A native savepoint implementation must
> also restore local row contents, deletion state and indexes, and account for
> optimistic row collections and schema changes. Earlier successful writes must
> remain visible to the transaction and survive a subsequent successful commit.
>
>
> Two additional live SQL Server probes establish statement atomicity over 5,000
> rows, both for previously committed data and data inserted earlier in the same
> transaction. A failing UPDATE raises 8134; CATCH reads state 1 and nesting 1.
> All 5,000 rows retain their original value, and an earlier log insert survives
> COMMIT. The local runs instead abort before CATCH can read, for both storage
> paths. Evidence: `artifacts/compatibility/sql-server-transaction-recovery.json`
> and `transaction-recovery-before.json`. Even a cleanup guarded by
> `IF @@TRANCOUNT>0` currently fails because evaluating the predicate invokes the
> aborted backend; these captures preserve that additional recovery limitation.
>
>
> Validation of the declaration/preflight correction: both focused client tests
> pass, and Linux formatting, strict Clippy and all 335 Rust tests pass. The local
> audit completed all 282 captures. Among the 279 preceding cases, each reuse probe
> changes only the XACT_STATE width from 4 to 2; the existing system-scalar execution
> has the same width correction. One unordered APPLY execution reverses two rows,
> retained as four raw cell differences in `xact-state-audit-diff.json`. No other
> existing capture fields change. All 345 remote client/harness tests also passed with no failures, cancellations
> or skips. All 282 remote audit captures completed. Their raw comparison against macOS
> differs only in four cells from two reversed rows in the unordered APPLY query;
> `artifacts/remote/linux.local/xact-state-baseline-comparison.json` preserves those
> differences. These results do not establish transaction recovery compatibility.
>
>
> `src/storage/table/update_segment.cpp` also merges subsequent updates by the
> same transaction into its existing update-info node. A statement boundary must
> preserve intermediate versions inside those nodes as well as newly allocated
> undo entries; truncating the undo allocator cannot recover repeated writes to
> the same committed row. This is an additional invariant for native savepoints.
