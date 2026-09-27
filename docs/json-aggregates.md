# JSON_OBJECTAGG and JSON_ARRAYAGG reference contract

`reference/json-aggregates.json` retains 194 SQL Server programs per run:

- 3 setup statements, 176 ordinary batches (116 named cases, plus 30 scalar
  families aggregated by each function) and a final session-reuse batch.
- 10 RPC `sp_executesql` calls.
- 4 `sp_prepare`/`sp_execute`/`sp_unprepare` programs. Three prepared
  successfully and ran 14 executions between them; one failed at prepare.

The generator ran every program in two fresh databases in each of two
independent containers. All four used the pinned SQL Server 2025 image digest
`86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a`, and all
four raw captures matched.

Each record keeps:

- the query, RPC parameter declarations and values;
- rows and TDS descriptors (type, length, flags, collation);
- error number, state, class, line and text;
- information messages and DONE tokens.

For prepared programs it also keeps the prepare, each execution and the
unprepare. To recapture and compare with the retained fixture, run
`node --max-old-space-size=2048 scripts/capture-json-aggregates.mjs OUTPUT`.
`--write-fixture` refuses to overwrite an existing fixture. Every mode
refuses, before starting any container, an `OUTPUT` that resolves to the
retained fixture (including through `..`, symlinked directories or a hard
link). `--one-database` is a faster diagnostic probe that cannot write it.

Values below are quoted exactly as the fixture holds them. Where this page
summarizes, the named fixture record is authoritative.

The source table `dbo.json_agg_src` has these rows:

| id | g | seq | rank_key | k | v | n | ansi |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | 1 | 2 | 2 | `b` | `y` | 2 | `b` |
| 2 | 1 | 1 | 1 | `a` | `x` | 1 | `a` |
| 3 | 1 | 3 | NULL | `c` | NULL | NULL | NULL |
| 4 | 2 | 1 | 1 | `d` | `x` | 4 | `x` |
| 5 | 2 | 2 | 1 | `d` | `x` | 5 | `x` |
| 6 | 4 | 1 | 1 | NULL | `z` | 6 | `z` |

`id` is the clustered primary key.

## Result type and descriptors

| Form | Captured behavior |
| --- | --- |
| Either aggregate, any non-JSON input | TDS `NVarChar`, length 65535 (MAX), database default collation (`SQL_Latin1_General_CP1_CI_AS`, sort ID 52). Flags are **1** (nullable only), unlike the scalar `JSON_OBJECT`/`JSON_ARRAY` flags 33. Wrapping the aggregate in `JSON_OBJECT`, `JSON_ARRAY`, `JSON_QUERY` or a scalar subquery gives flags 33. `sp_describe_first_result_set` reports `nvarchar(max)`, `max_length` -1, nullable, with TDS collation ID 13632521 and sort 52, including for a `VARCHAR` input column. |
| `RETURNING JSON`, or a `JSON`-typed input | Tedious receives TDS `VarChar`, length 65535, flags 33, UTF-8 collation. `sp_describe_first_result_set` reports `varchar(max)` (system type 167) with `Latin1_General_100_BIN2_UTF8` (collation ID 637535241, sort 0). `SELECT ... INTO` creates a `json` column (`max_length` -1, nullable, no collation). An aggregated `CAST(... AS JSON)` value takes this type without `RETURNING JSON` and is embedded unescaped (`{"v":{"x":1}}`, `[{"x":1}]`). |
| `SELECT ... INTO`, view columns | Non-JSON results become nullable `nvarchar` columns, `max_length` -1, with the database default collation. |
| Width | No truncation was observed. Six 4000-character values produced a 48086-byte object and a 48038-byte array. Six 10000-character MAX values produced 60019 characters. |
| Empty input | Both aggregates return one row holding NULL, not `{}` or `[]`, for a filtered-out source, an empty table or an empty sp_executesql/prepared filter. A grouped empty input returns no rows (DONE count 0). |

## JSON_OBJECTAGG

| Input | Captured behavior |
| --- | --- |
| Syntax | Exactly one `key:value` pair, optionally followed by `NULL ON NULL` or `ABSENT ON NULL`, then optionally `RETURNING JSON`. `JSON_OBJECTAGG(k,v)`, no arguments or two pairs: error 174 state 1 class 15 `The JSON_OBJECTAGG function requires 1 argument(s).` A missing value is 102 state 1. Both NULL clauses together: 102 state 20 `Incorrect syntax near 'JSON_OBJECTAGG'.` `RETURNING NVARCHAR(MAX)`: 102 state 19 `Incorrect syntax near 'RETURNING. Supported Syntax is RETURNING JSON'.` |
| Unsupported modifiers | An `ORDER BY` inside the call is 156 state 1 (near `ORDER`). `WITHIN GROUP (...)` is 102 state 1 (near `WITHIN`). `DISTINCT` is 102 state 1 (near `k:`). |
| Output shape | No whitespace. Pairs are in input row order; unordered group `g=1` gave `{"b":"y","a":"x","c":null}`, clustered-key order. A constant without `FROM` gives `{"a":1}`. |
| NULL value | The default and `NULL ON NULL` emit `"c":null`. `ABSENT ON NULL` omits the pair; if every pair is absent the result is `{}` (not NULL). |
| NULL key | Column, literal, sp_executesql and prepared-parameter NULL keys all give error 13638 state 1 class 16 `User error : Name parameter value in 'json_object' cannot be null`, even with `ABSENT ON NULL`. The error follows the column descriptor. In a grouped scan, rows for groups finished before the NULL-key group were sent first (`objectagg null key grouped` returned groups 1 and 2, then the error). |
| Duplicate keys | Kept as aggregated: `{"d":4,"d":5}`. |
| Key typing | Keys are converted to text: integer `1` → `"1"`, binary `0x41` → `"QQ=="`, `CHAR(3)` keeps its padding (`"k  "`). Keys are escaped like values. |
| Nesting | `JSON_OBJECT`, `JSON_ARRAY` and `JSON_QUERY` values are embedded as JSON (`{"a":{"n":1,"v":"x"}}`, `{"a":[1,"x"]}`, `{"q":{"x":[1,2]}}`). JSON-looking plain text is escaped as a string. An aggregate from a derived table embeds (`{"1":[1,2]}`). An aggregate inside the argument, such as `JSON_ARRAYAGG(n)` or `COUNT(*)`, is error 130 state 1 class 15. |
| Windows | `OVER(PARTITION BY g)`, `OVER()`, `OVER(ORDER BY id)` and an explicit `ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW` frame are all accepted. `OVER(ORDER BY id)` produces running objects: `{"b":2}` then `{"b":2,"a":1}`. |
| Scope errors | An ungrouped column is 8120 state 1 class 16. Use in `WHERE` is 147 state 1 class 15. Use in `HAVING` is accepted. |

## JSON_ARRAYAGG

| Input | Captured behavior |
| --- | --- |
| Syntax | One value, then optionally `ORDER BY`, then optionally a NULL clause, then optionally `RETURNING JSON`, in that order. A NULL clause or `RETURNING JSON` placed before `ORDER BY` is 156 state 1. Both NULL clauses together: 102 state 20. `RETURNING VARCHAR(100)`: 102 state 19 `Incorrect syntax near 'RETURNING'.` Two arguments: 174 state 3, lower-case `The json_arrayagg function requires 1 argument(s).` No arguments: 174 state 1, upper-case. `DISTINCT`: 313 state 2 class 16 `An insufficient number of arguments were supplied for the procedure or function json_arrayagg.` A `key:value` argument: 102 state 10. |
| NULL elements | The default and `ABSENT ON NULL` drop NULLs (`["x","y"]`; all-NULL input gives `[]`). `NULL ON NULL` emits `null` (`["x","y",null]`, `[null]`). The defaults are opposite to `JSON_OBJECTAGG`. |
| ORDER BY | `ASC`/`DESC`, several keys, expressions (`-id`), the aggregated value itself and `COLLATE` are accepted. Ascending order puts NULL keys first (`["c","a","b"]`); descending puts them last (`["b","a","c"]`). A `Latin1_General_BIN2` collation gave `["B","a","b"]`. An integer ordinal (`ORDER BY 1`) is 5308 state 1 class 16. |
| Ordering compatibility | Two ordered aggregates with different orders in one scope are 8711 state 1 class 16. This includes `JSON_ARRAYAGG` with `STRING_AGG ... WITHIN GROUP`, and applies at `sp_prepare` time. Identical orders are accepted, as is an ordered aggregate beside an unordered one. |
| Without ORDER BY | Order follows the plan. The captured scans used clustered-key order (`[1,2,3,4,5,6]`). This is observed, not guaranteed. |
| `WITHIN GROUP` | `JSON_ARRAYAGG(k) WITHIN GROUP (ORDER BY seq DESC)` is **accepted but did not order the result**. Output followed clustered-key order (`["b","a","c"]` where seq DESC gives `["c","b","a"]`, and `["y","x","x","x","z"]` for ascending seq). |
| Nesting | `JSON_OBJECT`/`JSON_ARRAY`/`JSON_QUERY` elements embed as JSON. A JSON-looking string is escaped (`["[1,2]"]`). An aggregate over grouped `JSON_OBJECTAGG` results embeds (`[{"b":2,"a":1,"c":null},{"d":4,"d":5}]`). An aggregate argument (`COUNT(*)`) is 130. |
| Composition | `JSON_OBJECT('g':g,'items':JSON_ARRAYAGG(...))` gives `{"g":1,"items":[1,2]}`. `JSON_ARRAY(JSON_ARRAYAGG(...),JSON_OBJECTAGG(...))` gives `[[1,2],{"a":1,"b":2}]`. `JSON_VALUE(JSON_ARRAYAGG(...),'$[1]')` gives `2`. A correlated scalar subquery with no matching rows gives NULL. |
| Windows | `OVER(PARTITION BY g)`, `OVER()` and `OVER(ORDER BY id)` (running `[2]` then `[2,1]`) are accepted. Combining an argument `ORDER BY` with `OVER` is 156 state 1 (near `OVER`). |
| Grouping | Grouped arrays are per group. With a `LEFT JOIN`, an unmatched group aggregates one NULL: `[]` by default, `[null]` with `NULL ON NULL`. Ungrouped column: 8120. In `WHERE`: 147. |
| View | A view over both aggregates grouped by `g` returns them with flags 9. Its columns are nullable `nvarchar(max)`. |

## Scalar families (identical in both aggregates)

Each family was aggregated from a single row. The formatting matches the
scalar constructors in `docs/json-constructors.md`:

| Family | Captured text |
| --- | --- |
| Integers | Bare, exact: `255`, `-32768`, `-2147483648`, `9223372036854775807` |
| `BIT` | `true` / `false` |
| `DECIMAL`/`NUMERIC` | Bare, declared scale kept: `-12.340`, `1.50` |
| `MONEY`/`SMALLMONEY` | Four decimals: `12.3456`, `-1.5000` |
| `FLOAT` | `1.000000000000000e-001`; negative zero `-0.000000000000000e+000` |
| `REAL` | `1.0000000e-001` |
| Date/time | Quoted ISO with `T` and declared scale: `"2024-01-02"`, `"03:04:05.1234567"`, `"2024-01-02T03:04:05.123"`, `"2024-01-02T03:04:00"`, `"2024-01-02T03:04:05.1234567"`, `"2024-01-02T03:04:05.1+05:30"` |
| `UNIQUEIDENTIFIER` | Quoted upper case |
| `CHAR`/`NCHAR` | Quoted with padding: `"ab   "`. `VARCHAR`/`NVARCHAR(MAX)`: `"ab"` |
| `VARBINARY` | Base64 `"QUJD"` |
| `XML` | Quoted with `/` escaped: `"<a>1<\/a>"` |
| `SQL_VARIANT` holding `INT` | Bare `1` |
| `JSON` | Embedded unescaped; result becomes the JSON/UTF-8 descriptor |
| Typed NULL `INT` | `{"v":null}` / `[]` (defaults) |
| `HIERARCHYID` (CLR) | Error 13666 class 16 after the descriptor. The object form is state 2 `json_object and json_objectagg does not support CLR type as parameters`; the array form is state 1 `json_arrayagg does not support CLR type as parameters`. |

String escaping matches the constructors:

- `"` → `\"`, `\` → `\\`, `/` → `\/`.
- TAB → `\t`, LF → `\n`.
- U+0001 → `\u0001`.
- U+007F, `é` and a surrogate pair are emitted unescaped.

## Parameters, prepared execution and completions

RPC and prepared parameters follow the same value rules:

- `NVARCHAR` key `key"1` → `{"key\"1":7}`.
- `BIGINT` 9007199254740993 is exact.
- `FLOAT` 0.5 → `5.000000000000000e-001`.
- `DECIMAL(10,3)` 1.5 → `1.500`.
- `DATETIME2(3)` → `"2024-01-02T03:04:05.123"`.
- `BIT` → `true`.
- A `VARCHAR` and an `NVARCHAR` value aggregate as `["x","yé"]`.

A replayed sp_executesql call and a repeated prepared execution gave the same
results.

The prepared runs covered these cases:

- A NULL group parameter matches no rows and returns one row whose two
  columns are both NULL.
- A NULL key in a later execution raises 13638 without invalidating the
  handle. The next execution succeeded.
- `sp_prepare` of two differently ordered `JSON_ARRAYAGG` calls fails with
  8711 and returns no handle.

Completion tokens:

- **Compile-time failures** (102, 130, 147, 156, 174, 313, 5308, 8120,
  8711) produce no column descriptor and a DONE without a row count.
- **Runtime failures** (13638, 13666) arrive after the descriptor, after any
  finished groups' rows. They are followed by a DONE without a row count.
- **Successful batches** end with a DONE carrying the row count.
- **RPC and prepared executions** end with `doneInProc` (row count, more)
  and `doneProc`. A failing execution carries only `doneProc`.
- **Prepare** returns the result descriptor with `doneInProc` count 0.

## Not captured

- Native JSON type on the wire. Tedious receives JSON results as
  `varchar(max)` UTF-8; a client negotiating the JSON feature may see a
  different descriptor.
- Whether `WITHIN GROUP` on `JSON_ARRAYAGG` is ever honoured. It is only
  known that the captured plans ignored it. Ordering without `ORDER BY`,
  including key order in `JSON_OBJECTAGG`, under parallel plans, hash
  aggregation or other indexes.
- Other CLR/spatial types (only `HIERARCHYID` was aggregated), `BINARY`,
  `ROWVERSION`, `DATETIME2` scales other than 7, and further escaping of
  control characters, which the constructor fixture already covers.
- Window frames other than the captured default and `ROWS UNBOUNDED
  PRECEDING`. `RANGE`, and `OVER` combined with NULL clauses or
  `RETURNING JSON`.
- `GROUPING SETS`/`ROLLUP`/`CUBE`, `DISTINCT` via a derived table, and
  aggregation inside `APPLY`, CTEs, or `UPDATE`/`MERGE` sources.
- Very large results (over 2 GB), memory-grant spills, and non-default
  `SET` options or compatibility levels.
- Collation variants of the result beyond the database default. Error line
  numbers are retained but not analyzed.

## Proposed successors

msduck does not implement these aggregates. This task only retains evidence.

1. **json-aggregates-core-v1** (deterministic core). Scope: an msduck-core
   JSON aggregate accumulator module and its unit tests, reusing the scalar
   JSON formatting proposed by `json-constructors-core-v1`. It should hold:
   - Per-function NULL defaults (object `NULL ON NULL`, array `ABSENT ON
     NULL`).
   - NULL-on-empty versus `{}`/`[]` on all-absent.
   - Duplicate-key preservation.
   - The NULL-key error 13638 and the CLR error 13666 with their states.
   - JSON-versus-string embedding by source kind.
   - Accumulating rows in a caller-supplied order.
   - Running (window) snapshots.
   - The result declaration rule (`nvarchar(max)` flags 1, or JSON/UTF-8
     for `RETURNING JSON` or JSON input).

   Tests replay this fixture's values without DuckDB.
2. **json-aggregates-sql-v1** (msduck-sql binding). Scope: parsing and
   binding over explicit catalog snapshots. It should cover:
   - The `key:value` argument.
   - The clause order `ORDER BY` → NULL clause → `RETURNING JSON`.
   - The compile-time errors 102 (states 1, 10, 19, 20), 156, 174 (states
     1 and 3), 313, 5308, 130, 147 and 8120.
   - Ordered-aggregate compatibility 8711 across `JSON_ARRAYAGG` and
     `STRING_AGG`.
   - Rejection of an argument `ORDER BY` combined with `OVER`.
   - Descriptor inference.

   How `WITHIN GROUP` on `JSON_ARRAYAGG` is handled needs a decision that
   preserves the captured behavior.
3. **json-aggregates-root-v1** (root integration). Scope: root backend
   lowering and root-side wire tests against this fixture, for batches,
   sp_executesql and sp_prepare/sp_execute. It should:
   - Feed the core accumulator in the requested order, evaluating operands
     once per row.
   - Emit the captured descriptors, flags and collations.
   - Raise runtime errors after the descriptor and after completed groups'
     rows.
   - Return NULL for empty input.
   - Reproduce the DONE/RPC completion shapes.

   DuckDB's `json_group_object`/`json_group_array` are not evidence that
   these SQL Server rules hold.

Parser, engine, catalog, metadata and client-test files were outside this
reference task's scope and were not changed.
