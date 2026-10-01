# Predicates, ordering and conversions over Unicode carriers

NVARCHAR and NCHAR columns are stored as the Unicode carrier
`STRUCT(__msduck_utf16le BLOB)` (see [unicode-storage-reference.md](unicode-storage-reference.md)).
DuckDB cannot compare a carrier with VARCHAR text, applies LIKE only to
VARCHAR, orders carriers by their little-endian payload bytes, and turns a
carrier into its STRUCT display text when a cast or CONCAT asks for VARCHAR.
So, before this work, `WHERE name = N'x'` failed with 245, `LIKE` failed to
bind, `n + N'!'` returned `{'__msduck_utf16le':!` and `CONVERT(nvarchar(5), n)`
returned `{'__m`.

These operations now work over carrier operands:

- comparisons `=`, `<>`, `<`, `>`, `<=`, `>=`, `IN (list)`, `IN (subquery)`,
  `BETWEEN`, simple `CASE x WHEN …`, and joins and correlated subqueries
  through them;
- `LIKE` and `NOT LIKE`, with `ESCAPE`;
- `ORDER BY` a carrier column (directly, by select-list alias or by ordinal),
  including TOP and OFFSET/FETCH;
- concatenation, `CONCAT`, `CAST` and `CONVERT` to NVARCHAR, NCHAR, VARCHAR and
  CHAR, `SELECT @variable = column`, and the VARCHAR-only DuckDB functions that
  `REPLACE`, `SUBSTRING`, `REVERSE`, `ASCII`, `CONCAT_WS` and `STRING_AGG` reach;
- `ISNULL`, `COALESCE`, `IIF`, `CASE` results and set operations (`UNION`)
  that mix a carrier column with other text.

The other operand can be an N'' or '' literal, an NVARCHAR or VARCHAR
variable or RPC parameter, another NVARCHAR or VARCHAR column or a
subquery. This holds in SELECT, UPDATE and DELETE statements, views, procedures and
scalar conditions such as `IF EXISTS (…)`.

## Semantics

Comparisons use msduck's default binary comparison, which corresponds to SQL
Server's `Latin1_General_100_BIN2`:

- case-sensitive;
- ordered by UTF-16 code units, so a surrogate pair sorts by its high
  surrogate (before U+E000) and U+0100 sorts after every ASCII character;
- the shorter operand is padded with spaces, so trailing spaces never
  distinguish values and `N'a' + NCHAR(0)` sorts before `N'a'`.

NULL follows SQL's three-valued logic, including `NOT IN` with a NULL in
the list or subquery. Isolated surrogates, NUL characters and NVARCHAR(MAX)
values compare like any other unit.

LIKE follows SQL Server's Unicode pattern matching:

- `_` matches one UTF-16 code unit, so a surrogate pair needs `__`;
- trailing spaces are significant in both the value and the pattern, so
  an NCHAR(6) value `x` does not match `N'x'`;
- `[a-c]`, `[^a-c]`, `[%]`, `[_]` and `[[]` match as in SQL Server. `[]`
  matches nothing and `[^]` matches any one unit; an unterminated set
  matches nothing;
- the escape character makes the next unit literal, inside a set too, and
  wins over `]`, `^` and `-`. A pattern that ends in the escape character
  matches nothing;
- an escape that is not one character fails with 506.

Explicitly collated operations (`COLLATE`) are left alone; the conversion
work handles them.

## How it works

The `keys` extension module (`src/engine/ext/keys/predicates/`) works in two
stages.

The first stage runs before translation, in `rewrite_statement` and, for
subqueries of scalar evaluations, `rewrite_expr`. It finds the carrier columns among the
relations the statement reads with `pragma_table_info`, then:

- **marks** comparison, LIKE, IN and BETWEEN operands that are carrier
  columns, or character functions or scalar subqueries over them, and
  `IN (subquery)` tests with a carrier on either side (`mark.rs`);
- **pins** carrier columns that ISNULL, COALESCE, IIF, CASE or a set
  operation mix with other values, as `CAST(column AS <declaration>)`
  (`pin.rs`). The cast keeps the logical type, and so the result metadata,
  while the backend gets text that it can unify with VARCHAR;
- **orders** ORDER BY items that name carrier columns by a sort key, and then
  by the value itself for other types (`order.rs`).

The second stage runs last on the backend AST, in `lower_expr` (`lower.rs`):

- Marked operations compare byte keys built by
  `__msduck_unicode_order_key`.
- Other operations that may involve a carrier dispatch on `typeof`, which
  DuckDB folds while binding. Each rewrite keeps the original expression as
  the branch for operands that are not carriers, so other types keep their
  exact behavior. Operands with subqueries are never dispatched, because
  DuckDB would plan the subquery once per branch.
- The character cast macros, `CAST(… AS VARCHAR)` and CONCAT read a carrier's
  code units instead of its STRUCT text. A carrier conversion that feeds a
  carrier consumer (concatenation, storage) stays a carrier, so isolated
  surrogates survive.

The sort key (`key.rs`) encodes each non-space unit with the run of spaces
before it, so that comparing keys byte-wise equals comparing space-padded
code units. Equal keys mean equal values, so the same key also serves
equality, IN, BETWEEN, joins and ORDER BY. A unit test checks the order
against `msduck_core::bin2::compare` for every pair of short strings over
NUL, controls, space, ASCII, surrogates and U+E000, and for long space runs.

LIKE (`like.rs`) is a native matcher over UTF-16 units (`__msduck_unicode_like`).
Constant patterns are parsed once per vector.

## Reference evidence

`reference/gaps-unicode-predicates.json` holds 9 programs (111 steps)
captured twice, identically, by `scripts/capture-gaps-unicode-predicates.mjs`.
Each program runs in a fresh database created with
`COLLATE Latin1_General_100_BIN2`. Each step keeps its rows, column types,
diagnostics and DONE tokens; RPC steps (sp_executesql with NVARCHAR and
VARCHAR parameters) keep their rows and diagnostics. The values cover
trailing spaces, case, NUL, U+0100, a surrogate pair, U+E000, an isolated
surrogate, empty strings, NULL, LIKE metacharacters and a 5000-character
NVARCHAR(MAX) value.

The programs ran on the pinned reference image
(`mcr.microsoft.com/mssql/server:2025-latest@sha256:86cc6144…`, SQL Server
2025 RTM-CU7, 17.0.4065.4). To capture again:

```sh
node scripts/capture-gaps-unicode-predicates.mjs artifacts/compatibility/gaps-unicode-predicates-reference
```

The script labels its container `msduck.task=gaps-unicode-predicates-v1:<pid>`
and never replaces the checked-in fixture.

`tests/compat/unicode_predicates.test.mjs` replays every step through tedious
and requires equal rows, diagnostics and column types. Column lengths must
also match, except for the differences it lists below. It also covers the
original failing statements, procedures, views and scalar conditions.
`tests/gaps_unicode_predicates.rs` checks the results through the engine, and
that views and computed columns using the lowering work after a restart that
replays the write-ahead log. Unit tests cover the sort key, every captured
LIKE case and the backend rewrites.

## Remaining differences and limits

- **Invalid LIKE escape.** Error 506 is raised before execution, so msduck
  sends no column metadata before it; SQL Server sends the metadata first.
  A non-literal escape that is not one character fails with a generic
  error instead of 506.
- **Result lengths.** The lengths of some character results are not
  inferred, and fall back to NVARCHAR(MAX): CONCAT over columns, COALESCE of
  NVARCHAR and VARCHAR, set operations that mix them, REPLACE, SUBSTRING,
  REVERSE and STRING_AGG. SQL Server reports, for example, nvarchar(25) for
  CONCAT and nvarchar(4000) for REPLACE.
- **GROUP BY, DISTINCT and set-operation duplicates** compare carriers by
  their exact units. Values that differ only in trailing spaces (`N'x'` and
  `N'x  '`) form separate groups, where SQL Server forms one. ORDER BY on a
  `SELECT DISTINCT`, on a set operation, and inside window functions
  (`OVER (ORDER BY …)`, `PARTITION BY`) still uses DuckDB's payload order.
- **Text functions count code points.** SUBSTRING, REVERSE and similar
  functions that reach DuckDB's VARCHAR functions work on code points, not
  UTF-16 units. They differ from SQL Server only for supplementary
  characters.
- **Carriers converted to VARCHAR text lose isolated surrogates.** A CAST that
  splits a surrogate pair, or a stored isolated surrogate converted to VARCHAR,
  becomes U+FFFD. That covers the truncating casts outside concatenation and
  storage, and the results of the text functions above.
- **Unresolved operands.** The first stage resolves column names against
  the tables and views of the statement. Columns of derived tables and CTEs,
  and names that a select-list alias or derived column shadows, go through
  the `typeof` dispatch instead. That dispatch cannot bind an ordering
  comparison, IN or LIKE between a carrier and a VARCHAR expression other
  than a literal, and it does not apply to operands with subqueries. Those
  combinations still fail.
- **Nested conversions.** A dispatch is not repeated over an operand that
  already contains one, so that nesting does not copy expressions
  exponentially. A comparison of a converted value that the first stage
  does not mark, such as `CAST(n AS nvarchar(5)) = N'x '`, compares the
  converted text as VARCHAR, where trailing spaces count.
- **Carriers against numbers.** A marked comparison of an NVARCHAR column with
  a number fails ("Comparing nvarchar with a value of DuckDB type … is not
  supported"). SQL Server converts the NVARCHAR value to the number's type.
- **Literals and variables.** N'' literals, NVARCHAR variables and
  parameters are VARCHAR in the backend. Comparisons between them, without
  a carrier column, keep the existing behavior: `N'a' = N'a  '` is false.
- **MERGE** statements are not supported by msduck yet. The lowering applies
  to them once they are.
- **DATALENGTH over concatenation** (`DATALENGTH(n + N'!')`) still fails
  with 40515, as it does for VARCHAR columns.
- **NCHAR(n) for surrogate code units** is not supported (an existing limit).
  The tests store isolated surrogates with `CAST(0x3DD8 AS nvarchar(1))`.
