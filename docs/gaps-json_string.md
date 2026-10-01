# FOR JSON AUTO, JSON_MODIFY, ordered STRING_AGG, STRING_SPLIT and HASHBYTES

Issue #724 (task `gaps-json-string-v1`, with companion
`gaps-json-string-v1-hooks`). The workload report against v0.2.4 found these
failures:

| Repro | v0.2.4 | Now |
| --- | --- | --- |
| `SELECT 1 AS value FOR JSON AUTO` | 50000 "FOR JSON AUTO is not yet supported" | 13600, as SQL Server: AUTO needs a table |
| `JSON_MODIFY(N'{"a":1}', N'$.a', 2)` | 208 | `{"a":2}`, NVARCHAR(MAX) |
| `STRING_AGG(CAST(id AS varchar(10)), ',') WITHIN GROUP (ORDER BY id)` | 50000 (unknown ordered aggregate) | ordered result, VARCHAR(8000) |
| `HASHBYTES('MD5', N'foo')` | 208 | `0x76FB6C85...`, VARBINARY(8000) |
| `SELECT value FROM STRING_SPLIT('a,b', ',')` | 208 | `a`, `b`, VARCHAR(3) NOT NULL |

## Evidence

`reference/gaps-json_string.json` holds 79 observations captured with
`scripts/capture-gaps-json_string.mjs` in two fresh SQL Server containers,
whose raw results matched. The pinned 2025 image was not available on the
capture host, so the fixture records the image it used
(`mcr.microsoft.com/mssql/server:2022-latest`, 16.0.4236.2). It covers FOR
JSON AUTO nesting, options, errors and nested use, the whitespace rules of
JSON_MODIFY inserts, deletes and appends, and the repros. `--compare PORT`
diffs a running msduck against it; all 79 observations match, including
TDS types, lengths and nullability.

The earlier first-party fixtures cover the other functions in depth and are
replayed through tedious by `tests/compat/json_string.test.mjs`:
`reference/json-constructors.json` (JSON_MODIFY), `string-split.json`,
`string-agg.json` and `hashbytes-checksum.json`. Rust coverage is in
`tests/gaps_json_string.rs` (values and error numbers through tiberius) and
in unit tests of the syntax rules.

## Behavior

### FOR JSON AUTO

The rules live in `crates/msduck-sql/src/dialect/ext/json_string/auto.rs`:

- Each table-like source (table, view, CTE, derived table, VALUES) whose
  column first appears in the SELECT list opens a nesting level. The first
  is the top level; each later one is an array inside the level opened just
  before it, named by its alias or by the table name as written (`dbo.b`).
- A table's columns go to its level. Expressions, variables and columns of
  table-valued functions (STRING_SPLIT, OPENJSON) go to the deepest level
  opened so far. An expression of a later table's column, placed before
  that table's first column, therefore stays at the upper level.
- Own properties come first in SELECT order, then the nested array.
- Consecutive rows with equal table columns at a level (and above) share
  that level's object; expressions are not compared and leaf objects never
  merge. Text compares like the default collation: trailing spaces are
  ignored and, unless the column's collation is case-sensitive or binary,
  case is ignored. The first row's spelling is kept.
- A null-extended child (LEFT JOIN without a match) is `{}`, or its NULL
  properties with INCLUDE_NULL_VALUES.
- Dotted aliases are literal names and duplicate names are kept.
- Set operations are flat.
- ROOT, INCLUDE_NULL_VALUES and WITHOUT_ARRAY_WRAPPER behave as for PATH.
  With no rows, a top-level AUTO query returns no row and a nested one is
  NULL.
- Batch-time errors: 13620 (ROOT with WITHOUT_ARRAY_WRAPPER), 13600 (no
  table source; table-valued functions do not count) and 13605 (unnamed
  column). As in SQL Server, these stop the whole batch before any
  statement runs.

AUTO works at top level, in scalar subqueries (including correlated ones),
nested inside FOR JSON PATH or AUTO, in variable assignments and in INSERT
... SELECT. The root adapter (`src/for_json/auto.rs`) binds the plan
against catalog metadata for unqualified columns and stars. It formats
top-level results while streaming rows, and lowers nested queries to the
native `__msduck_json_auto_row` and `__msduck_json_auto` functions inside
the existing `__msduck_json_array` wrapper, so outer FOR JSON clauses still
embed them as JSON.

### JSON_MODIFY

The rules live in `crates/msduck-sql/src/dialect/ext/json_string/modify.rs`
and edit the document's UTF-16 text in place. Untouched text, whitespace and
duplicate keys are kept:

- Replace: lax and strict update the first matching key or array element.
- Insert: a lax missing key is added before the closing brace (`,"b":2` after
  any whitespace; no comma in an empty object).
- NULL in lax mode deletes the first matching member and one adjacent comma.
  In strict mode it writes `null`. An array element is set to `null`.
- `append` adds to an array (before `]`). A missing lax key creates
  `"k":[v]`, and `append $` appends to a root array.
- Missing paths: lax returns the input unchanged; strict raises 13608 state
  2. Appending to a non-array: lax unchanged, strict 13621.
- Values: JSON_QUERY, JSON_MODIFY and FOR JSON results are embedded
  unescaped. Strings are escaped (including `/`). Integers and DECIMAL keep
  their spelling. FLOAT and REAL use `d.ddddddddddddddde±xxx`, and BIT is
  `true`/`false`. MONEY, date and time types, UNIQUEIDENTIFIER, VARBINARY
  and XML raise 8116 at compile time.
- Errors, with SQL Server's states: 13609/7 (document, with character and
  position), 13607/22, 14 and 21 (path syntax), 13619 (`$` alone), 13660/4
  (`$.*`), 8116/1 (literal NULL path) and 8116/8 (NULL path value), and 174
  (arity).
- The result is NVARCHAR(MAX). A NULL document gives NULL.

### STRING_AGG

STRING_AGG is lowered to DuckDB's `string_agg`, moving WITHIN GROUP into an
ordered aggregate. NULL inputs are skipped. A NULL separator concatenates
without one. NVARCHAR carriers are decoded to text, and other types use SQL
conversion text. The declaration follows SQL Server: VARCHAR(8000) or
NVARCHAR(4000) for bounded character inputs, the MAX family for MAX inputs,
and NVARCHAR(4000) for other known types. A bounded result over 8000 bytes
raises 9829 (state 0 for VARCHAR, 1 for NVARCHAR). Compile-time errors: 8733
(separator neither a literal nor a variable), 8734 (MAX separator variable),
8116 (binary input; VARCHAR input with NVARCHAR separator; integer
separator), 4113 (OVER) and 8711 (incompatible WITHIN GROUP orderings in
one scope). Unordered STRING_AGG over NVARCHAR columns, which v0.2.4
returned as carrier text, now returns the values.

### STRING_SPLIT

`STRING_SPLIT(source, separator [, enable_ordinal])` in FROM or APPLY
becomes a lateral derived table over the native `__msduck_string_split`.
Empty tokens are kept, a NULL source yields no rows, and an empty source
yields one empty token. `value` follows the source's declaration: VARCHAR or
NVARCHAR (NVARCHAR when either argument is), the same width, and MAX. It is
NOT NULL only for a string literal source. The optional `ordinal` is BIGINT
NOT NULL from 1. Errors: 214/11 (separator not a single UTF-16 unit,
including NULL, raised after the descriptor), 4199 (ordinal constant other
than 0 or 1), 8748 (ordinal not a constant), 8116 (non-character
arguments; decimal ordinal), 313 and 8144 (arity).

### HASHBYTES

MD2 (NULL, as captured), MD4, MD5, SHA, SHA1, SHA2_256 and SHA2_512 use the
digests in `crates/msduck-core/src/hashbytes.rs`. VARCHAR hashes Windows-1252
code-page bytes, NVARCHAR hashes UTF-16LE, and VARBINARY hashes raw bytes.
Algorithm names are case-insensitive, ignore trailing spaces, and return
NULL when unknown. The result is VARBINARY(8000). Arity is 174, and untyped
NULL or non-character/binary arguments raise 8116.

### Errors through DuckDB

Native functions raise
`__msduck_sql_error_v1:<number>:<state>:<class>:<text>`. The companion hook in
`src/json_extract.rs` recovers the SQL Server identity for an allowlist of
numbers, so TRY/CATCH, `@@ERROR` and scalar contexts (SET, DECLARE, IF) see
the same number and state as queries. The preflight hook in
`crates/msduck-sql/src/preflight.rs` routes FOR JSON AUTO clauses to the
json_string syntax checks instead of rejecting them.

## Remaining limits

- Procedure and trigger bodies are not implemented on `main` yet (separate
  gap tasks). The lowerings run in the ordinary statement and scalar
  evaluation hooks that those bodies execute through, so they apply there
  once bodies run, but that has not been exercised yet.
- FOR JSON AUTO grouping approximates collation equality, as described
  above. Accent-insensitive and other named collations are not modeled.
  `ROOT` without a name (`FOR JSON AUTO, ROOT`) does not parse, for PATH
  either.
- FOR JSON PATH keeps its existing behavior over no rows (`[]`, and `[]`
  when nested). SQL Server returns no row and NULL; the existing PATH
  tests assert the current behavior.
- The DONE token of an empty top-level AUTO result still counts one row.
- STRING_SPLIT: SQL Server's additional error 207 for a disabled `ordinal`
  column is not reported. A NULL ordinal with `ordinal` selected raises a
  generic binding error instead of 207.
- STRING_AGG declarations need static operand types. Columns of VALUES
  tables and other unresolved operands fall back to NVARCHAR(MAX), and a
  binary operand there is rejected after the descriptor. DATETIMEOFFSET
  inputs are not formatted.
- JSON_MODIFY: MONEY values held in unresolved operands are accepted as
  numbers. Text with isolated UTF-16 surrogates is rejected rather than
  preserved.
- HASHBYTES over `_UTF8`-collated VARCHAR hashes code-page bytes, because
  named collations are not implemented.
- `COLLATE` with named collations, `sp_describe_first_result_set`,
  JSON_OBJECT and JSON_ARRAY are outside this task. Long NCHAR concatenation
  chains do not finish in msduck generally. The tests skip those fixture
  records and name them.
