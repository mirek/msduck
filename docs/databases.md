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

A user database lives next to the primary file as
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
Creation refuses to adopt an existing file or WAL with that name, and leaves
it in place. A database file or
WAL that is a symbolic link, or not a regular file, is never opened: recovery
leaves the database unavailable and CREATE fails. DROP removes the link itself.
The check precedes the open, so it does not guard against a process that swaps
files in the data directory concurrently. Dropping a database
detaches it and deletes its file and WAL before the registration. If a deletion
fails, the database stays registered and DROP reports the error, so a retry can
finish. Before detaching, DROP creates and removes a `<file>.drop` probe; if
the directory does not allow that, DROP fails and the database stays attached.
If a later step fails while the file is intact, the database is attached again
when DuckDB allows it. DROP hides the database before it detaches or deletes
anything. Once the files are gone, DROP succeeds even if removing the
registration fails: a hidden registration without a catalog or files is stale,
and CREATE of that name or the next startup removes it. A recovery that cannot
detach a partially attached catalog hides it the same way. A CREATE that fails after its file exists cleans up the same way.

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
| `user_access`, `user_access_desc` | tinyint, nvarchar | 0, `MULTI_USER` |
| `is_read_only` | bit | 0 |
| `state`, `state_desc` | tinyint, nvarchar | 0, `ONLINE` |
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
The server selects the login database before creating the session, and
RESETCONNECTION returns a reset session to it.

The catalog uses SQL Server diagnostics for:

- a duplicate name, or one of `master`, `tempdb`, `model` or `msdb`: 1801;
- selecting an unknown database: 911;
- dropping an unknown database: 3701;
- dropping `master`: 3708.

DuckDB's own catalog names (`memory`, `system`, `temp`) and the primary
catalog's DuckDB name are rejected with an msduck error.

## Not yet supported

This layer does not execute T-SQL. Wiring `CREATE DATABASE`, `DROP DATABASE`,
`USE`, `DB_NAME` and `DB_ID` into the engine is task
`multi-database-statements-v1`, as is accepting a non-`master` database in
LOGIN7. Also not supported:

- `tempdb`, `model` and `msdb`;
- `ALTER DATABASE` and `CREATE DATABASE` file or collation options;
- three-part names that use `master` (the primary catalog's DuckDB name differs);
- detecting whether another session is still using a database before it is dropped.
