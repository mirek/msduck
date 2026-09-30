# Current-time functions, server principals, computed columns, key clustering and table hints

These statements came from a tedious-based SQL Server test suite (issue #697).
They used to fail on msduck:

```sql
SELECT SYSDATETIMEOFFSET();
SELECT DATEDIFF_BIG(millisecond, '1970-01-01', GETUTCDATE());
SELECT name, default_database_name FROM sys.server_principals WHERE name = SUSER_SNAME();
CREATE TABLE ComputedProbe (id int NOT NULL IDENTITY(1, 1) PRIMARY KEY,
  name varchar(100) NULL, nameUpper AS UPPER(name) PERSISTED);
CREATE INDEX IX_ComputedProbe_nameUpper ON ComputedProbe (nameUpper);
CREATE TABLE NonclusteredProbe ([version] varchar(64) NOT NULL
  CONSTRAINT NonclusteredProbe_Pk PRIMARY KEY NONCLUSTERED);
SELECT MAX(id) FROM HintProbe WITH (READCOMMITTEDLOCK);
ALTER DATABASE CURRENT SET READ_COMMITTED_SNAPSHOT ON; -- already ON, other sessions connected
```

Expected behavior comes from `reference/tedious-compat-gaps.json`, captured
from SQL Server 2025 (see
[the reference notes](tedious-compat-gaps-reference.md)).

Two test files cover this behavior:

- `tests/tedious_compat_gaps.test.mjs` compares msduck with the capture
  through tedious.
- `tests/tedious_compat_gaps.rs` covers the in-process session and restart
  cases.

## Current-time functions

| Function | Type |
|---|---|
| `SYSDATETIMEOFFSET()` | `datetimeoffset(7)` |
| `SYSUTCDATETIME()` | `datetime2(7)` |
| `SYSDATETIME()` | `datetime2(7)` |
| `GETUTCDATE()` | `datetime` |
| `GETDATE()` | `datetime` |
| `CURRENT_TIMESTAMP` | `datetime` |

- **Declarations.** These functions return the captured types, and their
  results are not nullable.
- **Clock.** The root crate reads the system clock and the local UTC offset
  (`localtime_r`, or UTC on platforms without it) once per translated
  statement. Every call in that statement sees the same instant, so
  `CURRENT_TIMESTAMP = GETDATE()` holds.
- **Stored definitions.** Column `DEFAULT`s, `CHECK` constraints and view
  bodies read DuckDB's clock each time they are used. Local time there uses the
  UTC offset in effect when the definition was created, so a later
  daylight-saving change is not reflected.
- **Rounding.** `datetime` values are rounded to SQL Server's 1/300 second.
- **Local time.** `GETDATE`, `SYSDATETIME` and the offset of
  `SYSDATETIMEOFFSET` follow the server process's time zone. The reference and
  Docker images run in UTC.
- **Errors.** An argument fails with 174. `CURRENT_TIMESTAMP()` fails with 102.
- **DATEDIFF_BIG.** `DATEDIFF_BIG(millisecond, '1970-01-01', GETUTCDATE())`
  returns a `bigint`.
- **Known difference.** The `datetime2` and `datetimeoffset` descriptors lack
  the computed-column flag (TDS flags 0 instead of 32). This is true of every
  `datetime2`/`datetimeoffset` expression in msduck. Nullability matches.

## sys.server_principals, SUSER_SNAME and SYSTEM_USER

- **Declarations.** `sys.server_principals` exists in every database, with the
  14 captured column declarations.
- **Rows.** msduck has no login catalog, so the view lists:
  - `sa`, with principal_id 1 and SID `0x01`;
  - every other name that has logged in since the server started, with
    principal_id 256 and up in order of first login, and a 16-byte SID derived
    from the name.

  All of these are `SQL_LOGIN` rows with default database `master` and
  language `us_english`.
- **Default database.** SQL Server reports a login's configured default
  database, but msduck always reports `master`.
- **Name functions.** `SUSER_SNAME()`, `SUSER_NAME()` and `SYSTEM_USER` return
  the authenticated login as a nullable `nvarchar(128)`, like
  `ORIGINAL_LOGIN()`. msduck has no impersonation, so they always agree.
- **SID lookup.** `SUSER_SNAME(sid)` looks the SID up in
  `sys.server_principals` and returns NULL when no login matches.
- **Unsupported.** `SUSER_NAME(id)` with an argument fails. `SUSER_SNAME`,
  `SUSER_NAME` and `SYSTEM_USER` in a column `DEFAULT`, `CHECK` or view body
  also fail. msduck cannot evaluate them for the session that later uses the
  definition.
- **Known difference.** An unaliased `SYSTEM_USER` column is named
  `SYSTEM_USER`; SQL Server leaves it unnamed.

## Computed columns

`CREATE TABLE` accepts `name AS expression [PERSISTED] [NOT NULL]`.

- **Storage.** The column becomes a DuckDB VIRTUAL generated column, even when
  `PERSISTED`, because DuckDB cannot store generated columns. Reads, filters
  and indexes evaluate the expression.
- **Declared type.** msduck infers the declared type from the expression over
  the other column declarations before creating the table. For example,
  `UPPER(varchar(100))` is `varchar(100)` and `CAST(a AS varchar(10)) + 'x'`
  is `varchar(11)`. The type is recorded like any declaration.
- **Catalog.** `sys.columns.is_computed` and `COLUMNPROPERTY(..., 'IsComputed')`
  report computed columns. Result descriptors carry the computed flag.
- **Writes:**
  - `INSERT` with or without a column list skips computed columns.
  - Naming one as an INSERT or UPDATE target fails with 271.
- **Captured errors:**

  | Case | Error |
  |---|---|
  | A computed column that references another computed column | 1759 |
  | Unknown column | 207 |
  | Non-deterministic `PERSISTED` | 4936 |

- **Explicit refusals.** msduck rejects these where SQL Server accepts them:
  - Non-persisted non-deterministic expressions (`GETDATE()`, `NEWID()`,
    `RAND()`, ...). msduck binds current-time functions to the statement's
    clock, so the stored value would never change.
  - Expressions over, or results of, types whose DuckDB storage is a carrier
    representation: `nchar`/`nvarchar`, `datetime2`, `datetimeoffset`, `time`,
    `money`, binary types and `sql_variant`.
- **Other known differences:**
  - `PERSISTED NOT NULL` is reported as nullable, because DuckDB cannot
    constrain a generated column.
  - `sys.computed_columns` is not implemented.
  - Index checks for non-deterministic columns (2729) are not applied.
  - `ALTER TABLE ... ADD name AS expression` is not supported.

## CLUSTERED and NONCLUSTERED

`PRIMARY KEY` and `UNIQUE` accept `CLUSTERED` or `NONCLUSTERED`:

- at column level;
- at table level, with `ASC`/`DESC` key columns.

`CREATE [UNIQUE] NONCLUSTERED INDEX` is accepted too. msduck stores every table
and index the same way, so it drops the keywords before parsing.

Table-level `PRIMARY KEY (...)` and `UNIQUE (...)` constraints now work at all.
Before this change, the key columns were rewritten with `NULLS FIRST`, which
DuckDB rejects. Key order (`ASC`/`DESC`) has no effect.

Known differences:

- msduck does not record which index SQL Server would make clustered.
- Two `CLUSTERED` constraints are not rejected with 8112.
- `CREATE [UNIQUE] CLUSTERED INDEX` still fails to parse.
- `WITH (FILLFACTOR = ...)` on a constraint is not supported.
- `ALTER TABLE ... ADD [CONSTRAINT name] PRIMARY KEY | UNIQUE ...` is not
  supported, with or without the keywords. This predates the change.
- `sys.indexes` still refuses tables with constraint-backed indexes.

## Table hints

- **Accepted hints.** msduck serializes each session's statements, so it
  accepts these and ignores them:
  - `NOLOCK`, `READUNCOMMITTED`, `READCOMMITTED`, `READCOMMITTEDLOCK`;
  - `REPEATABLEREAD`, `SERIALIZABLE`, `HOLDLOCK`;
  - `UPDLOCK`, `XLOCK`, `ROWLOCK`, `PAGLOCK`, `TABLOCK`, `TABLOCKX`;
  - `READPAST`, `NOWAIT`, `FORCESCAN`, `FORCESEEK`;
  - `INDEX(...)` and `INDEX = n`.
- **Where hints can appear.** Hints work in `SELECT` sources, joins, CTEs and
  subqueries, and on `UPDATE`, `DELETE` and `UPDATE ... FROM` sources. The
  legacy form without `WITH`, `t (NOLOCK)`, works when every name in the list
  is an accepted hint.
- **INSERT targets.** sqlparser cannot parse hints on an INSERT target
  (`INSERT INTO t WITH (TABLOCK) ...`). msduck removes an accepted hint list
  there before parsing.
- **Compile-time errors.** msduck rejects these before execution with the
  captured error:

  | Hint | Error |
  |---|---|
  | Unknown hint | 321 |
  | Two different isolation levels, such as `NOLOCK, SERIALIZABLE` | 1047 |
  | `NOLOCK` or `READUNCOMMITTED` on an UPDATE or DELETE target | 1065 |
  | `NOEXPAND` | 8171 |
  | `SNAPSHOT` | 367 |

- **Known differences:**
  - `INDEX(...)` does not check that the index exists (SQL Server: 308).
  - `FORCESEEK` never fails for want of a seekable index (8622).
  - `NOLOCK` on an INSERT target fails to parse rather than returning 1065.

## ALTER DATABASE to an unchanged value

Setting `READ_COMMITTED_SNAPSHOT` to its current value no longer needs the
other sessions gone. The statement completes at once, even `WITH NO_WAIT`, as
SQL Server does. See [ALTER DATABASE](alter-database-sessions.md).
