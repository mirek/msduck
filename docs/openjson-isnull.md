# ISNULL, COALESCE, IIF and CASE over OPENJSON

OPENJSON returns its `key` (NVARCHAR(4000)) and `value` (NVARCHAR(MAX))
columns as Unicode carriers, `STRUCT(__msduck_utf16le BLOB)`, so unpaired
surrogates survive (see [OPENJSON](openjson.md)). DuckDB cannot unify a
carrier with VARCHAR text, so before issue #900 every mix of an OPENJSON
column with a text literal, variable or column failed with 245:
`ISNULL(j.[value], N'')`, `COALESCE(j.[value], N'x')`, `IIF(j.[value] IS
NULL, N'n', j.[value])`, `CASE WHEN j.[value] = N'1' ...` and plain
comparisons such as `j.[value] = N'x'`. The workload report hit it in a
multirow AFTER UPDATE trigger that diffs `FOR JSON` snapshots of `inserted`
and `deleted` through an inline function that FULL JOINs two OPENJSON calls
and filters with `ISNULL(old_value, N'') <> ISNULL(new_value, N'')`.

## Behavior

- `__msduck_isnull`, the backend ISNULL, dispatches on the bind-time type of
  its first argument. A carrier first argument packs a text replacement into
  a carrier with `__msduck_carrier_input`, so the result keeps the carrier's
  code units and type. A carrier replacement of a text first argument
  converts through its code units (`__msduck_unicode_text`), never through
  its STRUCT display text. Integer and other first types are unchanged.
  This covers carriers that reach ISNULL through derived tables, inline
  functions and APPLY, which the predicate catalog cannot type.
- The NVARCHAR and NCHAR widths that ISNULL applies for a known first
  argument (`__msduck_isnull_nvarchar_width`, `__msduck_isnull_nchar_width`)
  accept both VARCHAR text and carriers and keep the input's backend type.
  Previously a carrier from an aggregate subquery, as in
  `N'text' + ISNULL((SELECT MAX(v) FROM ...), N'')`, reached the VARCHAR-only
  width function and failed to bind (found while verifying #901).

The multirow trigger from the report, ISNULL over OPENJSON keys and values
in projections and stored results, and ISNULL over aggregated subqueries now
match SQL Server.

## Evidence

- `scripts/capture-openjson-isnull.mjs` captures 21 cases from the pinned
  SQL Server 2025 reference image (17.0.4065.4) into
  `reference/openjson-isnull.json`: the report's query over NVARCHAR(MAX),
  VARCHAR(MAX), bounded NVARCHAR and literal documents; ISNULL key, value,
  ANSI replacement, type, LEN and DATALENGTH; COALESCE mixes; carriers as
  replacements; IIF and CASE results and comparisons; ISNULL in WHERE and
  ORDER BY; comparisons and IN; explicit WITH schemas over NVARCHAR and
  VARCHAR documents; two OPENJSON sources FULL JOINed through APPLY and
  through an inline function; the multirow trigger, with changed and
  unchanged rows; and ISNULL over STRING_AGG and MAX subqueries inside
  concatenation and over stored NVARCHAR and NCHAR columns. Comparison cases avoid supplementary characters,
  punctuation and case differences, whose ordering depends on the
  collation.
- `tests/compat/openjson_isnull.test.mjs` replays every case through
  tedious, comparing column names, types and lengths, rows, errors and DONE
  counts. Known differences assert msduck's complete current result.
- `tests/openjson_isnull.rs` checks the native ISNULL dispatch (carrier code
  units including an unpaired surrogate, carrier replacements of text,
  integer and date first arguments), stored ISNULL results over OPENJSON,
  the trigger, and both overloads of the ISNULL width functions.
- `tests/compat/apply_full_join.test.mjs` now expects SQL Server's rows for
  its ISNULL trigger case.

## Remaining differences

The tedious test lists them exactly:

- COALESCE, IIF, CASE results and comparisons (`=`, `<>`, `>`, IN, simple
  CASE) that mix a direct OPENJSON `key`/`value` column, or an explicit WITH
  NVARCHAR column, with text still fail with 245, or with DuckDB's binder
  error for COALESCE of a VARCHAR and an NVARCHAR WITH column. The same
  holds for comparisons of `ISNULL(j.[value], N'')` with a literal. The
  predicate catalog in `src/engine/ext/keys/predicates/catalog.rs` treats
  OPENJSON aliases as derived tables of unknown type; declaring their
  columns (key NVARCHAR(4000), value NVARCHAR(MAX), WITH columns by
  declaration) lets the existing marking and pinning handle them. That file
  belonged to the default-collation task while this one ran; a prototype
  made 9 more of the 21 cases match.
- Two carriers compare by their bytes, so `ISNULL(l.[value], N'') <>
  ISNULL(r.[value], N'')` treats `'x'` and `'x  '` as different; SQL
  Server ignores trailing spaces.
- `COALESCE(l.[key], r.[key])` and `COALESCE` over OPENJSON WITH columns
  report `nvarchar(max)`; SQL Server keeps the declared width.
- ISNULL with a VARCHAR first argument and a carrier replacement holding
  characters outside Windows-1252 fails on the wire instead of returning
  `?`; the same happens for VARCHAR and NVARCHAR variables.
- The OPENJSON `type` column is `int`; SQL Server reports `tinyint`, so
  `ISNULL(j.[type], 0)` is `int` too.
