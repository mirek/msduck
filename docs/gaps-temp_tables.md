# #temp tables and table variables

msduck supports local (`#t`) and global (`##t`) temporary tables and table
variables (`DECLARE @t TABLE (...)`). The feature lives in
`src/engine/ext/temp_tables.rs` (runtime) and
`crates/msduck-sql/src/dialect/ext/temp_tables.rs` (syntax). It uses the
extension hooks described in [extension-hooks.md](extension-hooks.md),
including the `batch_begin` and `batch_end` hooks added for it.

## What works

- `CREATE TABLE #t` and `##t`, `SELECT ... INTO #t`, and `INSERT`,
  `UPDATE`, `DELETE`, `SELECT`, `TRUNCATE TABLE`, `ALTER TABLE`,
  `CREATE INDEX` and `DROP TABLE [IF EXISTS]` on them. Several names in one
  `DROP TABLE` are dropped one after another.
- Names written as `#t`, `dbo.#t`, `tempdb..#t` and `tempdb.dbo.#t`, and
  column qualifiers such as `#t.id`. `OBJECT_ID('tempdb..#t')` (with or
  without a type) returns the table's object ID. Like SQL Server,
  `OBJECT_ID('#t')` without `tempdb` returns NULL.
- `tempdb.sys.*` and `tempdb.INFORMATION_SCHEMA.*` read the current
  database's catalog, which includes the temporary tables (see the limits
  below). `SELECT name FROM tempdb.sys.columns WHERE object_id =
  OBJECT_ID('tempdb..#t')` lists a temporary table's columns.
- Constraints and properties behave as on permanent tables: `PRIMARY KEY`,
  `UNIQUE`, `CHECK`, `DEFAULT`, `NOT NULL`, `IDENTITY` and indexes, with the
  same error numbers (2627, 547, 515). Result columns have the declared
  types.
- `DECLARE @t [AS] TABLE (...)` with the same column and constraint syntax,
  `INSERT`, `SELECT`, `UPDATE` (also `UPDATE alias ... FROM @t alias`),
  `DELETE`, joins with tables and other temporary objects, `OUTPUT` and
  `OUTPUT ... INTO @t`, and references in `IF`, `WHILE` and `SET`
  subqueries.

## Scope and lifetime

Every batch (a SQL batch, an RPC request, or a nested body such as a
procedure) opens a scope that ends with the batch.

- A local temporary table belongs to the session when a top-level SQL batch
  creates it, and lasts until it is dropped, the session ends, or the
  connection is reset. When an RPC request (`sp_executesql`, `sp_execute`,
  tedious `execSql`) or a nested body creates one, it is dropped when that
  request or body ends. Nested bodies see the tables of their callers and
  may create their own table with the same name, which hides the caller's
  until they end.
- Local temporary tables are private to their session: another session
  gets error 208 and can create its own table with the same name.
- A global temporary table is visible to every session in the database
  and is dropped when the session that created it ends (unless another
  session dropped and recreated it meanwhile).
- A table variable belongs to the batch or nested body that declares it.
  Neither later batches nor nested bodies can see it (error 1087). As in
  SQL Server, a declaration in a branch that does not run still declares
  the variable, and executing a declaration again in a loop keeps its rows.
- A batch cannot create the same temporary table twice, even in exclusive
  branches (error 2714 before anything runs), nor declare a variable twice
  (error 134).
- Temporary tables are transactional: a table created in a rolled-back
  transaction disappears, and rolled-back changes and drops are undone.
  Table variables are not: rows written inside a transaction stay after
  `ROLLBACK` (also `ROLLBACK` to a savepoint, when the transaction feature
  supports savepoints), and IDENTITY values are not reused.
- Tables whose scope ends inside an open transaction are dropped again if
  that transaction rolls back.

## Errors

| Case | Error |
| --- | --- |
| Unknown `#t` or `##t` | 208, state 0, class 16, `Invalid object name '#t'.` |
| Unknown or out-of-scope `@t` | 1087, state 2, class 15, `Must declare the table variable "@t".` |
| `CREATE TABLE #t` when the scope already has `#t` | 2714, state 6 (state 1 for `SELECT INTO` and for a batch that creates `#t` twice) |
| `DROP TABLE #missing` | 3701, state 5, class 11 |
| `TRUNCATE TABLE @t`, `DROP TABLE @t`, `SELECT ... INTO @t` | 102, state 1, class 15 |

Messages from constraint, binding and conversion errors name the temporary
object as written (`#t`, `@t`) rather than its backend table.

## Implementation

Every temporary object is an ordinary backend table in the `dbo` schema of
the database that is current when it is created, so declared types,
IDENTITY, defaults, constraints, indexes and the catalog work exactly as for
permanent tables. A statement that names a temporary object is rewritten to
the backend name and then runs through the ordinary engine path.

- `#t` becomes `__msduck_temp_<id>_t`, and `@t` becomes `__msduck_tv_<id>_t`,
  where `<id>` is unique to each creation.
- `##t` becomes `__msduck_global_t` (in lower case), which every session
  derives the same way.

Each session keeps a registry of the tables it created and a stack of batch
scopes. Table variable rows written inside a transaction are copied (as
Arrow batches) after each write, and restored after a rollback. When a
database is opened, backend tables left behind by a process that ended
without closing its sessions are dropped.

## Evidence

- `scripts/capture-gaps-temp_tables.mjs` runs 48 probes over three
  connections to one fresh database of a SQL Server container and keeps
  the results in `reference/gaps-temp_tables.json` (captured from SQL
  Server 2022 16.0.4236.2, with generated constraint names masked). Run it
  without arguments to compare a new capture with the fixture.
- `tests/compat/temp_tables.test.mjs` replays the same probes against msduck
  through tedious and requires the same rows and error numbers, and the
  same state, class and message for the errors in the table above. It also
  checks that a reset connection drops temporary tables and that prepared
  statements read them.
- `tests/gaps_temp_tables.rs` runs the generic repros `CREATE TABLE
  #foo(id int)` and `DECLARE @foo TABLE(id int)` through a TDS client,
  checking values, isolation between sessions, error numbers and the
  lifetime of a global table, and checks that a reopened database drops
  orphaned temporary tables.
- Unit tests cover nested bodies (visibility, shadowing and cleanup),
  scope ends inside a transaction, session end, name classification and
  rewriting.

[table-variable.md](table-variable.md) and
[temp-table-scope-reference.md](temp-table-scope-reference.md) hold earlier
reference captures of the same behavior.

## Limits

- Temporary objects live in the database that was current when they were
  created. After `USE` of another database, references to them fail with an
  explicit "unsupported reference" error instead of resolving.
- The backend tables appear in the creating database's catalog views
  (`sys.objects`, `sys.tables`, `sys.columns`, `INFORMATION_SCHEMA`) under
  their backend names, for every session. SQL Server lists them only in
  `tempdb`. Consequently `tempdb.sys.objects WHERE name LIKE '#t%'` does not
  find them; use `OBJECT_ID('tempdb..#t')`.
- `OBJECT_NAME` of a temporary table's ID returns the backend name, where
  SQL Server returns NULL outside `tempdb`.
- In `IF`, `WHILE` and `SET` conditions, `tempdb..#t` in a `FROM` clause is
  refused with 208 before this feature sees it; write `#t`.
- Preparing (`sp_prepare`) a batch that declares a table variable fails,
  because the variable does not exist until the batch runs. Prepared
  statements that use temporary tables work.
- References to undeclared table variables are reported when the statement
  runs, so earlier statements of the batch have already run. SQL Server
  reports them when it compiles the batch.
- The table variable restore after a rollback appends the saved rows, so
  computed columns in table variables are not restored.
- Every write to a table variable inside an open transaction copies the
  variable's rows, so writing a large table variable row by row inside a
  transaction is slow.
- `CREATE VIEW` over a temporary object is refused by the view checks, but
  with a generic error instead of SQL Server's 4508.
- Nested bodies get their own scope only when they run through the
  engine's batch loop, as procedures, triggers and dynamic SQL are expected
  to (see the stored procedure feature).
- Gaps of the engine apply equally to temporary objects, for example
  `UNIQUE` on `nvarchar` columns, `SET IDENTITY_INSERT`, `MERGE`, `SAVE
  TRANSACTION`, `OUTPUT` of constants into a table, statement errors inside
  explicit transactions, and SQL Server's exact constraint error messages.
