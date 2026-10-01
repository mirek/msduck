# Isolation levels, savepoints and WAITFOR

Issue #727 (task `gaps-transactions-v1`). The feature lives in the
`transactions` extension modules (see [extension-hooks.md](extension-hooks.md)):

- `crates/msduck-sql/src/dialect/ext/transactions.rs` parses `SAVE
  TRAN[SACTION]`, named `BEGIN`/`COMMIT`/`ROLLBACK TRAN[SACTION]`, `WAITFOR
  DELAY|TIME` and `DBCC USEROPTIONS`, and validates WAITFOR time strings.
- `src/engine/ext/transactions.rs` and its `options`, `savepoints` and
  `waitfor` submodules run them.
- `src/sessions.rs` reports `transaction_isolation_level` in
  `sys.dm_exec_sessions` and lets a session's waits be cancelled or ended.

## Evidence

- `reference/savepoint.json` (SQL Server 2025, 17.0.4065.4) holds the
  savepoint and named-transaction rules; [savepoint.md](savepoint.md)
  summarizes them.
- `reference/gaps-transactions.json` was captured for this task by
  `scripts/capture-gaps-transactions.mjs` from
  `mcr.microsoft.com/mssql/server:2022-latest` (16.0.4236.2,
  `SQL_Latin1_General_CP1_CI_AS`). Two fresh databases gave identical
  results. It holds 123 observations: isolation levels set through SQL and
  through transaction-manager begin requests, `sys.dm_exec_sessions`, `DBCC
  USEROPTIONS` (including `READ_COMMITTED_SNAPSHOT`), `sp_executesql`
  scoping, and the WAITFOR literal, variable and error matrix. Waits are
  recorded as booleans (at least the requested time, and under three seconds
  more), so that captures compare exactly.
- `tests/compat/transactions.test.mjs` replays that fixture through tedious
  and compares rows, errors (number, state, class and message) and the wait
  booleans. Differences owned by other features are listed there exactly:
  `EXEC sp_executesql` inside a SQL batch, the engine's wording of 137 for an
  undeclared variable, and `SQL_VARIANT` variables.
- `tests/gaps_transactions.rs` covers the same rules in process, plus
  savepoint restores, cancellation and termination.

## Isolation levels

`SET TRANSACTION ISOLATION LEVEL` accepts `READ UNCOMMITTED`, `READ
COMMITTED`, `REPEATABLE READ`, `SNAPSHOT` and `SERIALIZABLE`. A
transaction-manager begin request accepts levels 0 (keep the current level)
and 1-5. As captured:

- The level is a session setting. It persists after the transaction ends, a
  transaction-manager begin changes it too (also for a nested begin), and a
  level set inside a transaction stays after COMMIT.
- A level set inside an RPC (`sp_executesql`, prepared execution) reverts
  when the RPC returns.
- `sys.dm_exec_sessions.transaction_isolation_level` reports 1-5 (2 for a
  new session). The column is last, after the columns msduck already
  provided, and its descriptor is a nullable `smallint` because the catalog
  descriptors in `src/query_catalog.rs` belong to another task. SQL Server
  sends a non-nullable `smallint`.
- `DBCC USEROPTIONS [WITH NO_INFOMSGS]` returns SQL Server's rows in its
  order: `textsize`, `language`, `dateformat`, `datefirst`, `lock_timeout`,
  `quoted_identifier`, `arithabort`, `nocount` (when ON),
  `ansi_null_dflt_on`, `xact_abort` (when ON), `ansi_warnings` (when ON),
  `ansi_padding`, `ansi_nulls`, `concat_null_yields_null` and `isolation
  level`. The level reads `read committed snapshot` in a database with
  `READ_COMMITTED_SNAPSHOT ON`. Columns are `Set Option NVARCHAR(128)` and
  `Value NVARCHAR(46)`. Message 2528 follows unless `NO_INFOMSGS` is given.
  SQL Server ends the rows with their own DONE and then sends 2528 and a
  final DONE; msduck sends 2528 before the statement's single DONE.

### What each level means in msduck

DuckDB has one isolation level: snapshot isolation with optimistic
concurrency. A transaction reads the snapshot taken when it started, and
a write that conflicts with a concurrent committed write fails. msduck takes
no locks, so no level blocks or is blocked, and lock hints and `lock_timeout`
have no effect.

| Level | SQL Server | msduck |
| --- | --- | --- |
| READ UNCOMMITTED (1) | Dirty reads | Snapshot reads; never dirty |
| READ COMMITTED (2) | Each statement sees data committed before it; with `READ_COMMITTED_SNAPSHOT` a statement-level snapshot | Inside an explicit transaction, every statement sees the transaction's snapshot, not commits made after it started. Outside one, each statement is its own transaction |
| REPEATABLE READ (3) | Shared locks held until commit; phantoms possible | Repeatable reads and no phantoms from the snapshot; no locks |
| SERIALIZABLE (4) | Range locks; serializable | Snapshot isolation only: **not serializable**. Write skew is possible, and nothing blocks |
| SNAPSHOT (5) | Transaction snapshot; update conflicts fail with 3960 | Transaction snapshot; conflicts fail with DuckDB's conflict error, not 3960. msduck does not require `ALLOW_SNAPSHOT_ISOLATION` (no 3952) |

## Savepoints

`SAVE TRAN[SACTION] name|@variable`, `ROLLBACK TRAN[SACTION] name|@variable`
and transaction-manager save and rollback requests follow
`reference/savepoint.json`:

- SAVE leaves `@@TRANCOUNT` and `XACT_STATE()` unchanged and emits no
  ENVCHANGE. ROLLBACK to a savepoint undoes only the work done after it and
  keeps `@@TRANCOUNT`, also at nesting level 2.
- Rolling back to a savepoint consumes it and every later savepoint; with
  duplicate names, each rollback reaches the newest remaining one. A
  savepoint taken inside a nested BEGIN survives the inner COMMIT. SQL and
  transaction-manager savepoints share one namespace.
- Savepoint names compare with the database collation (case insensitive,
  accent sensitive) and ignore trailing blanks. Transaction names compare
  case sensitively. When a savepoint has the outer transaction's name, the
  first ROLLBACK reaches the savepoint and the second ends the transaction.
- Errors: no transaction, 628/0 (ends the batch; catchable); unknown or
  consumed name, 6401/1; an empty variable, 6401/2 with `Cannot roll back .`;
  a literal name over 32 characters, 103/2 while compiling (state 30 through
  the transaction manager, which does not truncate); a non-character
  variable, 3914; an empty transaction-manager name, 3977, which also rolls
  back the whole transaction. A variable name is truncated to 32
  characters. A NULL variable saves nothing, and ROLLBACK to a NULL name
  ends the whole transaction. In a doomed transaction, SAVE is 3930 and
  ROLLBACK to a savepoint 3931.
- Identity and sequence values are not rolled back.

### How the rollback works

DuckDB has no savepoints and no statement-level undo, so msduck keeps
before-images. The first time a statement after the newest savepoint writes
a table (INSERT, UPDATE, DELETE, MERGE, TRUNCATE TABLE or an OUTPUT INTO
target), msduck copies the table's rows into a temporary table that belongs
to that savepoint. Rolling back to a savepoint restores every table from its
earliest copy taken at or after that savepoint, changing only rows that
differ:

- With a primary key, rows are matched by key. Rows added later are
  deleted, changed rows are updated back, and removed rows are reinserted.
- Without a primary key, rows are compared as a multiset of whole rows
  (their JSON form, so a case-only change is restored).

Deletes run in reverse order of first write and inserts in order, so
foreign keys among restored tables stay satisfied. Computed columns are not
written. Copies are dropped when the savepoint is consumed or the
transaction ends.

### Limits

- The first write to a table after a savepoint copies the whole table, so
  its cost grows with the table, not with the change.
- Statements whose effects cannot be undone without rolling back the whole
  DuckDB transaction fail explicitly (40515, catchable) while a savepoint
  exists: CREATE, ALTER and DROP of any object (including `#temp` tables),
  SELECT INTO, and writes through views. Writes to existing `#temp` tables
  are restored like permanent tables; table variables keep their rows, as in
  SQL Server. SQL Server rolls these back to the
  savepoint. Without a savepoint they behave as before.
- Tables that reference a written table through a foreign key with a
  referential action (CASCADE, SET NULL, SET DEFAULT) are copied too,
  transitively, and triggers' writes go through the statement path. Other
  writes a feature performs directly on DuckDB are not copied first; a table
  first written that way after a savepoint is not restored.
- A native error that invalidates the DuckDB transaction (see
  [transaction-recovery.md](transaction-recovery.md)) cannot be recovered by
  rolling back to a savepoint. If a restore itself fails, the transaction is
  doomed and must be rolled back.
- A restore rewrites the rows that changed. Another session's concurrent
  write to the same rows can make it fail with DuckDB's conflict error.

## Named transactions

`BEGIN TRAN[SACTION] name|@variable [WITH MARK ['description']]` names the
outermost transaction. A nested name is ignored, `COMMIT TRAN name` ignores
the name, and only the outermost name can be rolled back (6401 otherwise).
`WITH MARK` is accepted and has no effect, because msduck has no log
restores. A reserved keyword after `BEGIN TRAN` starts the next statement.

## WAITFOR

`WAITFOR DELAY` waits for an interval and `WAITFOR TIME` until the next
occurrence of a time of day, in the clock that `GETDATE()` reads. As
captured:

- A literal (`'...'` or `N'...'`) is validated while the batch compiles.
  Accepted forms are `h[h]:m[m]`, `h[h]:m[m]:s[s]`, an optional fraction of
  one to three digits after `.` or milliseconds after `:`, an optional ` AM`
  or ` PM`, surrounding blanks, and the empty string (zero). Anything else,
  including a date part, hour 24, minute 60 or four fraction digits, is
  148/1/15 (`Incorrect time syntax in time string '...' used with
  WAITFOR.`), and nothing in the batch runs.
- A non-MAX character variable is parsed like a literal when the statement
  runs; an invalid value, and any `VARCHAR(MAX)` or `NVARCHAR(MAX)` value, is
  241/1/16. A `datetime` variable contributes its time of day. An `int` or
  `smallint` variable is a number of seconds (seconds after midnight for
  TIME); a negative value waits until cancelled. Other types are 9815/0/16
  (`Waitfor delay and waitfor time cannot be of type <type>.`). A NULL
  variable of any type does not wait.
- Other argument forms are syntax error 102 with SQL Server's message.
  `WAITFOR (RECEIVE ...)` fails explicitly: msduck has no Service Broker.
- WAITFOR keeps an open transaction open and leaves `@@ROWCOUNT` at 0.

### Cancellation

A wait checks every 10 ms for cancellation and ends early when:

- the request's Attention flag is set (the engine's cancellable request path,
  `Session::batch_response_with_read_cancel`). The request then completes
  like a cancelled read: no later statement or CATCH block runs, and with
  `XACT_ABORT ON` the transaction is rolled back;
- `Registration::cancel` is called for the session (`src/sessions.rs`). The
  cancellation lasts until the next request starts;
- the session is terminated, for example by `ALTER DATABASE ... SET
  SINGLE_USER WITH ROLLBACK IMMEDIATE` from another session.

The network server (`src/server.rs`) still reads a client's Attention only
after the running request has returned (see
[attention-design.md](attention-design.md)). Until that transport work lands,
a client that cancels a WAITFOR receives its cancellation acknowledgement
when the wait ends; the connection stays usable.
