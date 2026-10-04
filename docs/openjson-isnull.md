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
- The NVARCHAR and NCHAR widths that ISNULL applies for a known bounded
  first argument (`__msduck_isnull_nvarchar_width`,
  `__msduck_isnull_nchar_width`) accept VARCHAR text and carriers and
  return text, as the predicate pins convert carrier columns mixed with
  text. Previously a carrier from an aggregate subquery, as in
  `N'text' + ISNULL((SELECT MAX(v) FROM ...), N'')`, reached the
  VARCHAR-only width function and failed to bind (found while verifying
  #901); a carrier result would also have failed in comparisons with
  literals. An unpaired surrogate in such a bounded result becomes U+FFFD.

The multirow trigger from the report, ISNULL over OPENJSON keys and values
in projections and stored results, and ISNULL over aggregated subqueries now
match SQL Server.

The predicate catalog now declares direct OPENJSON sources before lowering:
`key` is NVARCHAR(4000), `value` is NVARCHAR(MAX), and `type` is integer.
WITH columns use their explicit declarations: NVARCHAR/NCHAR are carriers,
VARCHAR/CHAR are backend text, and other types stay non-text. These declarations
come from the AST, independent of the document value or returned rows. Default
and explicit aliases are recognized; ambiguous aliases and renamed column lists
remain unknown. Quoted `[@p]` columns stay distinct from unquoted scalar `@p`.
This lets the existing comparison and alternative-expression lowering handle
COALESCE, IIF, CASE, predicates and ordering over direct OPENJSON sources.

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
  counts. Known differences assert msduck's complete current result. Nine previously
  failing complete cases now use the unchanged reference expectations.
- `tests/openjson_isnull.rs` checks the native ISNULL dispatch (carrier code
  units including an unpaired surrogate, carrier replacements of text,
  integer and date first arguments), stored ISNULL results over OPENJSON,
  the trigger, and both overloads of the ISNULL width functions.
- `tests/compat/apply_full_join.test.mjs` now expects SQL Server's rows for
  its ISNULL trigger case.

## Remaining differences

The tedious test lists them exactly:

- Explicit WITH schemas now execute, but COALESCE may still advertise
  NVARCHAR(MAX) instead of the declared VARCHAR(10) or NVARCHAR(10).
  In the retained mixed-schema case, a VARCHAR value `'x '` compared with
  `N'x'` still returns false instead of SQL Server's true. Catalog recognition
  does not repair the later ANSI comparison or result descriptor adapters.
- The two direct OPENJSON FULL JOIN sources now ignore trailing spaces in
  their ISNULL comparison, matching the captured rows and DONE count. The
  COALESCE key descriptor remains NVARCHAR(MAX), rather than NVARCHAR(4000).
- OPENJSON columns propagated through an inline function remain unknown to
  this conservative statement catalog; the captured function query still
  fails when COALESCE mixes its returned carrier with text.
- ISNULL with a VARCHAR first argument and a carrier replacement holding
  characters outside Windows-1252 fails on the wire instead of returning
  `?`; the same happens for VARCHAR and NVARCHAR variables.
- Bounded NVARCHAR/NCHAR ISNULL results are text, so an isolated
  surrogate in a carrier first argument becomes U+FFFD. Stored columns
  already behaved this way through the predicate pins before this change
  (verified against the previous lowering with a TDS-parameter value); the
  aggregate-subquery form previously failed to bind. Keeping exact units
  needs carrier-aware comparison of these results, which belongs to the
  predicate lowering.
- A non-text replacement of a Unicode first argument converts with
  DuckDB's text cast, so `ISNULL(j.[value], CAST(1 AS BIT))` returns
  `true` and dates lose their SQL Server style. NVARCHAR variables and
  stored columns already behaved this way through the existing ELSE branch;
  SQL Server's character formatting is applied by AST conversion lowering,
  which the bind-time macro cannot reach.
- The OPENJSON `type` column is `int`; SQL Server reports `tinyint`, so
  `ISNULL(j.[type], 0)` is `int` too.
