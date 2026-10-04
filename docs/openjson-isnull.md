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
  `__msduck_isnull_nchar_width`) preserve their input family. Text returns
  text; a carrier returns a carrier bounded directly in UTF-16 units,
  with NCHAR padding and NULL validity preserved. No lossy Unicode decoding
  occurs on the carrier path. This includes aggregate-subquery results.
- Scalar-query binding avoids repeating the first query across native type
  prototypes; see [binding evidence and scope guards](isnull-subquery-binding.md).


The multirow trigger from the report, ISNULL over OPENJSON keys and values
in projections and stored results, and ISNULL over aggregated subqueries now
match SQL Server.

The predicate catalog now declares direct OPENJSON sources before lowering:
`key` is NVARCHAR(4000), `value` is NVARCHAR(MAX), and `type` is integer.
WITH columns use their explicit declarations: NVARCHAR/NCHAR are carriers,
VARCHAR/CHAR are backend text, and other types stay non-text. These declarations
come from the AST, independent of the document value or returned rows. Query
and SELECT frames keep nested/sibling sources separate and give local aliases
priority over correlated outer sources. Projection aliases do not shadow source
columns inside SELECT expressions; ORDER BY consumers resolve output aliases
against the projection explicitly. Scalar-query projection checks use that
query's frame; set-operation pinning resolves each branch separately. Default
and explicit aliases are recognized; ambiguous aliases, derived/CTE columns and
renamed column lists remain unknown. Same-named physical tables retain the
existing conservative shared description. Quoted `[@p]` columns stay distinct from unquoted scalar `@p`.
ISNULL keeps its direct OPENJSON carrier first argument: its native dispatch
already packs replacements into that type, so declaration pinning must not
convert it to text and lose exact UTF-16 units or change stored values. Character-only COALESCE, IIF and CASE alternatives with direct OPENJSON
sources instead convert each result branch to a common Unicode carrier
declaration. This preserves raw UTF-16 through literals, declared parameters
and ANSI columns, while conditions and selected operands retain their original
evaluation. Nested admitted alternatives retain this carrier provenance. ANSI branches
convert through their declared CP1252 family before Unicode promotion; under
the supported non-SC collations a supplementary ANSI literal contributes two
best-fit bytes. Numeric and unresolved alternatives keep the existing path.
Set branches align each immediate child result using its own query scope.
An inner DISTINCT finishes in its own character domain before a numeric parent
converts its completed output: `01 UNION 1 UNION ALL 7` therefore retains both
rows that become integer 1. ANSI child results convert through their own
declarations before Unicode promotion. Character branches use their common
Unicode declaration; proved numeric peers retain
SQL Server numeric precedence via explicit conversions of the completed branch
result. Unknown peers stay unresolved. The physical alternative’s internal
conversion remains unchanged. Distinct UNION and INTERSECT/EXCEPT membership
use the existing default character equality keys and NULL-safe equality, while
preserving the selected raw payload and materializing membership inputs once.
For the captured UNION cases, input-branch priority retains the first branch’s
case-equivalent payload. This is evidence for those queries, not a universal
claim about SQL Server representative selection under arbitrary execution plans.
The default key is gated against explicit unknown or nondefault collations and
declared nondefault column collations. Such domains retain the existing native
set path; their full collation-aware set compatibility remains unproved.
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
  counts. Known differences assert msduck's complete current result. Ten previously
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
- A non-text replacement of a Unicode first argument converts with
  DuckDB's text cast, so `ISNULL(j.[value], CAST(1 AS BIT))` returns
  `true` and dates lose their SQL Server style. NVARCHAR variables and
  stored columns already behaved this way through the existing ELSE branch;
  SQL Server's character formatting is applied by AST conversion lowering,
  which the bind-time macro cannot reach.
- The OPENJSON `type` column is `int`; SQL Server reports `tinyint`, so
  `ISNULL(j.[type], 0)` is `int` too.

### Additional raw-unit evidence

Native tests cover isolated high/low surrogates through COALESCE, CASE and IIF,
NULL fallbacks from literals, declared parameters and ANSI columns, bounded
NVARCHAR widths, common NCHAR widths, carrier peers and UNION branches. Native
sequence controls check that selected volatile leaves execute once and unselected
fallbacks do not execute. The fixed-width result adapter introduced after root
annotation preserves native carriers rather than stringifying their STRUCT.

Additional pinned SQL Server 17.0.4065.4 captures distinguish dynamic NCHAR(3)
with VARCHAR(5) alternatives (NVARCHAR(5)) from constant-folded COALESCE. They
also expose a remaining source conversion difference: an OPENJSON NCHAR(3)
containing only U+D800 is short in SQL Server, and conversion to the common
NCHAR(5) yields U+D800, two NUL units and two spaces. msduck currently pads the
source with spaces and yields U+D800 followed by four spaces. The native test
records msduck’s current value; it does not establish SQL Server parity for that
source conversion. Full-width NCHAR source controls avoid this separate gap.

The additional set tests retain complete rows rather than trimming values to
force equality. Distinct character sets compare keys and select an unchanged
payload, preferring the shorter binary prefix for equal trailing-space keys.
Membership wrappers name their generated CTE output columns explicitly and
avoid referenced user relation names. Nested distinct operators retain their
own equality behavior beneath UNION ALL; parenthesized ANSI literals retain
their full best-fit width.
