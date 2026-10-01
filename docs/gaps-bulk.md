# Bulk load: INSERT BULK and BulkLoadBCP

Issue #730 (task `gaps-bulk-v1`). Bulk-load clients (tedious `newBulkLoad`
and `execBulkLoad`, mssql `request.bulk()`, SqlBulkCopy, tiberius
`bulk_insert`, bcp) send an `INSERT BULK` statement as a SQL batch, then the
rows as one BulkLoadBCP message (TDS packet type 0x07: COLMETADATA, ROW
tokens, DONE). Before this task, msduck's parser rejected the statement with
102, so `mssql.Table` with `request.bulk()` failed at once.

The expected behavior comes from SQL Server 2022 (16.0.4236.2), captured by
`scripts/capture-gaps-bulk.mjs` into `reference/gaps-bulk.json` (two
identical runs in fresh databases). Bulk steps go through tedious, so the
statement and message are exactly what tedious and mssql send.
`tests/compat/bulk.test.mjs` replays every captured case against msduck and
compares diagnostics (number, state, class, message), informational
messages, rows, the load's row count and its DONE tokens.
`tests/gaps_bulk.rs` writes the wire bytes itself (every fragmentation of a
message, the error paths) and loads rows through tiberius over a socket.

## Statement

```text
INSERT BULK table ( column type [COLLATE name] [NULL | NOT NULL] [, ...] )
    [ WITH ( option [, ...] ) ]
```

- The table can be one-, two- or three-part (the current database), a
  `#temporary` table, quoted or not, with spaces around the dots.
- Options: `CHECK_CONSTRAINTS`, `FIRE_TRIGGERS`, `KEEP_NULLS`, `TABLOCK`,
  `ROWS_PER_BATCH = n`, `KILOBYTES_PER_BATCH = n` and
  `ORDER (column [ASC | DESC], ...)`, in any case. `TABLOCK` and the batch
  and order hints are accepted and have no effect.
- SQL Server has no `KEEP_IDENTITY` option: it fails with 102 like any other
  unknown option. A client keeps identity values (SqlBulkCopy's
  `KeepIdentity`) by listing the identity column; otherwise it leaves the
  column out and the values are generated.
- The statement must be alone in its batch (428). A successful statement
  answers with a DONE token without a count (CurCmd 253) and makes the
  session wait for the BulkLoadBCP message.

Statement errors, each followed by a failed DONE with CurCmd 253:

| Case | Error |
| --- | --- |
| Missing table | 208 twice |
| Column the table does not have | no diagnostic, only the failed DONE (as SQL Server) |
| Column listed twice | 264 |
| Computed column | 271 |
| Unknown type | 2715 state 2, "Column, parameter, or variable #n: ..." |
| Unknown option, missing column list | 102, near the offending token |
| Other statements in the batch | 428 |

If the next request is not the BulkLoadBCP message (a SQL batch, an RPC or
a transaction-manager request), it fails with 4022 and does not run; the
INSERT BULK statement is forgotten. An attention signal cancels the
expected load. A BulkLoadBCP message without INSERT BULK before it fails
with a DONE error and no diagnostic.

## BulkLoadBCP message

The server hands the message to the session packet by packet, so a load is
not limited by the 16 MiB message size of other requests: rows are decoded
as they arrive (with the read-only codec `crates/msduck-tds/src/bulk_load.rs`,
see docs/bulk-load-codec.md, fed in pieces of at least 1 MiB because it
re-parses an unfinished token on every push) and at most one INSERT
statement's worth of rows, and at most 8 MiB of values, stays buffered.

SQL Server checks the COLMETADATA token against the statement and the table
(4816 "Invalid column type from bcp client for colid n", CurCmd 253):

- the wire type must belong to the type the statement declared for that
  position (an `int` declaration with an `nvarchar` wire type fails);
- the NULLABLE flag must equal the target column's nullability: tedious
  columns are nullable unless `nullable: false`, so a NOT NULL column needs
  `nullable: false` and a nullable one must not have it (state 1);
- a `varchar(max)`, `nvarchar(max)` or `varbinary(max)` column needs a MAX
  (PLP) wire type (state 2).

Columns bind by position in the statement; the metadata column names are
not used. A different number of metadata columns fails with 4804 state 3.
A message without COLMETADATA (tedious sends only DONE for zero rows) fails
with 4804 state 2. A row that does not match its metadata fails with 4804
state 1, severity 17 (tedious sends a `binary(n)` value shorter than n with
length n). COLMETADATA, no rows and DONE load nothing and succeed with a
count of 0.

## Loading

The rows are inserted through the engine's INSERT, so the semantics follow
from it: conversion from the declared type to the column type with
INSERT's rules and errors, PRIMARY KEY and UNIQUE (2627 and 3621), NOT NULL
(515), truncation (2628), defaults, identity, computed and rowversion
columns, temporary tables. A load that fits in one statement (up to 1000
rows, fewer for wide rows) runs as one `INSERT ... VALUES` with typed
parameters. A larger load fills a private staging table
(`#__msduck_bulk_<session>`, with the declared types) statement by
statement, then copies it in file order with one `INSERT ... SELECT` and
drops it. Either way:

- the load is atomic: in autocommit mode it runs in its own transaction,
  and a failure loads nothing. In an explicit transaction, a failure after
  one of the load's statements wrote rows makes the transaction fail, so
  those rows can never commit (the transaction itself otherwise follows
  the engine; see Remaining limits). A trigger that fails or rolls back
  fails the load without showing the client a transaction it did not
  begin;
- the response is SQL Server's: a DONE with the row count and CurCmd 240
  (no count under NOCOUNT); a failure that terminates the statement (2627,
  515, 2628, 547) sends 3621 and a failed DONE with CurCmd 240; a
  conversion failure (245, 241) or a failure under XACT_ABORT a failed DONE
  with CurCmd 253 and no 3621. Completions of the load's own statements
  and of triggers it fires are not sent;
- `@@ROWCOUNT` is the number of rows loaded and `@@ERROR` the failure's
  number;
- the client's IGNORE bit (tedious `cancel()` while streaming) abandons the
  message: nothing is loaded and the response is a failed DONE.

Options:

- **Identity.** A listed identity column keeps the client's values and
  advances the identity like `SET IDENTITY_INSERT ON`: the load sets it
  for its INSERT in a nested scope (another table's setting is suspended),
  and the session's own setting is unchanged afterwards, whatever it was.
  `IDENT_CURRENT` and `@@IDENTITY` follow.
- **KEEP_NULLS.** Without it, a NULL for a column with a DEFAULT takes the
  default (the VALUES form writes `DEFAULT` for it; the staging form leaves
  the column out of the INSERT for those rows). With it, NULL is stored.
  Columns the statement leaves out always take their default.
- **CHECK_CONSTRAINTS.** Without it, the table's CHECK and FOREIGN KEY
  constraints are not checked for the load, and its enabled ones are marked
  not trusted afterwards (`is_not_trusted = 1`), as SQL Server does: the
  constraints feature (docs/gaps-constraints.md) is suspended for the
  load's INSERT, and only constraints still trusted are written, so loads
  do not contend on the constraint store. With it, violations fail with 547
  and 3621 and the trust is unchanged.
- **FIRE_TRIGGERS.** Without it, triggers do not fire. With it, AFTER
  INSERT triggers fire once for the whole load.

## Design

The feature uses the extension hooks (docs/extension-hooks.md):

- `crates/msduck-sql/src/dialect/ext/bulk.rs` parses INSERT BULK into a
  carrier whose payload is the canonical statement text, with SQL Server's
  102 messages.
- `src/engine/ext/bulk.rs`: the `batch` hook handles a batch with INSERT
  BULK (428, binding, the pending load); `Session::bulk_load_*` receive the
  message for the server.
  - `plan.rs` binds the statement to the table (temporary tables and the
    current database resolve as for a SELECT).
  - `wire.rs` path-imports the codec, checks the metadata (4816) and turns
    each ROW value into a typed parameter.
  - `load.rs` buffers, stages and inserts the rows and builds the response.
- `src/server.rs` streams the packets of the message that follows INSERT
  BULK into the session, and answers other requests with 4022.

## Remaining limits

- Differences of the engine, not of the load, that the replay accepts as
  documented: a NULL for a NOT NULL column (explicit or omitted) fails with
  DuckDB's "NOT NULL constraint failed" text and no 3621; failed implicit
  conversions of a varchar value to int or datetime report 245 with
  DuckDB's text (gaps-conversion-v1); 2628 names the database `master`
  (gaps-identifiers-v1); a key violation inside an explicit transaction
  aborts the DuckDB transaction, as for INSERT, where SQL Server keeps it
  (XACT_STATE 1).
- 2628's "Truncated value" names the value that did not fit; SQL Server's
  bulk path fills it with uninitialized bytes.
- Rows that need a column default only in some rows (without KEEP_NULLS) in
  a load larger than one statement are inserted with one statement per
  combination of defaulted columns: triggers fire once per statement and
  identity values follow those groups rather than file order (rows whose
  every listed column takes its default are inserted as rows of DEFAULT,
  1000 per statement). Loads that
  fit in one statement, and larger loads where every row has the same
  defaulted columns, are a single statement.
- Without CHECK_CONSTRAINTS, statements of a trigger the load fires (with
  FIRE_TRIGGERS) skip CHECK and FOREIGN KEY constraints too, on any table.
- A load that keeps identity values does not change `SCOPE_IDENTITY()`
  (its INSERT runs in a nested scope); SQL Server reports the load's last
  identity value. `@@IDENTITY` and `IDENT_CURRENT` are updated.
- Views are not supported as targets: the engine cannot insert into views,
  and their columns read as nullable, so a NOT NULL wire column already
  fails with 4816.
- INSERT BULK in an RPC request (sp_executesql) is refused explicitly; it
  was not probed against SQL Server. Other databases than the current one
  fail like other cross-database references.
- `TABLOCK`, `ORDER`, `ROWS_PER_BATCH` and `KILOBYTES_PER_BATCH` are hints
  without effect; locking follows DuckDB.
- Character values are decoded as Windows-1252 whatever their collation;
  `sql_variant`, `xml` and UDT wire types are refused by the codec or the
  type check.
- A load inserts through the engine's INSERT statement by statement; a
  debug build loads a few thousand rows per second.
