# Savepoint and nested transaction reference

`reference/savepoint.json` retains 662 ordered SQL Server observations from 16
programs covering `SAVE TRANSACTION`, `ROLLBACK TRANSACTION <savepoint>`, nested
`BEGIN`/`COMMIT`, savepoint names, errors, `XACT_ABORT`, TRY/CATCH, procedures,
RPCs and TDS transaction-manager requests, and their effects on table data,
identity and sequence values. `scripts/capture-savepoint.mjs` ran against the
pinned SQL Server 2025 image
`sha256:86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a`
(product version 17.0.4065.4, server collation `SQL_Latin1_General_CP1_CI_AS`).
Each run starts two containers and captures two fresh databases in each. All
four captures were identical after binding generated database names
(`<fresh-database>`), and the fixture keeps one of them. A later
independent check run (two more containers, four more databases) matched the
retained fixture exactly. The fixture SHA-256 is
`0e3d10e34afc9f3c58aee1ff70c5c6c84d6efc25c3f0ec529d264733172f98ea`.

The fixture holds values decoded by Tedious, TDS column descriptors,
diagnostics, DONE/DONEINPROC/DONEPROC tokens, return statuses and transaction
ENVCHANGE kinds. It does not hold raw packet bytes. Transaction descriptors are
recorded only by byte length (begin: new 8/old 0; commit and rollback: new
0/old 8), because the server allocates their values.

## Capture shape

Each program has its own table
`dbo.svp_<program>(id INT IDENTITY(1,1) CONSTRAINT pk_svp_<program> PRIMARY KEY, v INT NOT NULL CONSTRAINT ck_svp_<program>_v CHECK (v > 0))`.
After every step (except a data probe), a separate batch records
`@@TRANCOUNT` and `XACT_STATE()` (264 such probes). The 92 `data` probes also
return the table rows and `IDENT_CURRENT`. Every program ends with a recorded
cleanup that rolls back any open transaction and asserts `[[0, 0]]`.
Observation kinds are 623 SQL batches, 12 `sp_executesql` RPCs, 5
stored-procedure RPCs, 21 transaction-manager requests (packet type 14) and
one `sp_prepare`/`sp_execute`/`sp_unprepare` sequence with four executions.

The script follows `docs/reference-captures.md` from owner PR #312. That file
and its helpers are not on `main`, so the script replicates the fixed prepared
helper and the UTC setting instead of editing `scripts/lib`:

- `process.env.TZ = 'UTC'`. No captured case reads a clock.
- Each request clears the return status that Tedious carries on the connection
  (`procReturnStatusValue`) and records `returnStatus` only when a RETURNSTATUS
  token arrives during that request. This rule also applies to each prepared
  phase.
- Prepared completion is awaited via the `prepared`/`error` events.
  `request.error` is cleared before each phase, and only errors raised during a
  phase are attributed to it.
- Rows, messages, DONE tokens and ENVCHANGE entries are bounded per request.
  Comparisons use `isDeepStrictEqual`, `assertSameCapture` and
  `describeFirstDifference`, never whole-capture `node:assert`.
- Every batch and `sp_executesql` text carries a unique trailing
  `/*savepoint <program> <step>*/` comment. The prepared text is also unique.
  Constraints are named explicitly. There are no server `WHILE` loops.
- Containers are started with `withReferenceContainer` (labelled, removed by
  name). The script ran under `node --max-old-space-size=2048` with an RSS
  watchdog at 4 GB. Peak RSS was about 160 MB.
- `--write-fixture` checks for an existing fixture before any container starts.
  With the fixture present, it refused in about 0.14 s.

Transaction ENVCHANGE tokens are recorded by wrapping Tedious's
`RequestTokenHandler` begin/commit/rollback handlers for the life of the
script. Transaction-manager requests are sent through `connection.makeRequest`
with Tedious's own `Transaction` payload builders on a `Request` the script
owns, so their tokens are captured like any other request.

## Observed rules

These rules hold for the pinned build and the captured matrix only.

### Savepoints and ROLLBACK TO

- `SAVE TRANSACTION`/`SAVE TRAN name` in an open transaction returns one DONE
  with no row count and no ENVCHANGE. It leaves `@@TRANCOUNT` and `XACT_STATE()`
  unchanged, both at level 1 and at level 2.
- `ROLLBACK TRANSACTION savepoint` undoes the work done after the savepoint and
  keeps `@@TRANCOUNT` (including 2 inside a nested BEGIN). It emits no
  ENVCHANGE.
- **Rolling back to a savepoint consumes it.** Rolling back to the same name a
  second time fails with 6401/state 1/class 16
  (`Cannot roll back s1. No transaction or savepoint of that name was found.`).
  The transaction stays open and committable.
- With duplicate names, each rollback goes to the most recent remaining
  savepoint of that name. Two `SAVE TRANSACTION d` followed by three
  `ROLLBACK TRANSACTION d` undo to the second, then to the first, then fail
  with 6401.
- Rolling back to an earlier savepoint also removes later ones.
  `save a; save b; rollback a; rollback b` fails the last step with 6401.
- A savepoint taken inside a nested BEGIN survives the inner `COMMIT`. The
  caller can still roll back to it at level 1.
- Savepoints are transaction wide. Savepoints set by the caller, by a procedure,
  by `sp_executesql` or through the transaction manager can each be rolled back
  from any of the others.
- `SAVE TRANSACTION` with no active transaction fails with 628/state 0/class 16
  (`Cannot issue SAVE TRANSACTION when there is no active transaction.`) and
  aborts the rest of the batch: the preceding INSERT committed and the
  following INSERT and SELECT did not run. Inside TRY, 628 is caught
  (`ERROR_NUMBER()` 628, `XACT_STATE()` 0) and the batch continues after CATCH.
- `ROLLBACK TRANSACTION name` with no transaction fails with 3903/state 1.
  `COMMIT` with no transaction fails with 3902/state 1.
- `SAVE TRANSACTION` with no name is syntax error 102/state 1/class 15.

### Names

- A savepoint or transaction name longer than 32 characters in literal form is
  compile error 103/state 2/class 15
  (`The identifier that starts with '...' is too long. Maximum length is 32.`).
  This applies to `SAVE`, `ROLLBACK` and `BEGIN`. Exactly 32 characters is
  accepted.
- A name taken from a variable is truncated silently to 32 characters. This
  holds for NVARCHAR(64) with 40 characters, VARCHAR(40) with 33 characters and
  an `sp_executesql` NVARCHAR parameter with 40 characters. A later rollback
  with the 32-character prefix, or with the same longer value, reaches it.
- A non-character variable is error 3914/state 0/class 16 (`The data type "int"
  is invalid for transaction names or savepoint names. ...`).
- Trailing blanks do not matter. `CHAR(5) 'pad'` and a literal `[pad2   ]` are
  both reached by an unpadded rollback.
- An empty variable in `SAVE TRANSACTION @e` succeeds. `ROLLBACK TRANSACTION @e`
  with that empty value then fails with 6401/**state 2** and the message
  `Cannot roll back . No transaction or savepoint of that name was found.`
- A NULL variable in `SAVE TRANSACTION @z` succeeds silently (also through an
  `sp_executesql` NULL parameter). `ROLLBACK TRANSACTION @z` with NULL rolls
  back the **whole** transaction from level 2 to 0 and emits a rollback
  ENVCHANGE.
- **Savepoint names compare with the database collation.** In the CI database
  (`SQL_Latin1_General_CP1_CI_AS`), `ROLLBACK TRANSACTION casename` reaches
  `CaseName`, and `MixedCase` and `mixedcase` act as duplicates. The comparison
  is accent sensitive: `resume` does not reach `[résumé]`. After the database
  was altered to `Latin1_General_100_CS_AS` (server collation still CI),
  `casename` failed with 6401 and `CaseName` succeeded.
- **Transaction names compare case sensitively even in the CI database.**
  `BEGIN TRANSACTION CaseTx; ROLLBACK TRANSACTION casetx` fails with 6401.
  `ROLLBACK TRANSACTION CaseTx` rolls back everything.
- Bracket-quoted names with a space, double-quoted names (with
  `QUOTED_IDENTIFIER ON`) and non-ASCII names (`[żółw]`) all work.
- Only the outermost transaction name can be rolled back. `ROLLBACK TRANSACTION
  inner_tx` for a nested name fails with 6401 and leaves the count at 2.
  Rolling back the outer name from level 2 rolls back everything.
- When a savepoint has the same name as the outer transaction, the first
  `ROLLBACK TRANSACTION same_name` goes to the savepoint (count stays 1). The
  second rolls back the whole transaction.
- `COMMIT TRANSACTION unrelated_name` ignores the name and decrements the
  count.

### Nested BEGIN/COMMIT and completion tokens

- The outermost BEGIN emits a begin ENVCHANGE. A nested BEGIN, an inner
  COMMIT and SAVE emit none. The outermost COMMIT emits commit. Any full
  rollback emits rollback.
- `BEGIN; BEGIN; ROLLBACK` rolls back both levels, and a following `COMMIT`
  fails with 3902.
- In one batch, `@@TRANCOUNT` reads 1, 2, 2 (after SAVE), 1 and 0 across
  BEGIN, BEGIN, SAVE, COMMIT, COMMIT. One begin and one commit ENVCHANGE are
  emitted.

### Errors, XACT_ABORT and doomed transactions

- With `XACT_ABORT OFF`, a CHECK violation (547/state 0/class 16, followed by
  info 3621) fails only its statement. A two-row INSERT with one bad row
  inserts neither row but consumes two identity values. The transaction stays
  at `[1, 1]`. A savepoint taken before the error, or a new one taken after
  it, can be rolled back to. In a single batch, the batch continues past the
  547 to the ROLLBACK TO and the following SELECT.
- With `XACT_ABORT OFF`, an uncaught conversion error 245 outside TRY aborts
  the batch **and** rolls back the transaction (rollback ENVCHANGE, count 0).
  The next batch's `ROLLBACK TRANSACTION s3` fails with 3903.
- With `XACT_ABORT ON`, a CHECK violation emits 547 **without** the 3621 info
  message, rolls back the transaction (rollback ENVCHANGE) and leaves count 0.
- In CATCH, when `XACT_ABORT ON` has doomed the transaction (`XACT_STATE()`
  -1, count 1):
  - `ROLLBACK TRANSACTION s` fails with 3931/state 1/class 16 (`The current
    transaction cannot be committed and cannot be rolled back to a savepoint.
    Roll back the entire transaction.`). The batch then ends and the
    transaction is rolled back.
  - `SAVE TRANSACTION` or `COMMIT` fails with 3930/state 1 (`...cannot support
    operations that write to the log file. Roll back the transaction.`). The
    batch ends and the transaction is rolled back.
  - A full `ROLLBACK` followed by a new BEGIN, SAVE, ROLLBACK TO and COMMIT
    works in the same CATCH.
- With `XACT_ABORT OFF`, CATCH after a CHECK violation, RAISERROR(16) or
  divide by zero (8134) sees `XACT_STATE()` 1 and can roll back to a
  savepoint. CATCH after a conversion error 245 sees `XACT_STATE()` -1. The
  batch finishes and then emits 3998/state 1 (`Uncommittable transaction is
  detected at the end of the batch. The transaction is rolled back.`) with a
  rollback ENVCHANGE.
- `ROLLBACK TRANSACTION no_such_sp` inside TRY is caught as 6401, severity
  16, state 1, `XACT_STATE()` 1.
- In nested TRY blocks, the inner CATCH rolls back to `inner_sp` and uses
  `THROW`. The outer CATCH sees 547 with `XACT_STATE()` 1. Its own `ROLLBACK
  TRANSACTION inner_sp` fails with 6401 because the savepoint was consumed.

### Procedures and RPCs

- A procedure that saves and rolls back its own savepoint keeps the caller's
  count. It gets no 266. `RETURN 7` arrives as return status 7 both through
  `EXEC` in a batch and through a stored-procedure RPC.
- A savepoint left by a procedure can be rolled back by the caller. A procedure
  can roll back to a savepoint the caller set.
- A procedure that does `BEGIN; SAVE; ROLLBACK TO; COMMIT` returns 9 with no
  error.
- A procedure whose INSERT fails with 547 after its SAVE continues and returns
  11 with `XACT_STATE()` 1. The caller can roll back to the procedure's
  savepoint.
- A procedure that does `BEGIN; ROLLBACK` at caller level 1 returns 3. After
  the procedure exits, 266/state 2/class 16/line 0 is raised (`Transaction
  count after EXECUTE indicates a mismatching number of BEGIN and COMMIT
  statements. Previous count = 1, current count = 0.`), with a rollback
  ENVCHANGE. The batch continues to its SELECT, which reads count 0.
- A stored-procedure RPC with SAVE and no transaction raises 628 and ends with
  a single DONEPROC with **no** RETURNSTATUS (`returnStatus` null).
- An `sp_executesql` RPC returns status 0 on success and the error number on
  failure (6401 or 266). A net BEGIN, COMMIT or ROLLBACK inside
  `sp_executesql` raises 266 but keeps the changed count (1→2, 2→1, 2→0).
- `sp_prepare` of the savepoint text returns RETURNSTATUS 8182. The
  diagnostic run returned 0 for `SELECT` texts, and each phase clears the
  carried value first, so this status comes from `sp_prepare` itself. The
  four `sp_execute` phases return 0, 0, -6 (with 547 and 3621) and 0. The
  prepared rollback to `prep_b` consumed that savepoint, so a later batch
  `ROLLBACK TRANSACTION prep_b` fails with 6401. `ROLLBACK TRANSACTION
  prep_a` removes both surviving prepared inserts.

### Transaction-manager requests

- TM_SAVE_XACT (via the Tedious `Transaction` payload) behaves like SQL `SAVE`.
  TM_ROLLBACK_XACT with a savepoint name behaves like SQL ROLLBACK TO and
  emits no ENVCHANGE. Neither returns a RETURNSTATUS. SQL and TM savepoints
  share one namespace. TM name matching uses the database collation (CI:
  `MIXEDTM` reaches `MixedTm`).
- A TM save with no transaction returns 628/state 0. A TM rollback of an
  unknown or consumed name returns 6401/state 1.
- A 33-character TM savepoint name returns 103 with **state 30** (SQL batches
  report state 2). The TM path does not truncate.
- A TM save with an empty name returns 3977/state 1/class 16 (`The savepoint
  name cannot be NULL. The batch has been aborted.`) and rolls back the whole
  transaction (rollback ENVCHANGE, count 0). An empty variable in SQL `SAVE`
  succeeds.
- A nested TM begin (count 1→2) and a nested TM commit (2→1) emit no
  ENVCHANGE. A TM rollback with the outer transaction name, or with an empty
  name, rolls back everything.
- SQL `ROLLBACK TRANSACTION tm_named` reaches a transaction begun by TM with
  that name.

### Effects on data, identity and sequences

- Rollback to a savepoint removes rows but does not restore identity:
  `IDENT_CURRENT` keeps rising, and the next INSERT receives the next value
  (3 after 2 was rolled back). `SCOPE_IDENTITY()` and `@@IDENTITY` still report
  the rolled-back value 2.
- `NEXT VALUE FOR` is not rolled back. The next value after the savepoint
  rollback is 2, and `sys.sequences.current_value` reads 1 immediately after
  the rollback.
- A table variable keeps a row inserted after the savepoint. A local temporary
  table loses it. A table created after the savepoint no longer exists
  (`OBJECT_ID` NULL) after the rollback.
- `ALTER DATABASE CURRENT COLLATE` fails with 5075 (`The object
  'ck_svp_basic_v' is dependent on database collation...`) while CHECK
  constraints exist. For that reason the `cs` program first drops the earlier
  tables.

## Uncaptured gaps

- Distributed transactions, `BEGIN TRANSACTION ... WITH MARK`, `DELAYED_DURABILITY`,
  snapshot/serializable isolation and lock retention or release after ROLLBACK
  TO (including a second session observing locks or blocking).
- Triggers: SAVE/ROLLBACK TO inside AFTER/INSTEAD OF triggers and the trigger
  3609 rule. Cursors, `OUTPUT INTO`, MERGE and bulk load after ROLLBACK TO.
- Savepoints in user-defined functions (not allowed), in nested procedure
  levels beyond one, and `@@NESTLEVEL` interaction.
- `SET IMPLICIT_TRANSACTIONS ON`, `SET XACT_ABORT` changed inside procedures,
  and attention or cancellation during a savepoint rollback.
- Raw TDS bytes of the ENVCHANGE and DONE tokens. Transaction descriptor
  values. ENVCHANGE types other than 8/9/10 (such as 17 transaction ended),
  which Tedious does not expose. TM begin-after-commit/rollback restart
  flags and isolation bytes. A TM commit or rollback with no active
  transaction was not retained. An exploratory run, which is not retained,
  showed 3903 with state 2 for such a TM rollback.
- Case-sensitive **server** collation. Only the database collation was varied.
  Other collation properties (kana or width sensitivity, binary collations)
  were not varied.
- Savepoint behavior after errors of other severities (class 17+ resource
  errors, 1205 deadlock victim) and after compile errors inside TRY.

## Current msduck state

This task changed no Rust behavior, and msduck implements none of the rules
above. According to [transaction-manager.md](transaction-manager.md), nested
BEGIN counting, named outer rollback, 6401/3902/3903 and ENVCHANGE 8/9/10 exist
for SQL and TM requests. Declared savepoints and TM_SAVE_XACT fail explicitly.
[transaction-recovery.md](transaction-recovery.md) records that the pinned
DuckDB parser rejects `SAVEPOINT`, and that native runtime errors invalidate the
DuckDB transaction. No statement-level undo exists that could provide ROLLBACK
TO. The fixture has not been replayed against msduck.

## Proposed successors

Both need owner-published, disjoint task scopes.

1. **Deterministic core: transaction name and savepoint model.** Add a pure
   state machine (for example in `msduck-core`) over explicit inputs: the
   transaction count, the outer transaction name, a savepoint stack with
   duplicates, and a supplied name-comparison collation. It should cover:
   32-character literal limit (103, state 2 for SQL and 30 for TM); silent
   32-character truncation of variables and parameters; blank-insensitive
   comparison; savepoint names compared with the database collation;
   transaction names compared case sensitively; consume-on-rollback and
   duplicate-name order; empty and NULL name handling (6401 state 2, full
   rollback on NULL, 3977 on TM empty); outer-only transaction names; COMMIT
   ignoring names; 628/3902/3903 selection; and the 3930/3931 rules for doomed
   transactions. It should return typed outcomes (new count, removed
   savepoints, full rollback, ENVCHANGE kind, diagnostic number, state and
   class) without any backend access. Unit tests should replay this fixture's
   name/count/error sequences without DuckDB.
2. **Root: physical savepoint execution and wire replay.** Add a transaction
   adapter that can undo work back to a marker without invalidating the
   DuckDB transaction. This is step 5 of
   [transaction-recovery.md](transaction-recovery.md) and needs a proven
   backend facility, not SQL `SAVEPOINT` text. It must preserve identity and
   sequence consumption, table-variable persistence, and temp-table and DDL
   rollback. The session layer should apply the core model to SQL batches,
   procedures, `sp_executesql`, prepared execution and TM_SAVE_XACT/
   TM_ROLLBACK_XACT. It should emit the observed DONE, RETURNSTATUS, 266 and
   ENVCHANGE streams, including null RETURNSTATUS on a failed procedure RPC,
   `sp_executesql` statuses and sp_prepare 8182. Then replay
   `reference/savepoint.json` through a Tedious client test. That test must
   retain every raw difference until rows, descriptors, diagnostics,
   completion tokens and next-request state match.
