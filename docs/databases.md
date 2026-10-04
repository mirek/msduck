# Databases

msduck serves SQL Server databases from one DuckDB instance. The primary
database, the file given to `--database` or `/var/opt/mssql/data/msduck.duckdb`
in the container, is exposed as `master` with `database_id` 1. Each user
database is a separate DuckDB catalog attached to the same instance. User
databases get IDs from 5 upwards, the first ID SQL Server gives user databases,
and IDs are not reused after a drop.

`src/database_catalog.rs` owns this mapping. Its registry is the table
`main.__msduck_databases` in the primary catalog. It records each database's
name, the lower-case key used for case-insensitive lookup, the ID, the file name,
the create date and whether publication finished. The key uses the captured
`SQL_Latin1_General_CP1_CI_AS` lower-case mapping of UTF-16 units, not generic
Unicode casing, after dropping trailing spaces, so `İ`, `i` and `i ` name the
same database. The SQL helpers behind
database ID lookups apply the same mapping with `translate`. This approximates the
collation's comparison; other equivalences of its sort weights are not modelled.
A new database is listed in `sys.databases` and selectable only after its
catalog objects and every catalog's `sys.databases` view are in place. A file-backed server re-attaches every registered database
on startup. If one cannot be attached, the server logs the failure and still
starts. A missing file is never recreated, so lost data is not replaced by an
empty database. An unavailable database stays registered and keeps its name,
but it is not listed or selectable. Dropping it removes the registration and
any remaining files.

## Storage

A user database lives next to the primary file, which is resolved to its
canonical path, links included, at startup, as
`<primary file name>.<database_id>.<name fragment>.duckdb`, for example
`msduck.duckdb.5.sales.duckdb`. The full primary file name keeps servers apart
whose files differ only in extension, such as `tenant.db` and `tenant.duckdb`. The ID makes the name unique. The fragment is the
lower-case name with ASCII letters, digits, `_` and `-` kept and every other
UTF-8 byte written as `%XX`. It is cut at a whole character after 64 bytes, so
long non-ASCII names stay within file-name limits. A primary file name longer than 64
bytes is cut the same way and followed by `~` and a 64-bit FNV-1a hash of the
full name, so servers whose file names share a prefix do not collide. A `\` in
the primary file name, legal on Unix, is written as `%5C`, and a `%` as `%25`. The registry
is ordinary SQL data, so a stored file name is used only if it is a single
file-name component that ends with the `.<database_id>.<fragment>.duckdb`
suffix generated for that row's ID and name. Any other value, such as an
absolute path, `..`, master's file or another database's file, makes the
database unavailable, and neither recovery nor DROP touches the file. Any
prefix is accepted, so renaming the primary file keeps its databases. `My App` is stored as
`msduck.duckdb.5.my%20app.duckdb`. The registry records the file name.
Creation never adopts an existing file or WAL: it leaves it in place and
moves on to the next ID, for example when another primary now uses a renamed
primary's old file name. A database file or
WAL that is a symbolic link, or not a regular file, is never opened: recovery
leaves the database unavailable and CREATE fails. DROP removes the link itself.
The check precedes the open, so it does not guard against a process that swaps
files in the data directory concurrently. Dropping a database
detaches it and deletes its checkpointed WAL, then its file, before the
registration. A database that was not attached may have changes only in its
WAL, so its file is deleted first; deleting that file commits the drop, and a
WAL that cannot be deleted is removed with the stale registration later. If a
deletion
fails, the database stays registered and DROP reports the error, so a retry can
finish. Before detaching, DROP creates and removes a uniquely named `<file>.drop-*` probe; if
the directory does not allow that, DROP fails and the database stays attached.
If a later step fails while the file is intact, the database is attached again
when DuckDB allows it. DROP hides the database before it detaches or deletes
anything. Once the files are gone, DROP succeeds even if removing the
registration fails: a hidden registration without a catalog or files is stale,
and CREATE of that name or the next startup removes it. A hidden registration
whose files remain, from a failed CREATE or an interrupted DROP, is never
published by recovery; it stays unavailable until DROP removes it. If recovery
cannot detach a partially attached catalog, the server does not start. A CREATE that fails after its file exists cleans up the same way.

An in-memory server keeps its user databases in a private (`0700` on Unix)
temporary directory, which is
removed when the last `Server` or `server::Connection` handle is dropped. A
`Session` does not hold the catalog yet, so dropping an in-memory server while a
session is still open removes the files under that session. The statements task
gives `Session` the catalog, which it needs for `USE`. A process that crashes can leave this
directory behind in the system temporary directory.

## Per-database catalog objects

msduck's `sys` views, catalog tables and helper macros belong to each DuckDB
catalog, and views bind to the catalog they are defined in. Every user database
therefore gets its own `dbo` schema and the same catalog objects as the primary
database, so `sys.tables`, `sys.columns`, identities and defaults describe that
database only. Native scalar functions belong to the whole DuckDB instance and
cannot be registered a second time. The catalog therefore bootstraps each user
database file in a short-lived DuckDB instance of its own before the server
instance attaches it. This runs on every attach, so catalog objects follow the
running msduck version.

## sys.databases and helpers

`sys.databases` exists in every database and lists `master` and every attached
user database:

| Column | Type | Value |
| --- | --- | --- |
| `name` | nvarchar | database name as created |
| `database_id` | int | 1 for `master`, 5 and up for user databases |
| `source_database_id` | int | NULL |
| `create_date` | datetime | registry create date; SQL Server's fixed date for `master` |
| `compatibility_level` | tinyint | 160 |
| `collation_name` | nvarchar | `SQL_Latin1_General_CP1_CI_AS` |
| `user_access`, `user_access_desc` | tinyint, nvarchar | 0/1/2, `MULTI_USER`/`SINGLE_USER`/`RESTRICTED_USER` as set by ALTER DATABASE |
| `is_read_only` | bit | 0 |
| `state`, `state_desc` | tinyint, nvarchar | 0, `ONLINE` |
| `snapshot_isolation_state`, `snapshot_isolation_state_desc` | tinyint, nvarchar(60) | 0 `OFF`, 1 `ON`, 2 `IN_TRANSITION_TO_OFF`, 3 `IN_TRANSITION_TO_ON`, as set by `ALTER DATABASE ... SET ALLOW_SNAPSHOT_ISOLATION`; always 1 for `master` |
| `is_read_committed_snapshot_on` | bit | as set by ALTER DATABASE; 0 for `master` |
| `recovery_model`, `recovery_model_desc` | tinyint, nvarchar | 3, `SIMPLE` |

The other SQL Server columns are not provided. Result metadata comes from these
DuckDB types (`nvarchar(max)`, `datetime2`), not from SQL Server's `sysname`,
bounded `nvarchar` and `datetime` declarations. Task
`sys-databases-descriptors-v1` adds pinned descriptors. Each database also has the
macros `__msduck_db_id(name)`, `__msduck_db_name(id)` and
`__msduck_current_db_name()`, which the engine can use for `DB_ID` and
`DB_NAME`.

## Sessions

`Catalog::select` makes a database the connection's DuckDB default catalog and
restores the `dbo` schema, because DuckDB's `USE` resets the schema to `main`.
A session holds a `Use` guard from `Catalog::enter` for its current database;
while any session uses a database, DROP refuses it. A session that sets a
database to `SINGLE_USER` also holds a `Hold` guard for it; see
[ALTER DATABASE options, sessions and @@SPID](alter-database-sessions.md). A session starts in
`master`, then selects the LOGIN7 database. RESETCONNECTION returns a reset
session to the login database.

## T-SQL statements

Expected tokens and errors below were captured from SQL Server 2025
(`mcr.microsoft.com/mssql/server:2025-latest`) with tedious.

- `CREATE DATABASE name` completes with DONE command 203. `COLLATE` is accepted
  only for `SQL_Latin1_General_CP1_CI_AS`; file, containment and other options
  are refused. `IF NOT EXISTS`, which sqlparser accepts, skips an existing
  database. Inside a user transaction it fails with 226 (state 5).
- `DROP DATABASE [IF EXISTS] a, b` completes with DONE command 204. Inside a
  user transaction it fails with 574 (state 0). A database in use fails with
  3702: state 3 when the dropping connection uses it, state 4 when another
  session does. Like SQL Server, a multi-name DROP is not atomic: it drops every
  database it can and reports one error per failed name.
- `USE name` sends ENVCHANGE type 1 (new and old names), INFO 5701 (state 1,
  class 0) `Changed database context to 'name'.`, ENVCHANGE type 7 (the
  collation) and DONE command 226. An unknown database fails with 911.
- `DB_NAME()` and `DB_ID()` follow the session database; `DB_NAME(id)` and
  `DB_ID(name)` use the catalog helpers. Both are nullable; `DB_NAME` is
  nvarchar(128) and `DB_ID` is a smallint on the wire, as captured (the
  documentation says int). More than one argument fails with 189.
- LOGIN7 accepts any existing database, case-insensitively, and the login
  ENVCHANGE reports its stored name. An unknown database fails the login with
  4060 (state 1, class 11) followed by 18456.
- In `database.schema.object` relation names, the database resolves through the
  catalog, published databases only, case-insensitively. An unknown one fails
  with 208 `Invalid object name '...'`. `database..object` names the `dbo`
  schema. A name in the current database is bound as `schema.object`, so
  declared metadata and storage coercions apply. Other databases are covered in
  [Cross-database names](#cross-database-names).
- A `USE` inside a top-level `sp_executesql` RPC, as tedious `execSql` sends,
  persists for the connection, as captured from SQL Server. Only nested
  `EXEC sp_executesql` inside a batch reverts it, and msduck does not run that
  form.

The catalog uses SQL Server diagnostics for:

- a duplicate name, or one of `master`, `tempdb`, `model` or `msdb`: 1801;
- selecting an unknown database: 911;
- dropping an unknown database: 3701;
- dropping `master`: 3708 (state 4);
- dropping a database in use: 3702.

DuckDB's own catalog names (`memory`, `system`, `temp`) and the primary
catalog's DuckDB name are rejected with an msduck error.

## Cross-database names

Queries, `INSERT`, `UPDATE`, `DELETE`, variable assignments and
`IF`/`WHILE` conditions can use another database's tables and views through
three-part names, as in SQL Server. `reference/cross-database.json`,
captured by `scripts/capture-cross-database.mjs` from the pinned SQL Server
image, holds the expected rows, descriptors, errors and completion tokens,
and `tests/compat/cross_database.test.mjs` replays it. Every database is a
separate DuckDB catalog, so the engine places each statement in one of them:

- Reads (queries, conditions and assignments) run in the session's database,
  where functions that depend on the current database, such as `OBJECT_ID`,
  `@@DBTS` and user functions, bind as in SQL Server. Result descriptors and
  operand types (LEN, concatenation, comparisons of nvarchar columns) of
  another database's relations come from that database's catalog objects.
- Another database's catalog views (`b.sys.columns`,
  `b.INFORMATION_SCHEMA.TABLES`; `sys.databases` excepted), and its views
  whose definitions read them, directly or through other views, describe it
  only from inside it, so a query that reads them and nothing else runs in that
  database. Combined with other databases' objects or user functions, it
  fails with 40515 `unsupported cross-database statement: it reads catalog
  views of database '...' together with objects or user functions of other
  databases`. DML may read another database's catalog views only when it
  writes that database and reads no third one; otherwise it fails with 40515
  `unsupported cross-database statement: it reads catalog views of another
  database than the one it writes`. DML that writes another database may not read the
  session database's catalog views, or its views over them, either (40515 `... and reads catalog views
  of the session's database`).
- INSERT, UPDATE and DELETE that write another database run with it as
  DuckDB's default catalog. Features then treat it as the current database:
  declared column types and nvarchar storage, identity, defaults, triggers,
  constraints and snapshot isolation checks see its catalog objects, as they
  would after `USE`. Relations of the session's database and of others are
  read through their own catalogs. `DB_NAME()` and `DB_ID()` still name the
  session's database, as captured, and `SCOPE_IDENTITY()` reports the insert.
  Trigger bodies run in their own database. DML that writes the current
  database and reads others runs in the current database.
- A missing object of another database fails with 208 `Invalid object name
  'b.schema.object'.`, as captured.
- Before a statement uses another database, msduck applies the checks `USE`
  makes and keeps the database in use until the statement ends, so `DROP
  DATABASE` meanwhile fails with 3702. A database another session holds in
  `SINGLE_USER` fails with 924 (state 1, class 14) `Database '...' is
  already open and can only have one user at a time.`, as captured.
- Preparation (`sp_prepare`) binds such statements the same way without
  running them, and RPC parameters work as in the current database.
- A restored database is an ordinary user database and is read the same way.

DuckDB writes one attached database per transaction. SQL Server lets one
transaction modify several databases; msduck refuses the statement that would
modify a second database with error 40515 (state 1, class 16)
`unsupported cross-database transaction: database 'b' cannot be modified in a
transaction that has already modified database 'a'; a transaction may write
only one database` (without the names when DuckDB's diagnostic does not
identify the two databases unambiguously). DuckDB has already aborted the
transaction, so its work is rolled back at once and the transaction is
doomed: outside `TRY` the batch ends and rolls it back; inside `TRY` the
handler sees `XACT_STATE()` -1 and must roll it back. Nothing of it commits. A single statement always writes one
database. Each statement outside an explicit transaction commits on its own,
so consecutive statements may write different databases.

Remaining limits, refused explicitly unless noted:

- DDL on another database's objects (`CREATE TABLE b.dbo.t`, `DROP TABLE`,
  `CREATE INDEX`, `ALTER TABLE`) and `MERGE` with another database's objects
  fail with 40515 `unsupported reference to ... in another database; USE ...
  first`, as do `BULK INSERT` targets, computed column definitions and inline
  function bodies that name another database. `TRUNCATE TABLE b.dbo.t` fails
  with `unsupported TRUNCATE target`.
- A statement that writes another database and also reads a temporary table
  or table variable fails with 40515 `unsupported cross-database statement`:
  temporary objects belong to the session's database.
- A statement that writes another database and has `OUTPUT ... INTO` fails
  with 40515 `unsupported cross-database statement: OUTPUT INTO in a statement
  that writes database '...'`: the destination belongs to the session's
  database, and the statement may write only one. Plain `OUTPUT` works, and
  `OUTPUT INTO` works in statements that write the current database.
- User functions bind in the session's database, so a write to another
  database that calls a schema-qualified function (`dbo.f(...)`) fails with
  40515 `unsupported cross-database statement: it writes database '...' and
  calls functions of the session's database`.
- Built-in functions that depend on the current database, such as
  `OBJECT_ID` or `@@DBTS`, resolve in the written database in DML that
  writes another database, and in the other database in reads of its catalog
  views.
- Calls to procedures and functions in another database, and synonyms, are not
  resolved.

## Not yet supported

- `tempdb`, `model` and `msdb`;
- `ALTER DATABASE` options other than `READ_COMMITTED_SNAPSHOT`, user access
  ([ALTER DATABASE](alter-database-sessions.md)) and `ALLOW_SNAPSHOT_ISOLATION`
  ([transactions](gaps-transactions.md#allow_snapshot_isolation-and-snapshot-transactions)),
  and `CREATE DATABASE` file or non-server collation options;
- DDL on another database's objects, and transactions that write more than one
  database (see [Cross-database names](#cross-database-names));
- SQL Server resolves `USE` and three-part names when it compiles a batch, so an
  unknown database aborts the whole batch before any statement runs. msduck
  resolves them per statement, so earlier statements in the batch still run,
  and a batch may `USE` a database it created earlier in the same batch;
- four-part (server-qualified) names;
- name equality beyond the captured case map and trailing spaces, such as width
  equivalence.
