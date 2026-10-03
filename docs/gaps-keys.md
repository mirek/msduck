# Keys and indexes of every type

PRIMARY KEY, UNIQUE and index keys work on every column type msduck stores,
including the STRUCT carriers of NVARCHAR/NCHAR, DATETIME2 and
DATETIMEOFFSET. UNIQUE keys allow a single NULL, as SQL Server does.
`CREATE [UNIQUE] [CLUSTERED | NONCLUSTERED] INDEX` accepts INCLUDE, filters,
descending keys, WITH options and ON filegroup clauses. Duplicate keys fail
with SQL Server's 2627 or 2601 and message.

The feature is the `keys` extension (see [extension hooks](extension-hooks.md)):

- `crates/msduck-sql/src/dialect/ext/keys/` holds the deterministic parts.
  - `index.rs` parses CREATE INDEX in T-SQL clause order.
  - `table.rs` classifies the key constraints of CREATE TABLE.
  - `value.rs` builds index key expressions and shows key values.
  - `filter.rs` lowers filter predicates.
  - `message.rs` reads DuckDB's duplicate-key errors and writes SQL Server's.
- `src/engine/ext/keys/` runs CREATE TABLE, CREATE INDEX, DROP INDEX and
  the error translation for INSERT, UPDATE and MERGE.
- `crates/msduck-sql/src/dialect/key_index_type.rs` rewrites column-level
  constraints that have a column list.

## Reference evidence

`reference/gaps-keys.json` holds 16 SQL Server programs captured twice,
identically, in fresh databases by `scripts/capture-gaps-keys.mjs`. Each
step keeps its rows, diagnostics (number, state, severity, message) and DONE
tokens. The random suffix of system-generated constraint names and the
generated database name are redacted.

The programs ran on the pinned reference image
(`mcr.microsoft.com/mssql/server:2025-latest@sha256:86cc6144…`, SQL Server
2025 RTM-CU7, 17.0.4065.4); the fixture records the image and `@@VERSION`.
To capture again:

```sh
node scripts/capture-gaps-keys.mjs artifacts/compatibility/gaps-keys-reference
```

An earlier capture on `mssql/server:2022-latest` differed only in the text of
error 1911 ("target table or view" there, "target table, index or view" in
2025).

The script labels its container `msduck.task=gaps-keys-v1:<pid>` and writes
the fixture to the output directory. It never replaces the checked-in one.

`tests/compat/keys.test.mjs` replays every program through tedious. It
requires exact equality with the fixture for every step not listed in its
`differences` table, which names the reason for each listed step.

## How keys are enforced

DuckDB cannot index STRUCT columns. A DuckDB unique index or constraint also
lets any number of NULL keys coexist. A constraint is therefore enforced in
one of three ways.

- **Native.** DuckDB enforces the constraint when every key column has
  indexable storage and cannot be NULL. That covers primary keys over
  integer, decimal, character, binary, uniqueidentifier and similar columns,
  and UNIQUE over NOT NULL columns. Foreign keys can reference these.
- **Native and managed.** A UNIQUE constraint over nullable columns with
  indexable storage keeps its native DuckDB constraint, so foreign keys can
  still reference it. A *keys-managed* unique DuckDB index over expressions
  adds SQL Server's single-NULL rule.
- **Managed.** A constraint over STRUCT storage is only a keys-managed
  index. It is removed from the CREATE TABLE that DuckDB runs, and its
  PRIMARY KEY columns become NOT NULL.

A managed index has these key expressions:

1. A tag, `CASE WHEN <filter> THEN <tag> END`. DuckDB's duplicate-key error
   does not name the index; the tag's value identifies it. Rows outside a
   filter get a NULL tag, and DuckDB does not compare keys containing NULL.
2. For each key column, its comparable value:
   - NVARCHAR/NCHAR: the UTF-16 code units in hexadecimal, without trailing
     spaces. Under a case-insensitive collation (the database default) the
     units the collation ignores (NUL, surrogates, U+200D, U+200E, U+FEFF,
     U+FFFE, U+FFFF) are dropped and each unit of Basic Latin, Latin-1,
     Latin Extended-A, basic Greek and basic Cyrillic is folded to lower
     case first, by `regexp_replace` rules over whole units. Under a case-sensitive or binary column collation this is
     BIN2 equality.
   - VARCHAR/CHAR: the text without trailing spaces (lowercased under a
     case-insensitive collation), in hexadecimal. A key over such columns
     always gets a managed index under a case-insensitive collation, because
     DuckDB's native constraint compares the stored text exactly.
   - DATETIME2 and DATETIMEOFFSET: the UTC ticks. Two DATETIMEOFFSET values
     at the same instant are equal, whatever their offsets.
   - BINARY/VARBINARY: hexadecimal.
   - Numbers and bit: the value itself.
   - Other types: their canonical text.
3. For each nullable key column, an `IS NULL` discriminator, and NULL in the
   value replaced by a typed zero. NULL then equals NULL, and stays distinct
   from zero.

The expressions use only built-in DuckDB functions. DuckDB can then bind the
index while it replays its write-ahead log, before msduck registers its own
functions.

Non-unique indexes over carrier columns index the same comparable values,
without the tag.

## CREATE TABLE

- Column constraints (`id nvarchar(450) NOT NULL CONSTRAINT pk PRIMARY KEY
  NONCLUSTERED`) and table constraints are both handled.
- SQL Server accepts a table constraint after a column without a comma, for
  example `id int NOT NULL CONSTRAINT uq UNIQUE(id)` or
  `b int NOT NULL CONSTRAINT pk PRIMARY KEY (a, b)`. The tokenizer inserts
  the comma inside CREATE TABLE.
- Each constraint's SQL Server name is recorded in `main.__msduck_keys`.
  Unnamed constraints get `PK__<table>__<16 hex digits>` or
  `UQ__<table>__<16 hex digits>`, the table name cut to eight characters, as
  SQL Server generates them.
- The table and its managed indexes are created atomically.
- The following CREATE TABLE errors match the captures:
  - 8111 + 1750: a PRIMARY KEY on a column declared NULL.
  - 8110: two primary keys.
  - 1919 + 1750: a MAX, text, ntext, image or xml key column.
  - 1909 + 1750: a key column listed twice.
  - 1911 + 1750: a key column that does not exist.
  - 8168: two constraints of the statement with the same name.
  - 2714 + 1750: a constraint name already used in the schema.

## CREATE INDEX

The table-owned index catalog (`src/index_catalog.rs`) still creates
ordinary indexes: non-unique, ascending, over indexable columns, without
options. This feature creates every other index and registers it in that
catalog. The catalog assigns its index ID, sees its name for 1913, and lets
DROP INDEX bind it. This feature also creates indexes over carrier columns.
The index's clustering, included columns, filter and key constraints are
kept in `main.__msduck_keys`.

- Index forms:
  - `CLUSTERED` is recorded. A second clustered index on the table fails
    with 1902.
  - `INCLUDE` columns must exist (1911). They do not change uniqueness.
  - `ASC`/`DESC` are accepted; DuckDB's index ignores the order.
  - Filters can be conjunctions of `column op constant`,
    `column IS [NOT] NULL` and `column IN (constants)`, which is SQL Server's
    filter grammar. Other predicates fail explicitly. NVARCHAR columns
    support only `=` and `<>` in a filter. Unknown filter columns fail
    with 207. Text constants are written in hexadecimal, so no user text
    appears in the index definition.
- WITH options, in parentheses or in the legacy unparenthesized form:
  - Accepted without effect on storage: FILLFACTOR (1 to 100, else 129),
    PAD_INDEX, SORT_IN_TEMPDB, STATISTICS_NORECOMPUTE,
    STATISTICS_INCREMENTAL, ONLINE, RESUMABLE, MAX_DURATION, MAXDOP,
    ALLOW_ROW_LOCKS, ALLOW_PAGE_LOCKS, OPTIMIZE_FOR_SEQUENTIAL_KEY,
    DATA_COMPRESSION and XML_COMPRESSION.
  - DROP_EXISTING = ON replaces the index of that name; without one it
    fails with 7999.
  - IGNORE_DUP_KEY = OFF is accepted. ON fails with 1916 on a non-unique
    index, as in SQL Server. On a unique index it fails explicitly as
    unsupported (40515).
  - An unknown option fails with 155.
- `ON filegroup`, `ON scheme(column)` and `FILESTREAM_ON` are accepted and
  ignored.
- A unique index over existing duplicates fails with 1505, showing the first
  duplicate key in key order, NULL keys first, as SQL Server does.
- Every check runs before the first change, so a failure inside a user
  transaction leaves nothing behind (DuckDB has no savepoints). For example,
  DROP_EXISTING over duplicates keeps the old index.
- Other errors: 1909 for a repeated key column, 1919 for a MAX key column,
  1913 for a name already used by an index or a key constraint of the table.

## ALTER TABLE

- DROP COLUMN of a column that a key constraint or index uses (as a key,
  INCLUDE or filter column) fails with SQL Server's 5074 and 4922:
  - `The object 'uq' is dependent on column 'code'.` for a constraint;
  - `The index 'ix' is dependent on column 'v'.` for an index;
  - `ALTER TABLE DROP COLUMN code failed because one or more objects access this column.`
- ALTER COLUMN of such a column fails the same way, unless it widens a
  varchar, nvarchar or varbinary column, which SQL Server allows.
- DuckDB refuses most ALTER TABLE forms while a table has an index, even
  ADD COLUMN with a default or NOT NULL. For ADD, DROP and ALTER COLUMN the
  feature therefore drops the table's indexes (its own and the table-owned
  index catalog's), runs the change, and recreates them from their
  definitions in the same transaction. The catalogs keep the same index IDs
  and names. Keys are enforced again afterwards; the tests check duplicates
  and DROP INDEX after each rebuild.
- Inside a user transaction, an ALTER that fails still gets its indexes
  back. If they cannot return, the transaction can only roll back.
- DuckDB still sees an index dropped in the open transaction when it checks
  DROP COLUMN before an indexed column, or ALTER COLUMN of an indexed
  column. Outside a user transaction such a change commits the drop first,
  runs, and then recreates each index, whether or not the change succeeded.
- Other ALTER TABLE forms keep DuckDB's refusal while the table has indexes.

## DROP INDEX

DROP INDEX binds each target in order against the registered indexes and the
recorded key constraints, using the shared binder (`msduck_sql::drop_index`).

- Dropping a constraint's index fails with 3723, for both PRIMARY KEY and
  UNIQUE KEY constraints.
- It works while tables have native key constraints. The built-in path
  refused every DROP INDEX in such a database.
- Outside a transaction each drop commits on its own, so earlier targets stay
  dropped when a later one fails, as before.
- `MAXDOP` fails with 3748 on a nonclustered index and is accepted on a
  clustered one.

## Duplicate-key errors

INSERT, UPDATE and MERGE run through the feature's `statement` hook, so a
DuckDB duplicate-key error becomes SQL Server's:

- 2627, severity 14, state 1:
  `Violation of PRIMARY KEY constraint 'name'. Cannot insert duplicate key in object 'schema.table'. The duplicate key value is (...).`
  UNIQUE constraints say `UNIQUE KEY`.
- 2601, severity 14, state 1:
  `Cannot insert duplicate key row in object 'schema.table' with unique index 'name'. The duplicate key value is (...).`

A managed index is found by its tag, which is read after the closing `END`
of its first expression, so a filter's text cannot hide it. A native
constraint is matched on the statement's target table by kind and key
columns. When one statement repeats a key, DuckDB names neither the kind nor
the columns. The constraint is then known only if it is the table's single
native key of that width. Otherwise the error keeps DuckDB's text, with
SQL Server's number 2627, rather than naming a constraint that may be the
wrong one.

The values are shown as SQL Server shows them:

- `<NULL>`, and bit as 0/1.
- char/nchar padded to their length.
- money with two decimals; time with its scale.
- datetime2 and datetimeoffset with their scale.
- datetime and smalldatetime as `Jan  2 2024  3:04AM`.
- binary as lowercase `0x…`.

The errors are catchable (ERROR_NUMBER, ERROR_SEVERITY, ERROR_STATE,
ERROR_MESSAGE, @@ERROR) and work for parameterized statements. As in SQL
Server, a failed INSERT or UPDATE in a SQL batch ends only its statement:
the engine adds "The statement has been terminated." (3621) and the batch
continues.
A failed statement can abort DuckDB's transaction. The catalog is then read
through a separate connection.

## Remaining limits

- **Collation.** Keys follow the column's collation: case-insensitive and
  accent-sensitive under the database default (`N'ABC'` after `N'abc'`
  fails with 2627 or 2601), BIN2 equality under case-sensitive and binary
  collations. Unicode keys fold case only in the blocks listed above, so
  case pairs elsewhere (for example Armenian) stay distinct in a key while
  comparisons treat them as equal. Accent-insensitive collations use the
  case-insensitive key and so still distinguish accents. Tables created
  before this change keep their case-sensitive key indexes.
- **Shown values.**
  - A DATETIMEOFFSET key value is shown in UTC (`+00:00`). SQL Server shows
    the inserted offset.
  - A Unicode or ANSI key value is shown as written when the failing
    statement's literals or parameters hold it (with its case and trailing
    spaces). Otherwise (a value computed or read from another table, or
    CREATE UNIQUE INDEX over existing rows) it is shown without trailing
    spaces and, under a case-insensitive collation, in lower case. SQL
    Server shows the stored text.
- **Clustering of constraints.** The CLUSTERED/NONCLUSTERED keyword on
  PRIMARY KEY and UNIQUE, and DESC key columns, are recorded for the
  catalogs (`main.__msduck_key_layout`, read by `sys.indexes` and
  `sys.index_columns`; see docs/gaps-catalog.md). Only an explicit CLUSTERED
  constraint makes a later CLUSTERED index fail with 1902; a primary key that
  is clustered by default gives way to the index instead. Two clustered
  constraints are not rejected with 8112.
- **IGNORE_DUP_KEY = ON** on a unique index is unsupported.
- **Catalogs.**
  - `sys.indexes` and `sys.index_columns` come from the index catalog,
    including key constraints and keys-managed indexes (docs/gaps-catalog.md).
  - `sys.key_constraints`, constraint rows in `sys.objects` and `sp_pkeys`
    read `main.__msduck_keys` (docs/gaps-catalog.md).
- **Foreign keys.** DuckDB requires a native PRIMARY KEY or UNIQUE
  constraint on the referenced columns. A foreign key cannot reference a key
  over STRUCT storage, such as an NVARCHAR or DATETIMEOFFSET primary key.
- **ALTER TABLE.**
  - When DuckDB needs the drop committed first (see ALTER TABLE above),
    another session's writes can act between the commits. A duplicate
    inserted then makes recreating that unique index fail. The other
    indexes still return, and the failure is reported.
  - Inside a user transaction, those changes fail with DuckDB's message,
    and the transaction can only roll back.
  - A legacy index that neither catalog records keeps DuckDB's refusal.
  - ADD CONSTRAINT PRIMARY KEY/UNIQUE is the ALTER TABLE constraint work.
- **Other paths.**
  - Table variables (`DECLARE @t TABLE`) do not accept table constraints, and
    their keys are not managed.
  - Bulk loads do not translate duplicate-key errors.
  - Errors other than duplicates keep their existing messages, for example
    the NOT NULL violation (515) of a primary key column.
- **Transactions.** A duplicate key inside an explicit transaction still
  aborts DuckDB's transaction, so later statements fail until ROLLBACK. That
  is existing engine behavior.
- **RPC requests.** A duplicate key in an RPC request (sp_executesql,
  sp_prepexec, sp_execute) ends only the statement, as in a SQL batch:
  2627, 3621 and a DONEINPROC with the error flag. sp_prepexec then sends
  RETURNSTATUS 2627, the prepared handle and a DONEPROC without the error
  flag (captured in reference/gaps-rpc-procedures.json; see
  docs/gaps-rpc-procedures.md). The keys batch hook still records whether a
  batch is an RPC request, but the flag no longer changes the outcome.
- **MERGE.** A duplicate key in MERGE gets SQL Server's error but still
  ends the request. SQL Server ends only the statement.
- **1505** is not followed by 3621.

## References

- [CREATE INDEX](https://learn.microsoft.com/en-us/sql/t-sql/statements/create-index-transact-sql)
  (options, INCLUDE, filter grammar, DROP_EXISTING, IGNORE_DUP_KEY)
- [Create filtered indexes](https://learn.microsoft.com/en-us/sql/relational-databases/indexes/create-filtered-indexes)
- [Unique constraints and check constraints](https://learn.microsoft.com/en-us/sql/relational-databases/tables/unique-constraints-and-check-constraints)
  (a UNIQUE constraint allows one NULL per column)
- [CREATE TABLE](https://learn.microsoft.com/en-us/sql/t-sql/statements/create-table-transact-sql)
  (column and table constraints)
