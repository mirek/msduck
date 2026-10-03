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
- The predicate catalog (`src/engine/ext/keys/predicates/catalog.rs`)
  declares OPENJSON row sources: the default schema's `key` as
  NVARCHAR(4000) and `value` as NVARCHAR(MAX) carriers and `type` as an
  integer, and explicit WITH columns by their declarations (NVARCHAR and
  NCHAR columns are carriers, VARCHAR and CHAR are text). Comparisons,
  IN, CASE operands and ORDER BY over these columns then use the Unicode
  comparison, and ISNULL, COALESCE, IIF and CASE that mix them with text
  convert them to their declared type first, like stored NVARCHAR columns.

## Evidence

- `scripts/capture-openjson-isnull.mjs` captures 17 cases from the pinned
  SQL Server 2025 reference image (17.0.4065.4) into
  `reference/openjson-isnull.json`: the report's query over NVARCHAR(MAX),
  VARCHAR(MAX), bounded NVARCHAR and literal documents; ISNULL key, value,
  ANSI replacement, type, LEN and DATALENGTH; COALESCE mixes; carriers as
  replacements; IIF and CASE results and comparisons; ISNULL in WHERE and
  ORDER BY; comparisons and IN; explicit WITH schemas over NVARCHAR and
  VARCHAR documents; two OPENJSON sources FULL JOINed through APPLY and
  through an inline function; and the multirow trigger, with changed and
  unchanged rows. Comparison cases avoid supplementary characters,
  punctuation and case differences, whose ordering depends on the
  collation.
- `tests/compat/openjson_isnull.test.mjs` replays every case through
  tedious, comparing column names, types and lengths, rows, errors and DONE
  counts. Known differences assert msduck's complete current result.
- `tests/openjson_isnull.rs` checks the native ISNULL dispatch (carrier code
  units including an unpaired surrogate, carrier replacements of text,
  integer and date first arguments), stored ISNULL results over OPENJSON,
  and the trigger.
- `tests/compat/apply_full_join.test.mjs` now expects SQL Server's rows for
  its ISNULL trigger case.

## Remaining differences

The tedious test lists them exactly:

- The OPENJSON `type` column is `int`; SQL Server reports `tinyint`, so
  `ISNULL(j.[type], 0)` is `int` too.
