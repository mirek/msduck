# SQL Server table-variable reference behavior

`reference/table-variable.json` retains 44 labelled requests in each of two
fresh databases in each of two independent SQL Server 2025 containers (176
requests total). All four raw runs matched. The image is pinned to
`mcr.microsoft.com/mssql/server:2025-latest@sha256:86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a`.
The fixture preserves rows, ordered result descriptors, error number/state/
class/line/text, information messages, DONE-family tokens, callback counts
and RPC return statuses. Binary and date values use the repository's canonical
JSON representation. No SQL Server fixture value is treated as a command.

The copied mssqlite T-SQL skill describes a batch-scoped table-variable
implementation over SQLite temporary tables. That is an upstream status note,
not an msduck implementation claim. The observations here come from SQL Server.
msduck currently does not implement table variables.

## Observed declaration and type behavior

- `DECLARE @t TABLE (...)` provides a typed result descriptor even when no
  rows are present. The captured empty projection has non-null `Int` (`flags
  8`), nullable `NVarChar` length 10 (`flags 9`), `DecimalN(8,2)` length 17,
  and `DateTime2(3)`. The fixture retains the collation on the character
  column. The generated table-variable name is not exposed as a user table.
- The same variable is used by successive statements within its SQL batch:
  inserts, ordered reads, a count, `INSERT ... SELECT`, joins and a CTE.
  Defaults and nullable columns work; `PRIMARY KEY`, `UNIQUE` and `CHECK`
  declarations accept valid rows. A duplicate key leaves the first row
  available after TRY/CATCH; its captured diagnostic identity is 2627,
  state 1, severity 14, line 1.
- A missing required value raises 515/state 2/class 16, with `@t` in the
  message. Duplicate column names raise 2705/state 3/class 16. An unknown
  column type raises 2715/state 6/class 16. A table declaration followed by
  another table or scalar declaration in the same `DECLARE` raises
  102/state 1/class 15 at the comma. `TRUNCATE TABLE @t` and
  `SELECT ... INTO @t` also raise 102 in these probes.
- The unnamed primary/unique constraints receive generated names. Their
  *uncaught* violation messages vary across fresh databases. Explicit
  `CONSTRAINT name` inside a table-variable declaration was rejected with
  syntax error 156 in all four retained runs. The fixture therefore
  captures the primary-key error number/state/severity through TRY/CATCH,
  without inventing a stable constraint name or replacing the raw error text.

## DML, scope and transactions

- `INSERT`, `UPDATE` and `DELETE` target a table variable and produce
  `OUTPUT` rows. `OUTPUT ... INTO @sink` captures rows from an ordinary-table
  insert. The four result sets in the DML probe preserve their own column
  descriptors, row order and completion counts; the callback count is the
  sum of contributing DONE counts, not merely the final SELECT count.
- A table variable and a `#temp` table or an ordinary `dbo` table can coexist
  in one batch without confusing their names or rows. A separate request on
  the same connection cannot see a table variable declared in an earlier
  request: 1087/state 2/class 15. Dynamic `EXEC` and `sp_executesql` cannot
  see the caller's variable either, though a dynamic batch may declare and use
  its own. A procedure may declare/use a local variable, which the caller
  cannot see afterward. Creating a procedure that directly references the
  caller's undeclared table variable fails with 1087.
- A parameterized `sp_executesql` request declares a table variable, inserts
  the bound integer and returns it. Two executions of one prepared statement
  each start with a fresh declaration, yielding 12 then 13. Preparing a query
  that references an undeclared table variable returns 1087 followed by
  8180, sends no `sp_execute`, and has no `sp_unprepare` in the capture.
  The successful prepare phase reports return status 8182 in all four runs;
  this is retained as an observed TDS/tedious value, not interpreted as an
  application error when the prepare and executions succeed.
- Rolling back a transaction left an inserted table-variable row available
  while the ordinary-table insertion disappeared. A second table-variable
  identity allocation produced ID 2 after rollback. Rolling back to a
  savepoint similarly left the table-variable row while removing the
  ordinary-table row. The fixture captures the ordered rows and DONE tokens
  before and after these operations.

## Failed multi-row statements

- A three-row `INSERT` into a `NOT NULL` table variable failed on a NULL row
  with caught diagnostic 515/state 2/class 16. The earlier row `7` remained;
  neither other row from the failed statement appeared. The caught
  `XACT_STATE()` and `@@TRANCOUNT` were both 0.
- A two-row `UPDATE` with one valid candidate (`3` to `1`) and one invalid
  candidate (`1` to `-1`) emitted an `OUTPUT` row `(1,10)` for the valid
  candidate *before* failing the `CHECK` constraint with caught diagnostic
  547/state 0/class 16. Both original rows `(1,20)` and `(3,10)` remained
  afterward. The `OUTPUT` row is evidence of work attempted before failure,
  not a committed row. The generated constraint
  name is kept out of the stable probe by catching the error; the fixture
  retains the diagnostic identity, typed result descriptors and DONE tokens.
- Inside an explicit transaction, a failed three-row `INSERT` reported
  547/state 0/class 16 with `XACT_STATE() = 1` and `@@TRANCOUNT = 1` in the
  catch block. The table variable still held its earlier `(1,10)` row before
  and after `ROLLBACK`, while the ordinary-table control row disappeared.
  The next successful table-variable identity value was 4: the failed
  statement allocated values 2 and 3 even though it inserted no rows.
  These probes establish statement atomicity for the captured forms, not
  every constraint or transaction setting.

## Capture and replay

Run `node scripts/capture-table-variable.mjs --one-database` for a disposable
diagnostic capture. To create the retained fixture only when it does not exist,
run `node scripts/capture-table-variable.mjs --write-fixture`. The normal
command, without either flag, captures four fresh databases and compares
them to the retained fixture. An optional positional path changes only the
scratch output. The generator rejects paths that equal, symlink to or hard
link to the retained fixture before starting a container. It writes bounded
scratch output under ignored `artifacts/`, never silently replaces reference
evidence, and reports only a bounded first difference on mismatch.

Each request limits retained result sets to 32, rows per set to 200 and
messages/completion tokens to 200. Truncation is counted explicitly. The
client time zone and container time zone are UTC, statement text and object
names are stable, and the four captures use two newly created databases in
each of two owned containers. `scripts/lib/reference-container.mjs` supplies
the owner label and pinned image. The generator is a reference capture, not a
SQL Server compatibility test of msduck.

## Uncaptured behavior and implementation work

The exact uncaught message for an unnamed key/check violation is not retained
because SQL Server embeds a different generated constraint name per database.
Further reference work is needed for user-defined table types, alias types,
collations beyond the default, index declarations and query plans, computed
columns, named-constraint alternatives, multi-row statement atomicity under
other constraints and transaction settings, nested procedures, triggers, cursors, TVPs,
`INSERT EXEC`, and isolation across concurrent sessions.

A deterministic SQL-layer successor should parse and bind
`DECLARE @t TABLE` with typed columns, defaults and constraints, maintain
lexical batch/procedure scope, and resolve object-position `@t` in SELECT and
DML without treating it as a scalar. The root adapter must create session-
private storage with typed descriptors and statement atomicity, preserve rows
and identity allocation across transaction/savepoint rollback as captured,
and drop the storage on scope exit even after errors. RPC/prepared execution
needs a fresh table-variable scope per execution, with exact diagnostics and
DONE-family completion. Each successor needs its own non-overlapping claim;
this reference task does not edit parser, catalog, engine or client tests.
