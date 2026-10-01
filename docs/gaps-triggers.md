# DML triggers

msduck supports AFTER (FOR) and INSTEAD OF triggers for INSERT, UPDATE and
DELETE on tables:

```sql
CREATE [OR ALTER] TRIGGER [schema.]name ON [schema.]table
  [WITH option [, ...]]
  { FOR | AFTER | INSTEAD OF } { [INSERT] [,] [UPDATE] [,] [DELETE] }
  [WITH APPEND] [NOT FOR REPLICATION]
AS body

ALTER TRIGGER ...                      -- same form
DROP TRIGGER [IF EXISTS] [schema.]name [, ...]
{ DISABLE | ENABLE } TRIGGER { ALL | name [, ...] } ON { table | DATABASE | ALL SERVER }
ALTER TABLE table { DISABLE | ENABLE } TRIGGER { ALL | name [, ...] }
```

Expected behavior comes from `reference/gaps-triggers.json`, captured from the
pinned SQL Server 2025 image with `scripts/capture-gaps-triggers.mjs`.
`tests/compat/triggers.test.mjs` replays every captured batch against msduck
through tedious and compares result rows and ERROR tokens (number, state,
class and message); the three known differences are listed below.
`tests/gaps_triggers.rs` covers the same behavior through the engine,
including concurrent sessions and a restart.

The implementation lives in the triggers extension (`src/engine/ext/triggers`
and `crates/msduck-sql/src/dialect/ext/triggers.rs`), using the hooks in
[extension-hooks.md](extension-hooks.md).

## Definitions

- A trigger definition must be the first statement of its batch and owns the
  rest of it. A later `CREATE`/`ALTER TRIGGER` in a batch fails with 111
  (state 6, class 15) before anything in the batch runs.
- The body is ordinary batch text: optional `;` terminators, `BEGIN...END`,
  control flow, TRY/CATCH, variables, RAISERROR, THROW, RETURN, transaction
  statements, cursors and nested DML. It is parsed when the trigger is
  created; a syntax error fails the definition (102 and the parser's message).
  Names are resolved when the trigger runs, so a body may refer to tables
  that do not exist yet.
- Definitions live in the module store (`main.__msduck_modules`) as type `TR`,
  with `parent_object_id` set to the table's object id. The properties JSON
  records the events and the timing:
  `{"events":["INSERT","UPDATE","DELETE"],"instead_of":false}` (events in
  this order). Triggers appear in `sys.objects` (type `TR`, `SQL_TRIGGER`)
  and take object ids from the table sequence. `sys.triggers`,
  `sys.sql_modules` and `OBJECTPROPERTY` belong to the catalog task.
- The stored definition is the batch text. As in SQL Server, `ALTER TRIGGER`
  is stored as `CREATE TRIGGER` and `CREATE OR ALTER` drops `OR ALTER`
  (`CREATE   TRIGGER`). ALTER keeps the object id and the disabled flag.
- Completion: CREATE and ALTER send DONE CurCmd 221, DROP TRIGGER 225,
  DISABLE/ENABLE TRIGGER 253 and ALTER TABLE ... TRIGGER 216.
- Errors follow the capture:
  - target missing, or a view for an AFTER trigger: 8197 (state 4, state 6
    for a view);
  - trigger name in use by any object in the schema: 2714 (state 2);
  - schema of the trigger name differs from the table's: 2103 (class 15);
  - duplicate event: 1034 (class 15);
  - second INSTEAD OF trigger for an event: 2111;
  - ALTER of a missing trigger: 208 (state 6); ALTER naming another table:
    2110 (class 15);
  - database prefix on the trigger name: 166;
  - empty body: 102 "Incorrect syntax near 'AS'.".
- DROP TRIGGER drops every name it finds and reports 3701 (state 5, class 11)
  for each missing one; `IF EXISTS` skips missing names.
- DISABLE/ENABLE: a missing table is 1088 (state 21), a trigger not on that
  table 1088 (state 119). With ALTER TABLE a missing table is 4902 and a
  missing trigger 4920. `ALL ON DATABASE` and `ALL ON ALL SERVER` succeed,
  since there are no DDL triggers.
- DROP TABLE drops the table's triggers in the same transaction.

## Firing

- Triggers fire once per statement, for every row the statement affects,
  including none: a zero-row UPDATE still fires its UPDATE triggers.
- AFTER triggers on one table fire in creation order.
- `inserted` and `deleted` hold the statement's rows: new rows for INSERT,
  old rows for DELETE, both for UPDATE. They bind with the table's declared
  column types, so string, Unicode, money and date/time expressions over them
  behave as over the table. Columns include computed columns.
- `@@ROWCOUNT` is the statement's row count when the body starts, and again
  after the statement completes, whatever the body did. `@@TRANCOUNT` inside
  a body is one more than outside (2 for an autocommit statement).
- `UPDATE(column)` is true for every column of an INSERT, for the assigned
  columns of an UPDATE and for no column of a DELETE. `COLUMNS_UPDATED()`
  returns one bit per column id (least significant bit first); for DELETE it
  is empty. `TRIGGER_NESTLEVEL()` and `TRIGGER_NESTLEVEL(object_id)` report the
  trigger nesting, and `@@NESTLEVEL` in a trigger body its level.
- Statements in a body produce DONEINPROC completions and their result sets
  go to the client, before the triggering statement's DONE. `SET` options
  changed in a body, such as NOCOUNT, are restored when it ends.
- Nested triggers fire (a trigger's statements fire other tables' triggers).
  A trigger is not fired again by its own statements (RECURSIVE_TRIGGERS
  OFF), and an INSTEAD OF trigger's statements on its table run normally and
  fire the table's AFTER triggers. Nesting deeper than 32 fails with 217 and
  ends the batch.
- `OUTPUT` without `INTO` on a statement that would fire an enabled trigger
  fails with 334. `OUTPUT ... INTO` works.
- TRUNCATE TABLE does not fire triggers.

### INSTEAD OF

The statement does not change the table. The trigger receives the rows the
statement would have written:

- INSERT: rows as the table would store them, with defaults applied and
  identity columns 0;
- UPDATE: `deleted` holds the selected rows and `inserted` the same rows with
  the assignments applied;
- DELETE: `deleted` holds the selected rows.

The statement's row count (DONE and `@@ROWCOUNT`) is the number of those rows.

### Errors and ROLLBACK

A statement on a table with triggers runs in a transaction: the caller's, or
one opened for the statement in autocommit mode.

- An error inside a trigger (THROW, a failed statement, an arithmetic error,
  a nested trigger's error) stops the trigger and ends the batch, as with
  XACT_ABORT. The triggering statement is undone with the transaction. Inside
  TRY, control passes to CATCH; an explicit transaction is then uncommittable
  (`XACT_STATE() = -1`) until CATCH rolls it back, and in autocommit mode the
  statement is already undone.
- RAISERROR does not stop a trigger. The usual pattern
  `RAISERROR(...); ROLLBACK TRANSACTION; RETURN` rolls back, lets the trigger
  finish, and then ends the batch with 3609 ("The transaction ended in the
  trigger. The batch has been aborted."). After the rollback `inserted` and
  `deleted` are empty, and the trigger's later writes are kept, as in SQL
  Server. An explicit transaction's rollback sends the transaction ENVCHANGE;
  an autocommit statement's does not.
- COMMIT in a trigger commits the transaction and ends the batch with 3609.

## Implementation notes

- A statement on a table with enabled triggers for its event runs through
  the triggers extension; statements on other tables cost one lookup in the
  module store.
- INSERT images are the rows with `rowid` greater than the table's largest
  `rowid` before the statement. UPDATE and DELETE first run their FROM, WHERE
  and WITH through the engine to capture the row ids they select, then copy
  the stored rows. DuckDB rewrites rows whose key columns change with new,
  larger row ids, which the UPDATE image also takes.
- Images are tables in the database's `main` schema named
  `__msduck_trigger_<table id>_<session>_<sequence>_{i,d}`, created in the
  statement's transaction and dropped when the firing ends. `main` is not a
  user schema, so they never appear in `sys.objects`. Inside a body
  `inserted`/`deleted` are rewritten to these names (keeping the name as
  alias). Catalog binding resolves names with `__msduck_object_id`; at
  bootstrap the triggers extension extends that macro's lookup map with
  `#<object id>` keys for user tables and rewrites an image name's key to
  `#<table id>`, so images bind with the triggering table's declarations.
  The macro still evaluates its argument once and costs the same.
- INSTEAD OF INSERT runs the statement against an empty copy of the table
  (same declarations and defaults, no constraints, identity default 0);
  INSTEAD OF UPDATE runs it against a copy of the selected rows.
- `UPDATE(column)` and `@@NESTLEVEL` are replaced in the body text before it
  is parsed, so batch validation sees an ordinary predicate and number.

## Remaining limits

- RAISERROR in a trigger fired inside TRY: SQL Server passes control to CATCH
  at the RAISERROR. msduck cannot see the caller's TRY, so the trigger
  continues (typically to ROLLBACK) and CATCH receives 3609 instead of the
  RAISERROR, which is also sent to the client. The data outcome is the same.
- "The statement has been terminated." (INFO 3621) is not sent after an error
  inside a trigger, and the final DONE carries CurCmd 0 instead of 253.
- Duplicate-key errors inside triggers carry the engine's message and class
  (`tests/compat/triggers.test.mjs` compares only their number).
- `@@TRANCOUNT` after COMMIT or ROLLBACK inside a trigger reads 1 or 0;
  SQL Server keeps reporting 2. BEGIN TRANSACTION left open by a trigger
  keeps the transaction open, but the count after nested user transactions
  can differ from SQL Server's.
- MERGE does not fire triggers yet; a MERGE whose actions match an enabled
  trigger on its target fails explicitly.
- Triggers on views (INSTEAD OF), DDL triggers (`ON DATABASE`, `ON ALL
  SERVER`), logon triggers, `sp_settriggerorder`, the RECURSIVE_TRIGGERS and
  nested-triggers options, `WITH ENCRYPTION`/`EXECUTE AS` semantics and
  `FIRE_TRIGGERS` for bulk loads are not implemented. Creating a trigger on a
  view or a DDL trigger fails explicitly.
- Statements whose target is reached only through a CTE or a view do not
  fire the base table's triggers, and statements on a table in another
  database (three-part names) do not fire its triggers.
- DML executed by `INSERT ... EXEC` inside a triggering statement does not
  fire triggers.
- An error inside a procedure called from a trigger body follows the EXEC
  path, which ends only that statement, instead of ending the trigger.
- SCOPE_IDENTITY() scoping across trigger bodies belongs to the
  rowversion_identity task; `State::depth()` reports the trigger nesting.
- `FOR JSON AUTO` in trigger bodies depends on the json_string task.
- Engine parser limits apply inside bodies as in any batch: `FETCH` inside a
  `WHILE ... BEGIN ... END` block does not parse, and `COMMIT` or `ROLLBACK`
  directly after a `SELECT` without a terminator is read as a column alias.
