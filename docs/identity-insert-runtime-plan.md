# SET IDENTITY_INSERT runtime plan

The [pinned SQL Server 2025 capture](../reference/identity-insert.json) retains
34 cases from each of two fresh databases, with an independent-container replay.
The [capture notes](identity-insert-reference.md) identify the observed
session, allocator, diagnostic, and DONE boundaries. This document maps those
boundaries to the current Rust implementation. It does not claim that msduck
implements `SET IDENTITY_INSERT`.

## Current path and missing behavior

| Boundary | Current source | Required consequence |
| --- | --- | --- |
| Batch and SET syntax | [`msduck-sql::batch::parse`](../crates/msduck-sql/src/batch.rs) builds ordered statements. [`session_setting`](../src/engine.rs) allows a fixed list of SET statements, with no `IDENTITY_INSERT`; [`Session::execute`](../src/engine.rs) returns SET completions. The precise SQL AST for qualified/bracketed `SET IDENTITY_INSERT` has not been tested. | Recognize a typed table name and ON/OFF in the deterministic SQL crate. Do not use uppercased `Statement::to_string()` for table identity or silently accept unsupported shapes. Preserve SET command codes 183/184 and error command 253 from the fixture. |
| Session lifetime | [`Session`](../src/engine.rs) owns NOCOUNT, DATEFIRST, transactions and the DuckDB connection, but has no active identity-insert table. [`batch_response_context`](../src/engine.rs) saves and restores some settings around RPC execution. | Keep one resolved ON table per connection, independent of other connections. The fixture shows both sessions can enable the *same* table and that a failed 8107 switch leaves the old table ON. `ROLLBACK` does not undo an observed SET. Do not infer how a SET issued *inside an RPC* persists from this batch-only capture. |
| Preparation | [`validate_prepared_sql`](../src/engine.rs) traverses SET and INSERT statements without executing them. [`RpcState`](../src/rpc.rs) stores prepared SQL; `sp_execute` invokes the session's `prepared_batch`. | Validate syntax and stable declarations without changing session state or allocating an identity at prepare time. The current [`insert::lower`](../src/insert.rs) unconditionally rejects an explicit identity during preflight, so it cannot simply be reused unchanged for an ON-capable prepared INSERT. Bind the current setting at execute time. Prepared SET persistence needs a separate SQL Server probe before a claim. |
| INSERT binding | [`insert::lower`](../src/insert.rs) identifies private identity defaults from catalog columns, rewrites omitted identity columns to an explicit nonidentity list, then rejects every listed identity column through [`identity::EXPLICIT`](../src/identity.rs). [`engine::error_number`](../src/engine.rs) maps that sentinel to 544. | Check the resolved table and current session setting *before* the rewrite. OFF + explicit listed value produces 544. ON + explicit value **without a column list** produces 8101. ON + the captured `INSERT table(v) VALUES(...)` shape produces 545 when the identity is omitted; `DEFAULT VALUES` and other source shapes still need probes. Preserve one source evaluation and statement atomicity across VALUES, SELECT, OUTPUT and prepared paths; do not let a DuckDB column-count error stand in for 8101. |
| Allocation | [`identity`](../src/identity.rs) recognizes a private `nextval` default and persists seed/increment definitions. [`identity_metadata`](../src/identity_metadata.rs) derives `IDENT_CURRENT` from native sequence state. | Explicit ID 100, then lower ID 5, then another session's 200 yields next implicit ID 201 after OFF. An explicit 50 on a `(10,2)` table yields next implicit ID 52. The explicit-value advance must update the same shared, durable allocator seen by `IDENT_CURRENT`, including rollback and reopen behavior. Do not introduce a separate session counter or transactional `MAX(id)` lookup. Check whether bundled DuckDB supports an atomic non-rollback advance; if not, a scoped native change is required before public execution. |
| Diagnostics and wire | [`SqlError`](../crates/msduck-core/src/diagnostic.rs) and [`emit_error`](../src/engine.rs) carry typed errors; [`Execution::statement`](../src/engine.rs) supplies command codes and DONE tokens. | Replay exact 544, 545, 8101 and 8107 numbers, state 1, class 16, messages and DONE status `0x0002`. INSERT failures use command 195; SET failures use 253. Successful SET ON/OFF use 183/184 with status zero. Query descriptors and all rows must match the fixture, including typed empty/error results when present. |

The owner-authored, pinned [mssqlite identity code](https://github.com/mirek/mssqlite/blob/7f71f2081602f8e3051998f5c11f058e65fe24ec/packages/engine/src/identity.ts)
stores an active table key per session, rejects a second ON table with 8107,
checks explicit inserts with 544, and advances its allocator for successful
explicit values. Its [session structure](https://github.com/mirek/mssqlite/blob/7f71f2081602f8e3051998f5c11f058e65fe24ec/packages/engine/src/session.ts)
keeps that key apart from pending, scoped and last identity values. Those are
useful ownership and state-shape precedents, not proof that SQLite's allocator
steps transfer to DuckDB or that mssqlite covers every captured 545/8101 case.

## Required execution order

1. Parse `SET IDENTITY_INSERT` into a typed operation with a table name and
   Boolean state. Resolve the table against an explicit catalog snapshot in
   the root; distinguish a missing/nonidentity table from a valid target. Store
   a stable catalog identity, not merely the textual spelling, so
   `[dbo].[alpha]` and `dbo.alpha` address the same table. If a different table
   is already ON, report 8107 without changing the state. ON for the same table
   and OFF for the active table should be separately probed before assuming
   idempotence or behavior for OFF on another table.
2. Bind each INSERT target before source execution. Determine whether it has a
   listed identity column, an omitted identity, or positional values without a
   list. Apply the session gate and the diagnostic proven for that input shape,
   then perform the ordinary typed storage conversion. Do not authorize
   explicit writes merely because another connection has ON for that table.
   The captured errors are statement failures, with no published row or
   speculative OUTPUT.
3. On a successful explicit write, advance the shared sequence only when the
   value passes its current high-water mark for positive increments (and the
   corresponding low-water mark for negative increments). The fixture proves
   positive increments only; negative-increment rules need a probe. Coordinate
   allocation with concurrent sessions, failed inserts, rollback, WAL replay
   and `TRUNCATE`; preserve existing non-rollback allocation guarantees.
4. Preparation must not execute the SET or INSERT, change the active table, or
   advance the allocator. Execute-time binding reads the current session state.
   The observed transaction rollback keeps SET ON, so the state cannot live in
   DuckDB's transaction. RPC option restoration must be decided from reference
   evidence rather than inferred from existing NOCOUNT handling.

## Ordered successor scopes

These are **proposals**, not published or claimable tasks. Publish them only
after checking reservations and recording a handoff where necessary. Each
stage should run the verification required by `AGENTS.md` for its changed
behavior.

| Stage | Proposed exact paths | Gate and acceptance |
| --- | --- | --- |
| SQL operation | `crates/msduck-sql/src/identity_insert.rs`, `crates/msduck-sql/src/lib.rs`, `crates/msduck-sql/tests/identity_insert.rs` | After the active SQL `lib.rs` reservation (`string-agg-rules-v1`) is released. Inspect the real AST for qualified/bracketed ON/OFF and semicolon-free batches; return a typed operation without catalog/I/O effects. Reject malformed shapes and verify preparation can inspect syntax without applying it. |
| Shared allocator | `src/identity.rs`, `tests/identity_insert_allocator.rs` | After `identity-sequence-dependency-v1` releases `src/identity.rs`. Prove or implement an atomic explicit-value advance on the existing private sequence. Focus on high/low, positive/negative increments, concurrent sessions, failed writes, rollback, reopen and WAL. If the bundled DuckDB needs a patch, publish a new scoped dependency rather than hiding it in this stage. |
| Session, INSERT and wire | `src/engine.rs`, `src/insert.rs`, `tests/identity_insert.rs`, `tests/identity_insert.test.mjs`, `docs/identity.md`, `README.md`, `ROADMAP.md` | After both prior stages and an explicit handoff of the blocked `result-alignment-v1` engine claim. Apply SET and INSERT rules once per statement, preserve statement atomicity and typed diagnostics, and replay every captured row, descriptor, error and ordered DONE token. Check source-evaluation count and two-session isolation. If RPC setting persistence or `src/rpc.rs` changes are needed, first capture the missing behavior and publish a nonoverlapping successor scope after its current reservation. |

The root engine is also named by backlog `multi-database-statements-v1`, while
`src/identity.rs` is currently claimed. A blocked task state does not release
its file claim. None of the successor scopes may be claimed concurrently with
overlapping ready work. The SQL stage can proceed independently only after its
own export-file reservation clears.

Use the retained case names as differential assertions: both sessions' ON/OFF
transitions, 8107 conflicts, 8101 without a column list, 545 implicit inserts
while ON, 544 explicit inserts while OFF, high and low explicit values, the
next implicit IDs 201 and 52, and the setting surviving transaction rollback.
Compare raw DONE status and command codes as well as decoded values. The
fixture does **not** establish temporary tables, permission and missing-table
diagnostics, trigger/procedure nesting, negative increments, reconnects,
prepared RPC setting lifetime, or every failed-write allocation boundary;
those remain unknown and need fresh SQL Server evidence before broad claims.
