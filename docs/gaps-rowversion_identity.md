# rowversion, decimal identity, SCOPE_IDENTITY and IDENTITY_INSERT

This feature closes four gaps observed against v0.2.4 (issue #717):

- `rowversion` columns failed with 208;
- `timestamp` columns were created as DuckDB timestamps and failed with 515;
- `decimal(15,0) IDENTITY` and `SCOPE_IDENTITY()` failed;
- `SET IDENTITY_INSERT` was rejected as an unsupported setting.

The runtime lives in `src/engine/ext/rowversion_identity.rs` and its
submodules, and the syntax in
`crates/msduck-sql/src/dialect/ext/rowversion_identity.rs`. Both use the
[extension hooks](extension-hooks.md).

Ground truth is `reference/gaps-rowversion_identity.json`, captured by
`scripts/capture-gaps-rowversion_identity.mjs` from SQL Server 2022 (16.0.4236.2).
It holds 11 cases, each run statement by statement in a fresh database and
captured twice identically. `tests/compat/rowversion_identity.test.mjs` replays
every captured statement through tedious and compares rows, errors (number,
state, class and message) and column descriptors. The few documented
differences listed below are excluded. Further tests assert:

- values through bound parameters;
- RPC scope;
- two and four concurrent sessions.

`tests/gaps_rowversion_identity.rs` checks values, error tokens and a restart
that replays the write-ahead log.

## rowversion and timestamp

`rowversion` and its T-SQL synonym `timestamp` declare a binary(8) column.
T-SQL also allows a bare `timestamp` column without a type, as in
`CREATE TABLE t(id int, timestamp)`. That column is named `timestamp`.

- **Values.** Each database has one counter, the private sequence
  `main.__msduck_rowversion`. A rowversion column's default is the counter's
  next value as eight big-endian bytes.
  - A fresh database has `@@DBTS` 0x00000000000007D0, and the first value is
    0x...07D1, as in SQL Server.
  - The counter is shared by all tables of the database and survives
    restart.
  - Values are never reused, even after rollback.
- **INSERT.**
  - An omitted rowversion column takes a new value.
  - A `NULL` or `DEFAULT` value also takes a new value. This holds in VALUES
    lists, positional VALUES and, for NULL, `INSERT ... SELECT`.
  - Any other value fails with 273.
  - A positional INSERT with the wrong number of values fails with 213.
- **UPDATE.**
  - Every UPDATE of a row assigns a new value, even if no column value
    changes. This includes `UPDATE alias ... FROM table alias`.
  - Assigning the column fails with 272.
  - Optimistic concurrency (`WHERE rv = @original`) therefore works with
    bound binary parameters.
- **DDL.**
  - A second rowversion column fails with 2738, in CREATE TABLE or ALTER
    TABLE ADD.
  - A DEFAULT on the column fails with 1755 and 1750.
  - ALTER TABLE ADD fills the existing rows with new values.
- **Nullability.** Rowversion columns are NOT NULL unless declared NULL, as
  SQL Server reports. A nullable column also always holds a value.
- **Metadata.** Result descriptors are BINARY(8). sys.columns names the type
  `timestamp`, with max_length 8.
- **@@DBTS and MIN_ACTIVE_ROWVERSION().** `@@DBTS` returns the last value
  used in the current database as varbinary(8). `MIN_ACTIVE_ROWVERSION()`
  returns the next value.

## decimal, numeric, tinyint and smallint identity

- **decimal and numeric.** A `decimal(p,0)` or `numeric(p,0)` IDENTITY column
  gets the same private allocator as the integer identity columns of
  `src/identity.rs`: a `main.__msduck_identity_<hex>` sequence and a
  definition row. IDENT_SEED, IDENT_INCR, IDENT_CURRENT, sys.identity_columns,
  TRUNCATE (reseed) and DROP TABLE treat these columns like integer identity
  columns. An exhausted precision fails with 8115, "Arithmetic overflow error
  converting IDENTITY to data type decimal." (or tinyint and so on).
- **tinyint and smallint.** These already worked through `src/identity.rs`.
  Their exhaustion now also reports SQL Server's 8115 message.
- **Errors.**
  - Other identity types (for example `decimal(5,2)` or `float`) fail with
    2749.
  - A nullable decimal identity fails with 8147.
  - A DEFAULT on it fails with 1754 and 1750.
- **sys.identity_columns.** seed_value, increment_value and last_value of a
  decimal identity are numeric sql_variant values. They are encoded in
  `src/variant.rs`.

## SCOPE_IDENTITY() and @@IDENTITY

Both are numeric(38,0) values for the session. They change after each
successful INSERT:

- an INSERT that produced identity values sets both to its last value;
- an INSERT into a table without an identity column sets both to NULL;
- an INSERT of zero rows into an identity table changes nothing;
- failed INSERTs, UPDATE and DELETE change nothing;
- rollback changes nothing.

The values persist across batches of a connection, and each session has its
own. Each RPC request (`sp_executesql`, which tedious uses for `execSql`, and
prepared execution) starts with `SCOPE_IDENTITY()` NULL. Afterwards the
caller's scope value is unchanged, while `@@IDENTITY` keeps the RPC's last
value. This follows `reference/identity-insert-rpc.json` and
`reference/identity-retrieval.json`.

A generating INSERT holds a process-wide lock on its table's sequence while
it runs. The sequence's last value afterwards is therefore this INSERT's
last value, even with concurrent sessions inserting into the same table.

## SET IDENTITY_INSERT

`SET IDENTITY_INSERT [schema.]table ON|OFF` uses the deterministic rules of
`crates/msduck-sql/src/identity_insert.rs` and
`crates/msduck-sql/src/identity_insert_gate.rs`. The live catalog adapters
are `src/identity_insert_*.rs`. Earlier tasks captured these rules (see the
`identity-insert-*` documents); this task wires them into the engine.

- **SET.**
  - One table per session can be ON. A second fails with 8107, naming the
    active table as `database.schema.table`.
  - A table without identity fails with 8106, and a missing table with 1088.
  - OFF for another table is accepted.
  - The setting survives ROLLBACK.
  - A SET inside an RPC request lasts only for that request. An RPC sees the
    caller's setting but cannot change it.
- **INSERT while ON.** INSERT needs a column list that includes the identity
  column:
  - positional values fail with 8101, naming the table as the INSERT spells
    it;
  - an omitted identity column fails with 545, also with DEFAULT VALUES;
  - NULL or DEFAULT identity values fail with 339.
- **INSERT while OFF.** An explicit identity value fails with 544.
- **Explicit values.**
  - Each explicit value passes through `__msduck_identity_note`, which
    records it for the session.
  - `SCOPE_IDENTITY()` and `@@IDENTITY` become the last row's value.
  - The allocator then advances past the extreme value in the direction of
    the increment, through `__msduck_identity_advance` (non-transactional, as
    in SQL Server). A lower value on an ascending identity does not move it
    back.
  - Explicit values work in VALUES lists and in `INSERT ... SELECT`, also
    with bound parameters.
  - `src/insert.rs` lets the feature permit explicit identity values for the
    target table while the statement runs (`with_explicit_identity`).

## Remaining limits

- **sys.columns type.** sys.columns reports a rowversion column's
  system_type_id as 173 (binary), with user_type_id 189 so the type name is
  `timestamp`. SQL Server reports 189. Result metadata binds declared types
  by system type, and that binding is outside this feature's files.
  INFORMATION_SCHEMA.COLUMNS still shows DuckDB's BLOB.
- **Numeric results.** numeric(38,0) results such as `SCOPE_IDENTITY()`
  travel as DECIMALN rather than NUMERICN, with the same precision and
  scale. Tedious decodes both the same way.
- **sql_variant properties.** `SQL_VARIANT_PROPERTY` is not supported for
  numeric values, so `SQL_VARIANT_PROPERTY(SCOPE_IDENTITY(), 'BaseType')`
  fails with 40515. For decimal identity columns in sys.identity_columns it
  returns NULL. Their sql_variant values carry precision 38, not the column's
  precision.
- **Decimal identity range.** Decimal identity values are limited to the
  bigint range, because DuckDB sequences are BIGINT. A seed or increment
  beyond it fails explicitly (40515). ALTER TABLE ADD of a decimal identity
  column is still unsupported.
- **Scopes.** Procedures, triggers and functions do not get scopes of their
  own (other tasks implement them): SCOPE_IDENTITY() inside and after them
  follows the session, like @@IDENTITY.
- **Rowversion writes.** MERGE, OUTPUT INTO and BULK INSERT do not assign
  new rowversion values on update. Their inserts use the column default.
- **Positional sources without a known width.** A positional INSERT whose
  source is a wildcard SELECT or a set operation is not rewritten. A NULL
  for the rowversion column there fails with 515 instead of generating a
  value.
- **Explicit identity edge cases.**
  - Explicit identity values from source shapes other than VALUES or a
    simple SELECT list (for example UNION) fall back to the table's extreme
    stored value for SCOPE_IDENTITY() and the advance.
  - A parallel INSERT ... SELECT may record a "last" value other than the
    last row in SELECT order.
  - Explicit values written through OUTPUT INTO are rejected with 544, as
    before.
  - A prepared (`sp_prepare`) explicit-identity INSERT is validated without
    the session setting and fails with 544.
  - `SET IDENTITY_INSERT` itself cannot be prepared.
- **Allocation on bind-time failures.** msduck rejects some values, such as
  `'x'` for an int column, before it allocates an identity value, and with
  DuckDB's message. SQL Server consumes the value first.
- **Not implemented.** Converting binary to bigint, `CONVERT` styles for
  binary values, and `rowversion` or `timestamp` as a variable or CAST type
  are separate gaps.
