# Databases

msduck serves SQL Server databases from one DuckDB instance. The primary
database, the file given to `--database` or `/var/opt/mssql/data/msduck.duckdb`
in the container, is exposed as `master` with `database_id` 1. Each user
database is a separate DuckDB catalog attached to the same instance. User
databases get IDs from 5 upwards, the first ID SQL Server gives user databases,
and IDs are not reused after a drop.

`src/database_catalog.rs` owns this mapping. Its registry is the table
`main.__msduck_databases` in the primary catalog. It records each database's
name, the lower-case key used for case-insensitive lookup, the ID, the file name
and the create date. A file-backed server re-attaches every registered database
on startup. If one cannot be attached, the server logs the failure and still
starts. A missing file is never recreated, so lost data is not replaced by an
empty database. An unavailable database stays registered and keeps its name,
but it is not listed or selectable. Dropping it removes the registration and
any remaining files.

## Storage

A user database named `Sales` lives next to the primary file as
`<primary stem>.sales.duckdb`, for example `msduck.sales.duckdb`. File names use
the lower-case name. ASCII letters, digits, `_` and `-` are kept and every other
UTF-8 byte becomes `%XX`, so `My App` is stored as `msduck.my%20app.duckdb`.
Creation refuses to adopt an existing file with that name. Dropping a database
detaches it and deletes its file and WAL.

An in-memory server keeps its user databases in a temporary directory, which is
removed when the server is dropped. A process that crashes can leave this
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

The other SQL Server columns are not provided. Each database also has the
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
