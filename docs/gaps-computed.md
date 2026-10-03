# Computed columns over Unicode and JSON, and session-dependent defaults

Issue #718 reported two statements that failed on msduck v0.2.4 with 40515:

```sql
CREATE TABLE items (body nvarchar(max) NOT NULL,
  value AS (CONVERT(nvarchar(200), JSON_VALUE(body, N'$.value'))) PERSISTED);
CREATE TABLE items (value nvarchar(100)
  DEFAULT (CONVERT(nvarchar(100), SESSION_CONTEXT(N'foo'))));
```

Both now work. The `computed` extension feature implements them through the
hooks in [extension hooks](extension-hooks.md):

- runtime: `src/engine/ext/computed.rs` and `src/engine/ext/computed/`;
- syntax and rewrite rules: `crates/msduck-sql/src/dialect/ext/computed/`.

`src/computed_columns.rs` keeps the existing computed-column rules described in
[the earlier computed-column work](tedious-compat-gaps.md#computed-columns).
Its refusal of carrier-typed expressions is lifted for the cases below.

## Evidence

`scripts/capture-gaps-computed.mjs` defines 15 cases. Each case runs in a
fresh database, through a connection that reports the workstation
`computed-host` and the application `computed-app`.

`reference/gaps-computed.json` holds the SQL Server results of those cases.
They were captured twice, with identical results, from the pinned reference
image `mcr.microsoft.com/mssql/server:2025-latest@sha256:86cc6144…`
(17.0.4065.4, RTM-CU7). The first 10 cases were originally captured from
`2022-latest` (16.0.4236.2); the 2025 capture of those cases is identical.

Three test files cover this work:

- `tests/compat/computed.test.mjs` runs every case through tedious and
  compares errors, rows and result descriptors with the capture. The
  differences listed under [Remaining differences](#remaining-differences) are
  asserted explicitly. Separate tests cover two sessions, sp_executesql,
  RESETCONNECTION and indexed filters.
- `tests/gaps_computed.rs` checks stored values, catalog rows and error numbers
  in process, including a restart of a file-backed database.
- Unit tests in `crates/msduck-sql/src/dialect/ext/computed/session.rs` cover
  the rewrite rules.

## Computed columns over carrier-stored columns

Before this change, msduck refused a computed column over, or producing, a
type whose DuckDB storage is a carrier representation: `nvarchar`, `nchar`,
`datetime2`, `money`, binary and similar types. These columns now work, PERSISTED
or not, including:

- `JSON_VALUE`;
- `CONVERT(nvarchar(n), ...)`;
- concatenation;
- `UPPER`, `LEFT`, `RTRIM` and `LEN`;
- `DATEADD` over `datetime2`;
- arithmetic over `money`;
- `DATALENGTH` over binary types.

The `statement` hook processes a `CREATE TABLE` before the engine does:

1. **Validation.** It applies the existing reference and determinism rules:

   | Case | Error |
   |---|---|
   | Unknown column | 207 |
   | A reference to another computed column | 1759 |
   | Non-deterministic `PERSISTED` | 4936 |

2. **Declared type.** It infers the declared type from the expression over
   the other column declarations, as before. When the inference has no answer
   (for example `LEN` over `nvarchar(max)`), it uses the backend type of the
   lowered expression if that type maps to exactly one SQL Server type, such
   as `bigint`. Otherwise the column is refused.
3. **Lowering.** It lowers the expression through the ordinary query
   pipeline, as `SELECT expr FROM (SELECT CAST(NULL AS type) AS column, ...)`.
   That pipeline handles session functions, Unicode binding, concatenation and
   the translator.
4. **Layout.** Derived rows carry `nvarchar`/`nchar` values as UTF-8 VARCHAR,
   while table columns store UTF-16 carriers. The lowered expression therefore
   reads each Unicode column through `__msduck_carrier_utf8`. Conversions that
   read their input as VARCHAR text get the UTF-8 text of a carrier-valued
   operand, such as the result of `JSON_VALUE` or `LEFT`, instead of its
   stringified STRUCT.
5. **Storage.** A Unicode result is stored as UTF-8 VARCHAR, the layout views
   and derived tables use. The column's declaration (for example
   `nvarchar(200)`) is recorded as usual. The lowered DuckDB expression
   replaces the generated expression, and later passes do not translate it
   again.

`sys.columns.is_computed`, `COLUMNPROPERTY(..., 'IsComputed')`, the declared
`type`/`max_length` and the result descriptors match the capture. Writes that
name a computed column fail with 271, as before.

**Indexes and filters.** Indexes on these columns can be created, and
filters such as `WHERE value = N'abc'` work.

- A MAX computed column fails with 1919, like `body + N'!'` over
  `nvarchar(max)`.
- `CREATE INDEX` on a `JSON_VALUE` column (`nvarchar(4000)`) succeeds.
  SQL Server also warns with 1945 that the key can exceed 1700 bytes; msduck
  does not.

**Restart.** The generated expressions use the native function
`__msduck_carrier_utf8` and the macro `__msduck_computed_text`. Both are
registered for every DuckDB instance, so the expressions survive a restart.

## Session-dependent column defaults

A column `DEFAULT` can now read session state, in `CREATE TABLE` and in
`ALTER TABLE ... ADD` (including `WITH VALUES`). The value always comes from
the session that inserts.

| Function | Value |
|---|---|
| `SUSER_SNAME()`, `SUSER_NAME()`, `SYSTEM_USER`, `ORIGINAL_LOGIN()` | the login |
| `HOST_NAME()`, `APP_NAME()` | the LOGIN7 workstation and application names |
| `CAST`/`CONVERT(type, SESSION_CONTEXT(N'key'))` | the stored value, converted from its base type |
| `SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'key'), 'property')` | a property of the stored value (issue #869) |
| `SESSION_CONTEXT(N'key') IS [NOT] NULL` | whether the key has a value |

**Mechanism.** DuckDB evaluates the stored default when it binds the
`INSERT`. DuckDB variables belong to a connection, and every msduck session
has its own connection.

- The feature rewrites the session functions of a DEFAULT into
  `getvariable(...)` reads.
- Before every batch and statement, the feature writes the session's login,
  client names and `SESSION_CONTEXT` entries into variables of the session's
  connection. Only changed variables are written, and the values are bound.
- The `SessionContext` API now exposes its entries and its key identity
  (companion task `gaps-computed-v1-context`).

This covers `INSERT ... VALUES` (including `DEFAULT`), `INSERT ... SELECT`,
`DEFAULT VALUES`, sp_executesql and `ALTER TABLE ... ADD ... WITH VALUES`.
After RESETCONNECTION, the session context is empty, so the defaults are NULL.

**SESSION_CONTEXT rules:**

- **Conversion.** `SESSION_CONTEXT` returns `sql_variant`, and an explicit
  conversion converts from the stored base type: `nvarchar`, `bit`, `tinyint`,
  `smallint`, `int` or `bigint`. The rewrite applies the user's conversion to
  each base type, and the stored kind selects one branch. A key with no value
  gives NULL.
- **Keys.** A key matches the stored key under the captured rule: case is
  ignored, trailing spaces are ignored, and the last character must match
  exactly.
- **Bare use.** A bare `SESSION_CONTEXT(...)` default on a non-`sql_variant`
  column fails when the table is created, with SQL Server's 257 (state 3).
- **Key type.** A `varchar` key fails with 8116, as in queries.
- **Properties.** `SQL_VARIANT_PROPERTY(SESSION_CONTEXT(...), 'property')`
  with a constant property name reads the stored kind, so a conditional
  default such as
  `CASE WHEN SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'foo'), 'BaseType') = N'nvarchar' THEN CONVERT(nvarchar(100), SESSION_CONTEXT(N'foo')) ELSE NULL END`
  stores the nvarchar value and NULL for an `int` value or no value. The
  captured values are:

  | Base type | BaseType | MaxLength | Precision | Scale | TotalBytes | Collation |
  |---|---|---|---|---|---|---|
  | nvarchar | `nvarchar` | declared bytes (6 for `N'bar'`, 100 for `nvarchar(50)`) | 0 | 0 | 8 + data bytes | `SQL_Latin1_General_CP1_CI_AS` |
  | bit, tinyint, smallint, int, bigint | the type | 1, 1, 2, 4, 8 | 1, 3, 5, 10, 19 | 0 | MaxLength + 2 | NULL |

  The variables also hold an nvarchar value's declared and total byte
  lengths. Where a comparison (`=`, `<>`, `<`, `BETWEEN`, `IN`, `IS NULL`,
  simple `CASE`) or an explicit `CAST`/`CONVERT` consumes the property, it is
  evaluated in its base type (`nvarchar(128)` for BaseType and Collation,
  `int` otherwise), which is what SQL Server converts the sql_variant to. In
  other positions, and with a non-constant property name, the default is
  refused with 40515.
- **sql_variant results.** ISNULL, COALESCE, NULLIF, IIF and CASE results built
  from `SESSION_CONTEXT` or its `SQL_VARIANT_PROPERTY` are `sql_variant`, so a
  default of another column type fails with 257 (state 3) when the table is
  created, as captured. `ISNULL(NULL, SESSION_CONTEXT(...))` has the
  replacement's type and fails the same way. So does an `ISNULL` whose replacement is such a
  sql_variant when the first argument's type is evident (a literal or an
  explicit conversion); the message names that type. Converting first, as in
  `ISNULL(CONVERT(nvarchar(10), SESSION_CONTEXT(N'foo')), N'none')`, works.
- **Insert paths.** The conditional defaults are evaluated per inserted row
  in the inserting session for `INSERT ... VALUES`, `INSERT ... SELECT`,
  `DEFAULT` and `MERGE ... WHEN NOT MATCHED THEN INSERT`.

**SQL_VARIANT_PROPERTY of SESSION_CONTEXT in queries.** With a constant
property name, the property is computed from the session's stored value. A
property selected directly is a `sql_variant` (a sysname for BaseType and
Collation, an `int` otherwise), as captured. In comparisons and explicit
conversions it is evaluated in its base type. This also applies to
`SESSIONPROPERTY`.

**HOST_NAME() and APP_NAME() in queries.** Both are now supported anywhere, as
nullable `nvarchar(128)`. They read the same variables, so a view or default
that uses them sees the session that queries or inserts.

## Remaining differences

**Asserted by the client tests:**

- **Expression errors during writes.** DuckDB evaluates every generated column
  while it writes a row. An expression error during INSERT or UPDATE, such as
  invalid JSON, fails with error 50000 and DuckDB's message `Constraint
  Error: Incorrect value for generated column ...`. SQL Server fails with
  13609. For a non-persisted column, SQL Server raises the error only when the
  column is read, but msduck also raises it on the write.
- **Conversion errors.** Converting a `SESSION_CONTEXT` value that is not a
  number to a numeric type fails with msduck's general conversion error 245.
  SQL Server fails with 8114.
- **sql_variant defaults.** A bare `SESSION_CONTEXT` default on a `sql_variant`
  column is refused with 40515. msduck's `sql_variant` carrier cannot hold
  `nvarchar` values. ISNULL, COALESCE, CASE and similar `sql_variant` results
  over `SESSION_CONTEXT` on a `sql_variant` column are refused the same way.

- **Implicit sql_variant writes.** Writing or assigning the property without
  a conversion (`INSERT ... VALUES`, `UPDATE ... SET`, `SET @v =`) fails in
  SQL Server with 257 when the batch is compiled, so earlier statements of
  the batch do not run. msduck refuses it with 40515 when the statement runs,
  including the select items of `INSERT ... SELECT`, so earlier statements of
  the batch have already run. Moving the refusal into whole-batch preflight
  is outside this task's files.
- **NULLIF over SESSION_CONTEXT.** SQL Server accepts
  `DEFAULT (NULLIF(1, SESSION_CONTEXT(N'k')))`; msduck refuses it with 40515.

**Not asserted by the capture:**

- **Converting compound sql_variant results.** An explicit conversion is
  applied to `SESSION_CONTEXT` itself, or to `SQL_VARIANT_PROPERTY` of it. A
  conversion of a compound sql_variant result, such as
  `CONVERT(nvarchar(10), CASE WHEN ... THEN SESSION_CONTEXT(N'k') END)` or
  `CONVERT(int, ISNULL(SESSION_CONTEXT(N'k'), 0))`, is refused with 40515;
  SQL Server accepts it. Convert each `SESSION_CONTEXT` operand instead.

- **Comparing SESSION_CONTEXT itself.** A DEFAULT that compares the
  `sql_variant`, such as `CASE WHEN SESSION_CONTEXT(N'foo') = 1 THEN ...`, is
  refused with 40515; convert it or test `SQL_VARIANT_PROPERTY` first.
- **Property comparisons are binary.** BaseType is compared in its lower-case
  base type text; SQL Server compares it under the case-insensitive server
  collation, so `= N'NVARCHAR'` matches there and not in msduck.
- **Other positions in queries.** A `SQL_VARIANT_PROPERTY` of a session value
  is returned as a `sql_variant` only from the outermost select list of a
  SELECT. Nested in another expression that is not a comparison or explicit
  conversion (for example `ISNULL(SQL_VARIANT_PROPERTY(...), N'x')`), or
  selected by a derived table, CTE or subquery, it is refused with 40515.
  SQL Server accepts, for example,
  `SELECT CONVERT(nvarchar(128), p) FROM (SELECT SQL_VARIANT_PROPERTY(...) AS p) s`.
  msduck's sysname `sql_variant` carrier would convert to its internal struct
  text there, so it is refused instead.

**Not covered by this work:**

- **SESSIONPROPERTY.** It is still refused in stored definitions.
- **Session functions in computed columns.** A computed column that uses a
  session function (`SUSER_SNAME()`, `SYSTEM_USER`, `HOST_NAME()`,
  `APP_NAME()`, `SESSION_CONTEXT`, `DB_NAME()`, `@@SPID` and similar) fails
  with 4936 when `PERSISTED`, as in SQL Server. A non-persisted one is refused
  with 40515, because msduck would fix the creating session's value into the
  definition. SQL Server accepts it.
- **Other stored definitions.** Session functions in CHECK constraints are not
  covered. Defaults with `USER_NAME()` and `DB_NAME()` are not supported either:
  msduck does not implement `USER_NAME()`, and `DB_NAME()` in a stored
  definition keeps the creating database.
- **Metadata:**
  - `HOST_NAME()` and `APP_NAME()` descriptors lack the computed flag (TDS
    flags 1 instead of 33).
  - `INFORMATION_SCHEMA.COLUMNS.COLUMN_DEFAULT` shows the backend expression,
    as for other lowered defaults.
  - `sys.computed_columns` and `sys.default_constraints` now exist; see
    [catalogs](gaps-catalog.md).
  - Descriptor differences of `sys.columns` itself are outside this task.
- **Unpaired surrogates.** Unicode computed values are stored as UTF-8, so an
  unpaired surrogate becomes U+FFFD.
- **Earlier limits still apply:**
  - `ALTER TABLE ... ADD name AS expression` is not supported; DuckDB cannot
    add generated columns.
  - `PERSISTED NOT NULL` is reported as nullable.
  - Non-persisted non-deterministic expressions are refused.
- **General Unicode gaps in plain queries** (comparisons with literals,
  concatenation and CONVERT over nvarchar columns, styled CONVERT, and bit to
  text) were fixed by later work. See
  [Unicode predicates](gaps-unicode-predicates.md) and
  [conversions](gaps-conversion.md).
