# tedious compatibility gaps reference

`scripts/capture-tedious-compat-gaps.mjs` records how SQL Server 2025
(17.0.4065.4, the pinned reference image) handles the statements from issue
#697. These statements came from a tedious-based test suite and failed on
msduck.

`reference/tedious-compat-gaps.json` keeps 124 observations from two fresh
containers, which were identical. Each observation keeps descriptors, rows,
ERROR and INFO tokens and the raw DONE bodies (status, CurCmd, row count).

```sh
node scripts/capture-tedious-compat-gaps.mjs            # fresh capture, written to artifacts/
node scripts/capture-tedious-compat-gaps.mjs --check    # validate the retained fixture offline
```

## Reductions

Some values depend on the environment, so the capture reduces them:

- **Clock values.** The capture records only the descriptors (from `WHERE 1=0`
  queries) and comparisons computed inside the server.
- **Time zone.** The container runs with `TZ=UTC`.
- **Login SIDs and dates.** The capture records their types, byte lengths and
  ordering, not the values.
- **Generated constraint names.** Names like `PK__Computed__3213E83F…` end in a
  random suffix. The capture records them as `PK__<generated>` or
  `UQ__<generated>`.
- **Probe login password.** The capture redacts it from the recorded SQL.

A statement that is still running after 5 seconds is cancelled with an
ATTENTION and recorded as `waited: true`.

## Current-time functions

| Function | TDS type | Base type |
|---|---|---|
| `SYSDATETIMEOFFSET()` | DATETIMEOFFSETN scale 7 | `datetimeoffset(7)` |
| `GETUTCDATE()` | DATETIME (not nullable) | `datetime` |
| `SYSUTCDATETIME()` | DATETIME2N scale 7 | `datetime2(7)` |
| `GETDATE()` | DATETIME | `datetime` |
| `SYSDATETIME()` | DATETIME2N scale 7 | `datetime2(7)` |
| `CURRENT_TIMESTAMP` | DATETIME | `datetime` |

- All six columns have flags 32: they are not nullable and are not identity
  columns.
- `DATEDIFF_BIG(millisecond, '1970-01-01', GETUTCDATE())` returns nullable
  `bigint` (INTN, length 8).
- With `TZ=UTC`:
  - `DATEPART(TZOFFSET, SYSDATETIMEOFFSET())` is 0.
  - The UTC and local forms agree within one second.
  - `CURRENT_TIMESTAMP = GETDATE()` holds.
- `GETUTCDATE()` has `datetime` precision: its milliseconds end in 0, 3 or 7.
- Passing an argument fails with error 174 state 1 ("The getutcdate function
  requires 0 argument(s).").
- `CURRENT_TIMESTAMP()` fails with error 102 near `)`.

## sys.server_principals and SUSER_SNAME

- **Declarations.** `sys.server_principals` has 14 columns: `name`,
  `principal_id`, `sid`, `type`, `type_desc`, `is_disabled`, `create_date`,
  `modify_date`, `default_database_name`, `default_language_name`,
  `credential_id`, `owning_principal_id`, `is_fixed_role` and `tenant_id`. The
  fixture keeps the complete declarations and the `SELECT *` descriptors.
- **The report's query.** `select name, default_database_name from
  sys.server_principals where name = suser_sname()` returns `sa`, `master`. The
  `name` column is NVARCHAR(128), not nullable. `default_database_name` is
  NVARCHAR(128), nullable.
- **The `sa` row:**
  - `principal_id` 1, `type` `S`, `type_desc` `SQL_LOGIN`;
  - `is_disabled` false, `default_language_name` `us_english`;
  - `credential_id` NULL, `owning_principal_id` NULL, `is_fixed_role` false;
  - `sid` is a 1-byte varbinary.
- **Name functions.** `SUSER_SNAME()`, `SUSER_NAME()` and `SYSTEM_USER` are
  NVARCHAR(128) and not nullable (flags 32). `ORIGINAL_LOGIN()` is
  NVARCHAR(4000). All four return the login name.
  - `SUSER_SNAME(0x01)` returns `sa`.
  - An unknown SID returns NULL.
- **A second login.** The capture creates `probe_login` with
  `DEFAULT_DATABASE=[probe_db]`. That login's session:
  - gets `probe_login`, `probe_db` from the report's query;
  - sees only `sa` and itself among the `S`, `U` and `G` principals;
  - reports `SUSER_SNAME()` = `SYSTEM_USER` = `probe_login`.

## Computed columns

The report's DDL succeeds:

```sql
create table ComputedProbe (id int not null identity(1, 1) primary key,
  name varchar(100) null, nameUpper as upper(name) persisted);
create index IX_ComputedProbe_nameUpper on ComputedProbe (nameUpper);
```

- **Completions.** CREATE TABLE ends with DONE CurCmd 198 and CREATE INDEX with
  CurCmd 200.
- **Reads and writes:**
  - An INSERT with a column list, or without one, skips computed columns.
  - Reads evaluate the expression; a NULL input gives NULL.
  - UPDATE of the source column recomputes the value.
  - WHERE can filter on the computed column.
- **Writing a computed column** fails with error 271 state 1, both as an INSERT
  target and as an UPDATE target.
- **Declared types and nullability:**
  - The computed column takes its type from the expression: `upper(varchar(100))`
    is `varchar(100)` and `a + b` is `int`.
  - `CAST(a AS varchar(10)) + 'x'` is `varchar(11)`.
  - The result is nullable even when every input is NOT NULL.
  - `PERSISTED NOT NULL` makes it not nullable.
- **Catalog.** `sys.columns.is_computed` is 1. `sys.computed_columns` shows the
  normalized definition, for example `(upper([name]))` or `([a]+[b])`, and
  `is_persisted`. `COLUMNPROPERTY(..., 'IsComputed')` returns 1.
- **Indexes.** A deterministic non-persisted computed column (`total AS a + b`)
  can be indexed too.
- **Errors:**

  | Case | Error |
  |---|---|
  | Non-deterministic `PERSISTED` (`GETDATE()`) | 4936 |
  | Index on a non-deterministic column | 2729 |
  | A computed column that references another computed column | 1759 |
  | Unknown column | 207 |

  A non-persisted `GETDATE()` column is accepted.

## CLUSTERED and NONCLUSTERED keys

- **Accepted forms.** SQL Server accepts `PRIMARY KEY` and `UNIQUE` with
  `CLUSTERED` or `NONCLUSTERED`:
  - at column level;
  - at table level, with `ASC`/`DESC` key columns and `WITH (FILLFACTOR = 90)`;
  - in `ALTER TABLE ... ADD CONSTRAINT`.

  It also accepts `CREATE CLUSTERED INDEX` and
  `CREATE [UNIQUE] NONCLUSTERED INDEX`.
- **The report's table.** `NonclusteredProbe`, whose PK is on
  `[version] ... primary key nonclustered`, becomes a `HEAP` (index 0) with a
  `NONCLUSTERED` unique primary-key index 2. A duplicate key fails with 2627.
- **Which index is clustered:**
  - Without a keyword, the primary key is `CLUSTERED` (index 1) and `UNIQUE` is
    `NONCLUSTERED`.
  - `UNIQUE CLUSTERED` makes the unique constraint index 1. The nonclustered
    primary key is then index 2.
- **Two clustered constraints** fail with 8112 state 0.

## Table hints

- **Accepted.** These run unchanged and return the same rows:
  - `NOLOCK`, `READCOMMITTEDLOCK`, `UPDLOCK`, `ROWLOCK`, `HOLDLOCK`;
  - `READUNCOMMITTED`, `READCOMMITTED`, `REPEATABLEREAD`, `SERIALIZABLE`;
  - `TABLOCK`, `TABLOCKX`, `PAGLOCK`, `XLOCK`, `READPAST`, `NOWAIT`,
    `FORCESCAN`;
  - `INDEX(0)`, `INDEX(1)` and `INDEX = 1`;
  - lists such as `UPDLOCK, ROWLOCK, HOLDLOCK`.
- **Where hints can appear.** Hints work:
  - after aliases;
  - in joins, CTEs and subqueries;
  - on UPDATE, DELETE and INSERT targets;
  - in `UPDATE ... FROM`;
  - inside an explicit transaction.

  The legacy form without `WITH`, `FROM t (NOLOCK)`, also works.
- **Errors:**

  | Hint | Error |
  |---|---|
  | `FORCESEEK` with no usable seek | 8622 |
  | `NOEXPAND` on a table | 8171 |
  | `SNAPSHOT` | 367 |
  | `NOLOCK` on an UPDATE target | 1065 |
  | Unknown hint | 321 (`"bogus" is not a recognized table hints option.`) |
  | `NOLOCK, SERIALIZABLE` | 1047 |
  | Missing index | 308 |

## ALTER DATABASE to an unchanged value

These results come from three sessions connected to `probe_db` (the altering
session and two idle ones) after
`SET READ_COMMITTED_SNAPSHOT ON WITH ROLLBACK IMMEDIATE`.

- **Unchanged value.** These all complete at once with DONE status 0, CurCmd
  215, and no messages:
  - `ALTER DATABASE CURRENT SET READ_COMMITTED_SNAPSHOT ON` (the report's
    statement);
  - the same statement with the database name, or `WITH NO_WAIT`;
  - `SET MULTI_USER` with or without `NO_WAIT`;
  - `SET READ_COMMITTED_SNAPSHOT ON, MULTI_USER`.
- **Changed value.** `SET READ_COMMITTED_SNAPSHOT OFF` without a termination
  clause waits. It was cancelled after 5 seconds: error 5069, DONE status 2
  CurCmd 215, then the attention acknowledgement (status 32). The option stays
  ON and the other sessions keep working.
- **Changed value with `WITH NO_WAIT`** fails with 5070 and 5069.
- **Changed value, no other sessions.** It succeeds at once.
