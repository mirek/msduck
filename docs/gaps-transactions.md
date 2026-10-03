# Isolation levels, savepoints and WAITFOR

Issue #727 (task `gaps-transactions-v1`), with `ALLOW_SNAPSHOT_ISOLATION`
from issue #870 (task `v025-snapshot-isolation-v1`). The feature lives in the
`transactions` extension modules (see [extension-hooks.md](extension-hooks.md)):

- `crates/msduck-sql/src/dialect/ext/transactions.rs` parses `SAVE
  TRAN[SACTION]`, named `BEGIN`/`COMMIT`/`ROLLBACK TRAN[SACTION]`, `WAITFOR
  DELAY|TIME` and `DBCC USEROPTIONS`, and validates WAITFOR time strings.
- `src/engine/ext/transactions.rs` and its `options`, `savepoints`,
  `snapshot` and `waitfor` submodules run them.
- `crates/msduck-sql/src/dialect/alter_database.rs` parses `ALTER DATABASE
  ... SET ALLOW_SNAPSHOT_ISOLATION {ON | OFF}`, and `src/database_catalog.rs`
  stores the option and publishes it in `sys.databases`.
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
  the engine's wording of 137 for an undeclared variable, and `SQL_VARIANT`
  variables.
- The fixture's separate `snapshot` section was captured by
  `scripts/capture-gaps-transactions.mjs --snapshot` from the pinned
  `mcr.microsoft.com/mssql/server:2025-latest@sha256:86cc6144…` image (two
  fresh databases, identical results). Its 107 observations use four
  connections (`a`, `b` and `c` in the fresh database, `m` in master): the
  `ALLOW_SNAPSHOT_ISOLATION` forms and errors, `sys.databases` states, 3952
  for data access while the option is OFF, SNAPSHOT reads and 3960 write
  conflicts against a concurrent writer, and changes that wait for open
  transactions (an ALTER sent in the background, whether it was still
  running 1.5 seconds later, and 3956/3954 meanwhile).
  `tests/compat/snapshot_isolation.test.mjs` replays it (rows, column
  descriptors, errors, messages and the waits) and lists the remaining
  differences exactly.
- `tests/gaps_transactions.rs` covers the same rules in process, plus
  savepoint restores, cancellation and termination, and the option's
  persistence across a restart.

## Isolation levels

`SET TRANSACTION ISOLATION LEVEL` accepts `READ UNCOMMITTED`, `READ
COMMITTED`, `REPEATABLE READ`, `SNAPSHOT` and `SERIALIZABLE`. A
transaction-manager begin request accepts levels 0 (keep the current level)
and 1-5. As captured:

- The level is a session setting. It persists after the transaction ends, a
  transaction-manager begin changes it too (also for a nested begin), and a
  level set inside a transaction stays after COMMIT.
- A level set inside an RPC (`sp_executesql`, prepared execution), a
  procedure or dynamic SQL (`EXEC`, `EXEC sp_executesql`) reverts when it
  returns.
- `sys.dm_exec_sessions.transaction_isolation_level` reports 1-5 (2 for a
  new session) as a non-nullable `smallint`, in SQL Server's column position
  (after `is_user_process`).
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
| SNAPSHOT (5) | Transaction snapshot from the first data access; update conflicts fail with 3960; requires `ALLOW_SNAPSHOT_ISOLATION` (3952) | Transaction snapshot from `BEGIN TRANSACTION`; the conflicts DuckDB detects fail with 3960; requires `ALLOW_SNAPSHOT_ISOLATION` (3952). See below |

## ALLOW_SNAPSHOT_ISOLATION and SNAPSHOT transactions

`ALTER DATABASE {name | CURRENT} SET ALLOW_SNAPSHOT_ISOLATION {ON | OFF}`
stores the option per database; it persists across restarts and appears as
`sys.databases.snapshot_isolation_state` (`tinyint`: 0 OFF, 1 ON, 2
IN_TRANSITION_TO_OFF, 3 IN_TRANSITION_TO_ON) and
`snapshot_isolation_state_desc` (`nvarchar(60)`) in SQL Server's column
order, before `is_read_committed_snapshot_on`. As captured:

- New databases start OFF. `master` is always ON: setting the option there
  succeeds with informational message 3987 ("SNAPSHOT ISOLATION is always
  enabled in this database.") and changes nothing.
- Other sessions may stay connected; no termination clause is needed. The
  statement completes with no row count and leaves `@@ROWCOUNT` 0. It works
  inside `sp_executesql`.
- A change waits for other sessions' open transactions: ON for those that
  wrote to the database, OFF also for SNAPSHOT transactions that read it.
  Read-only transactions at other levels, and SNAPSHOT transactions that
  have not accessed data yet, do not delay it. Meanwhile the state is 3 or 2, and a SNAPSHOT
  transaction that accesses the database fails with 3956 ("…because the
  ALTER DATABASE command which enables snapshot isolation for this database
  has not finished yet…") or 3954 ("…because the ALTER DATABASE command that
  disallows snapshot isolation had started before this transaction
  began…"), with the effects of 3952. A SNAPSHOT transaction that already
  read the database continues during the change to OFF. An attention
  cancels the wait and restores the previous state; a server stopped during
  the wait also restores it.
- Errors, in SQL Server's order: 226 inside a transaction; 12104 for
  `CURRENT` in master; 5011 for a missing database; 5082 ("Cannot change the
  versioning state on database "…" together with another database state.")
  when combined with another option such as `READ_COMMITTED_SNAPSHOT` or
  `MULTI_USER`; 5083 for any `WITH ROLLBACK …` or `WITH NO_WAIT` clause.
  5011, 5082 and 5083 are followed by 5069 ("ALTER DATABASE statement
  failed."), which `ERROR_NUMBER()` reports in CATCH. `ON` together with
  `OFF` in one statement is 5062, raised for the whole batch before any
  statement runs; repeating the same value is accepted. A missing `ON`/`OFF`
  is 102.
- With the session at SNAPSHOT, reading or writing (SELECT, INSERT, UPDATE,
  DELETE, MERGE) a table or view of a database whose option is OFF fails
  with 3952 ("Snapshot isolation transaction failed accessing database '…'
  because snapshot isolation is not allowed in this database. Use ALTER
  DATABASE to allow snapshot isolation."), naming the accessed database,
  also through a three-part name. It ends the batch and rolls back an open
  transaction; inside TRY it dooms the transaction (`XACT_STATE()` -1, then
  3998 at the end of the batch). Statements without table access, catalog
  views, temporary tables, table variables, common table expressions and
  DDL are allowed. A missing table still fails with 208.
- With the option ON, a SNAPSHOT transaction keeps reading the rows of its
  snapshot after another connection commits an update, and sees them after
  it commits. Updating or deleting a row that a transaction committed after
  the snapshot began fails with 3960 (state 2, naming `schema.table` and the
  database), which ends the batch and rolls the transaction back, or dooms
  it inside TRY. Writes to other rows commit normally.

Remaining differences, retained exactly in the replay:

- SQL Server takes the snapshot at the transaction's first data access;
  DuckDB takes it at `BEGIN TRANSACTION`, so a commit made between the two
  is not visible to msduck.
- DuckDB detects a conflict when both transactions update, or both delete,
  the same row. A DELETE of a row another transaction updated (or an UPDATE
  of a row it deleted) succeeds in msduck instead of failing with 3960.
- SQL Server sends a failing SELECT's column metadata before 3952; msduck
  checks access first and sends no empty result set.
- 3952 is checked before name resolution for tables and views that exist,
  in queries, writes and the subqueries of SET and DECLARE. Data access in
  IF and WHILE conditions, in a procedure's RETURN expression (which the
  procedures feature evaluates itself), in scalar function calls, or in a
  module that runs through another path is not checked.
- 226 ends the batch in msduck, as it does for the engine's other ALTER
  DATABASE options; SQL Server continues with the next statement. A missing
  `ON`/`OFF` reports state 1 instead of 6, and a missing table reports
  DuckDB's 208 message.
- Waiting changes find the transactions to wait for from the statements
  each one ran: the database current at `BEGIN TRANSACTION`, the tables it
  accessed, and the targets of its writes and DDL (temporary objects
  excluded). Any statement msduck does not classify as read-only counts
  as a write to the current database. Only transactions that began before
  the change are waited for, and changes of one database run one at a
  time. An autocommit statement counts until the next statement of its
  batch or body starts, so a change may wait slightly longer than SQL
  Server. Likewise, a SNAPSHOT statement that fails because a temporary
  table it names is missing still counts as a read of the other tables it
  names, until its transaction ends.
- After a write conflict DuckDB aborts its transaction, so msduck restarts
  an empty one for the doomed transaction's remaining reads until it is
  rolled back.
- `sys.databases` lists only `master` and user databases; SQL Server's
  `msdb` (ON), `tempdb` and `model` (OFF) are not published.

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
