# MERGE execution: deterministic plan and atomic effects

The first-party [46-observation SQL Server capture](https://github.com/mirek/msduck/blob/ffeb20b86507fc15cabfec4059abc4a06a264fbd/reference/merge-execution.json) is the execution oracle for this plan. It was reproduced in four fresh databases on the pinned SQL Server 2025 image; see PR #196 and `docs/merge-execution-reference.md` at that revision. The copied [T-SQL skill](../.agents/skills/t-sql/SKILL.md) and owner-authored `mssqlite` explain likely implementation shapes, but the capture wins where summaries differ. The observations cover selected cases, not the full MERGE language.

## Present boundary

| Area | Current evidence | Remaining requirement |
| --- | --- | --- |
| Parse and diagnostics | `crates/msduck-sql/src/merge.rs::{parse,finish,validate,error_number}` enforces a semicolon and some arm ordering; its unit test covers basic shapes. `crates/msduck-sql/src/dialect.rs` validates MERGE nested in a WITH query. | Bind target/source/ON and action expressions against catalog declarations; retain SQL Server error numbers 10713, 10714 and 5324 at the right boundary. The `sqlparser 0.63.0` `Merge` AST has no TOP member; the captured TOP cases need an explicit retained representation. |
| Execution | `src/engine.rs::Translator::pre_visit_statement` rejects `Statement::Merge`, including WITH-nested MERGE. `Session::execute_inner` has an allowlist without MERGE. | Implement target/source snapshot, action selection, atomic write and response. A passing parse test is not execution coverage. |
| Ordinary DML / OUTPUT | `src/update.rs::lower_captured`, `src/output_join.rs::Binding`, `crates/msduck-sql/src/output.rs::{validate,NativePlan}`, and `src/engine.rs::{lower_output,collect_output_rows,store_output_rows}` implement parts of INSERT/UPDATE/DELETE OUTPUT. | MERGE needs both old/new row images, `$action`, and one combined result or OUTPUT INTO sink. Existing single-operation `Operation` and completion commands do not represent a mixed MERGE. |
| Transaction and completion | `src/engine.rs::{execute,begin_transaction,commit_transaction,rollback_transaction}` wraps only some operations in a self-owned transaction; explicit transactions are kept open. `Execution` carries count/command/kind, and `batch_response` writes `@@ROWCOUNT` plus DONE. `TransactionRequest::Save` explicitly rejects savepoints. | A MERGE must be statement-atomic even inside an existing transaction. A multi-statement UPDATE/INSERT/DELETE sequence cannot use the existing `own_transaction` path to provide that guarantee. |
| Backend | `Cargo.lock` pins `duckdb 1.10505.0` with the bundled engine. [DuckDB's MERGE INTO documentation](https://duckdb.org/docs/stable/sql/statements/merge_into.html) describes matched/unmatched arms, by-source actions and `RETURNING merge_action`; [transaction documentation](https://duckdb.org/docs/current/sql/statements/transactions) describes ACID transactions. | These are candidate primitives, not evidence of SQL Server equivalence in the pinned build. Probe duplicate matches, TOP, CHECK failure inside an ambient transaction, action conditions, row images, RETURNING shape and row counts on the exact bundled revision before selecting a write strategy. |

The currently live `result-alignment-v1` and `concat-integration-v1` claims both reserve `src/engine.rs`/`src/lib.rs` (the former is blocked). `drop-index-batch-syntax-v1` reserves `crates/msduck-sql/src/dialect.rs`. A successor touching these paths must wait for owner-coordinated handoff or completed claims. A protected claim is never reused or moved merely because a PR merged.

## Captured contract to replay

Names below are case names in the retained fixture. Result order is asserted only where the capture's SQL orders a later read; MERGE action order itself is unspecified.

| Captured cases | Value, descriptor, error and completion contract |
| --- | --- |
| `matched update`, `unmatched insert`, `by source delete` | Respectively emit `UPDATE`, `INSERT`, `DELETE` plus correct old/new images. Each MERGE DONE has rowCount 1; the next `@@ROWCOUNT` is 1. Direct `$action` is `NVarChar(20 bytes)`, flags 0, with database collation. The projected non-null integer images are fixed `Int`, flags 8. |
| `mixed actions`, `mixed output rows` | Conditional arms select exactly one UPDATE, one INSERT and two DELETEs. `OUTPUT ... INTO` emits no MERGE result set. A sorted later read contains four action rows (`DELETE`, `DELETE`, `INSERT`, `UPDATE`); destination `action` is `NVarChar(20 bytes)` flags 8, nullable image columns are `IntN(4)` flags 9. MERGE DONE and subsequent `@@ROWCOUNT` are 4; target ends `(1,11),(4,44)`. |
| `cte source` | A CTE-backed source changes one target row; DONE and following `@@ROWCOUNT` are 1. Target then reads `(1,11),(2,20),(4,40),(5,50)`. |
| `duplicate source error` | Error 8672, state 1, class 16; no output rows and target remains `(1,10)`. The error is detected before any visible partial write. |
| `constraint failure` | CHECK error 547, state 0, class 16; mixed UPDATE/INSERT leaves the entire target `(1,10)`. Error message contains the generated database name, so comparison binds only that field. |
| `rollback merge`, `rollback pending rows`, `rollback final rows` | MERGE affects two rows (DONE 2); both are visible inside the explicit transaction and both disappear after ROLLBACK. |
| `top zero`, `top one`, `negative top error` | TOP (0) yields DONE 0 and no target change; TOP (1) yields DONE 1 and one target row. Negative TOP raises 127, state 1, class 15 without a write. The capture does not establish ordering for general TOP selection. |
| `unconditional matched before conditional error`, `repeated matched update error` | Errors 5324 (class 16) and 10714 (class 15) precede execution, with no target change. Existing parse validation partially covers these. |

The reference does **not** prove self-target-source behavior, every TOP ordering choice, trigger/identity effects, multiple matches whose conditions select no action, isolation races, or MERGE OUTPUT source-column binding. Add first-party captures before making a broad compatibility claim. The copied T-SQL skill has a self-target-source restriction that differs from its own mssqlite notes; neither statement is substituted for a live observation.

## Pure action-selection contract

The deterministic core consumes explicit, typed inputs; it does not query DuckDB or evaluate SQL text. A binder first resolves target/source columns, `ON`, ordered arm predicates, assignments, TOP and OUTPUT against a catalog snapshot. The shell then materializes target rows with a stable physical identity and source rows with a separate identity **once**, evaluates the joined candidate relation **once**, and passes typed candidate records to the core:

```text
Candidate { target_id?: TargetRowId, source_id?: SourceRowId,
            target_before?: TypedRow, source?: TypedRow,
            on_match: bool, arm_predicates: [bool; clause_count] }
Action { target_id?, source_id?, clause_index, kind: Insert|Update|Delete,
         target_before?, source?, evaluated_assignments?: TypedRow }
```

Both identities must survive equal-valued duplicate rows. A joined pair is MATCHED; an unpaired source is NOT MATCHED BY TARGET; an unpaired target is NOT MATCHED BY SOURCE. Evaluate clauses in source order within the eligible family, choose the first true condition, and emit at most one action for a candidate. Track target identities across matched actions; reject the captured duplicate-source UPDATE/DELETE case with 8672 before writes. Do not collapse two source rows by value, re-evaluate volatile ON/arm/assignment expressions after a write, or let an earlier action alter a later candidate's classification. Assignment evaluation uses the pre-write target/source images and explicit declared types. The shell applies storage conversion before `inserted` images are exposed.

[Microsoft's MERGE specification](https://learn.microsoft.com/en-us/sql/t-sql/statements/merge-transact-sql?view=sql-server-ver17) places TOP after the full source/target join and removal of rows that qualify for no action; the remaining action rows are unordered. The core should accept an explicitly chosen candidate subset from the shell so its result is deterministic for those inputs, without promising an order SQL Server does not provide. TOP (0) still binds and validates but selects no writes; negative values fail before effects. A separate SQL Server capture and backend probe must determine the exact candidate and tie behavior for mixed-action TOP before supporting that broader shape.

The core returns a complete action list, cardinality and typed image requirements, or a structured diagnostic. It performs no catalog I/O, writes, transaction control, TDS encoding or global session mutation. This keeps clause selection reproducible and testable across engines while leaving actual SQL evaluation and row identity acquisition with the shell.

## Imperative shell and atomicity gate

The shell must bind a writable base target, materialize one snapshot, validate all selected actions and declared storage conversions, then execute the physical writes as **one statement-atomic unit**. It must keep the enclosing user transaction open after a recoverable MERGE error and preserve its prior writes. `src/engine.rs::execute` currently opens a transaction only when none exists for selected DDL/OUTPUT paths, and DuckDB savepoints are not exposed; wrapping three backend DML statements in this path would violate the captured 547 boundary inside an explicit transaction. Do not enable MERGE execution on that basis.

Probe the pinned native DuckDB `MERGE INTO` as one possible atomic write primitive. Require evidence that its selected action set can be constrained to the deterministic plan, duplicate matches can be rejected as 8672 before the write, target storage conversions and CHECK/unique failures preserve the SQL Server boundary, and old/new images plus OUTPUT INTO can be obtained without rerunning expressions. If any requirement fails, choose another backend strategy with genuine per-statement rollback inside an ambient transaction, or keep that shape explicitly unsupported. `RETURNING merge_action` alone does not establish SQL Server `$action` descriptors or old/new image semantics. Native BEGIN/ROLLBACK applies to the whole user transaction and cannot substitute for a statement savepoint.

`OUTPUT INTO` sink writes must share the target write's statement-atomic boundary; they cannot be applied after committing the target. Stage direct OUTPUT rows and publish them only after the whole statement succeeds. Then update `Session::rowcount` and emit the proper DONE count. A failed statement must emit no speculative OUTPUT and must leave `@@ROWCOUNT`/`@@ERROR`, transaction state and wire completion consistent with fresh captures. Use the existing typed result metadata path for `NVarChar(20 bytes)` action and fixed/nullable image descriptors; do not infer descriptors from current row values. Differential tests must compare complete ordered result sets, descriptors, errors, information and DONE tokens, not only final table contents.

## Successor work with exclusive scopes

These are proposed task IDs and scopes, **not claimable registry entries** until the owner publishes them. Keep dependencies and currently reserved files explicit.

| Task | Exact scope | Dependency and acceptance |
| --- | --- | --- |
| `merge-action-core-v1` | `crates/msduck-core/src/merge_action.rs`, `crates/msduck-core/tests/merge_action.rs` | After this plan. Path-import the pure planner without editing `msduck-core/src/lib.rs` (reserved by the legacy datetime export successor). Test each action family, conditional precedence, duplicate identities and 8672, zero actions, explicit TOP subset and single-evaluation inputs against the fixture's action counts. |
| `merge-duckdb-atomicity-probe-v1` | `tests/merge_duckdb_atomicity.rs`, `docs/merge-duckdb-atomicity.md` | After PR #196. Probe the **bundled** DuckDB revision directly in isolated databases, including native MERGE duplicate-source behavior, mixed arms, `RETURNING merge_action`, constraint failure in an already open transaction, rollback and physical row identity. Retain raw observations and state which backend operations can or cannot meet the SQL Server contract. This task does not wire runtime MERGE. |
| `merge-output-contract-v1` | `crates/msduck-sql/src/merge_output.rs`, `crates/msduck-sql/tests/merge_output.rs` | After the action core. Path-import deterministic `$action`/`inserted`/`deleted` projection and OUTPUT INTO binding contracts using explicit logical declarations; compare direct versus stored descriptor expectations. Do not edit shared SQL exports or root adapters. |
| `merge-top-binding-v1` | `crates/msduck-sql/src/merge_top.rs`, `crates/msduck-sql/tests/merge_top.rs` | After additional first-party TOP capture. Design a lossless TOP representation and typed nonnegative evaluation in isolation. Integrating it into `dialect.rs` waits for the `drop-index-batch-syntax-v1` claim to finish or an explicit owner handoff. |
| `merge-runtime-integration-v1` | `src/merge_execution.rs`, `src/engine.rs`, `src/lib.rs`, `tests/merge_execution.rs` | After core, backend probe and output contract, **and** owner coordination for the existing `result-alignment-v1`/`concat-integration-v1` claims on `engine.rs` and `lib.rs`. Wire one atomic write strategy, OUTPUT/INTO, exact errors, transaction behavior, count/metadata and CTE source. Replay all 46 captured cases and add real TDS client tests before declaring execution support. Do not publish as Ready while the shared files remain reserved. |

The first two scopes can be worked in parallel after publication; the runtime task cannot. Any change to a task's scope requires a new owner-published successor rather than editing an active claim. The plan is complete only as a roadmap; SQL Server-compatible MERGE execution remains unimplemented until the runtime and differential work pass.
