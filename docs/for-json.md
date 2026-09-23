# FOR JSON implementation boundary

Top-level and nested SELECT FOR JSON PATH execute through `msduck_core::for_json`.
`msduck-sql::for_json` validates syntax and aliases; the root adapter binds the
source with DESCRIBE, converts typed Arrow values and emits NVARCHAR(MAX) through
TDS. Preparation binds without executing source rows. Native sequence coverage
checks this and verifies one evaluation per row during execution.

Supported options are INCLUDE_NULL_VALUES, ROOT and WITHOUT_ARRAY_WRAPPER.
Explicit aliases, ordinary named columns and bound stars are supported. TOP,
ORDER BY, OFFSET/FETCH and DISTINCT remain in the existing source query pipeline.
The output column uses SQL Server's JSON_F52E2B61-18A1-11d1-B105-00805F49916B name.
Empty input retains that metadata and produces the serializer's empty output.
Direct JSON_QUERY expressions and nested FOR JSON arrays are promoted; ordinary
strings remain quoted. Nested WITHOUT_ARRAY_WRAPPER output stays quoted unless
JSON_QUERY explicitly promotes it, as documented by Microsoft.
Integer/decimal values retain exact spelling; bits become booleans, binary values
use padded Base64, and MONEY/SMALLMONEY values remain exact scale-four JSON numbers.
Temporal and GUID values become strings. DATETIMEOFFSET JSON removes the space
between civil time and offset in the native adapter's text representation.

The current adapter returns one PLP string row within the server's 16 MiB response
limit; it does not reproduce SQL Server's large-result row chunking. Client tests
cover 6000 ordered Unicode rows crossing native batches. Known projection errors
are rejected during batch preflight, before preceding DML. Star-dependent alias
conflicts are rejected after binding but before evaluating the source. Error
numbers 13601/13603/13605/13620 are mapped; diagnostic wording and states are not
yet differentially verified.

The plan compiles ordered column aliases into a flat object tree. Dots introduce
nested objects. Empty aliases, empty path components, duplicate leaves,
leaf/object collisions and reopening an object after another property are
rejected before processing rows. Entry order is explicit; a deterministic map
is used only for name lookup. Planning, rendering and dropping a deep plan do
not rely on recursive object ownership or rendering calls.

Input values distinguish SQL NULL, text, booleans, validated lexical numbers and
validated JSON fragments. Numbers are never converted through floating point;
JSON fragments keep their original spelling, whitespace and duplicate keys.
Text and property names use SQL JSON escaping, including forward slashes.
The adapter must explicitly promote JSON_QUERY/nested FOR JSON output rather
than guessing that every string containing JSON should be embedded.

The row writer omits SQL NULL properties by default, including nested objects
whose children were all omitted. A non-NULL JSON null or explicitly supplied
empty object remains present. INCLUDE_NULL_VALUES retains SQL NULLs. The result
writer preserves supplied row order, defaults to an array, supports ROOT, and
supports WITHOUT_ARRAY_WRAPPER (including comma-separated multi-row output).
ROOT plus WITHOUT_ARRAY_WRAPPER is rejected. A row-width error leaves accumulated
output unchanged. Adapters can inspect buffered UTF-8 bytes to apply resource
limits; wire size and framing remain adapter concerns.

Tests cover nested paths, property order, omission, promoted versus quoted JSON,
control/Unicode escaping, exact large numbers, option combinations, empty results,
invalid fragments, rejected aliases, failed-row atomicity, deep paths and 6000
ordered rows. Core and client tests are local evidence, not SQL Server differential results.

## Nested execution

Nested and correlated PATH queries lower to a derived source, a native typed-row
serializer and an aggregate wrapper. The source retains its projection, ORDER BY,
TOP, DISTINCT and OFFSET/FETCH. DuckDB performs the correlation; the adapter does
not issue a separate database request for each parent. Empty children produce an
empty array. Nested ROOT and INCLUDE_NULL_VALUES use the same core rules.

`msduck-sql` owns the AST wrapper and chooses an internal alias absent from the
source identifiers. The root binds fragment/temporal descriptors,
reads only validated live native vector slots, and calls the existing core writer.
The final aggregate wrapper validates and frames serialized rows in the core.
Native conversions retain decimal coefficients, UUID bits, binary bytes, exact
DATETIME2/DATETIMEOFFSET scales, TIME precision, and the currently supported
integer/bit/sysname SQL_VARIANT tags. No Arrow/SQL/native types enter the core.

Source annotation precedes wrapper lowering. Annotating the generated wrapper
again would insert SQL conversions around already typed native fields, including
an invalid integer-only conversion of SQL_VARIANT metadata strings. Nested JSON
queries have one NVARCHAR(MAX) result for compiler/catalog inference, regardless
of the shape of their source projection.

Client coverage includes correlated arrays, deeper nesting, quoted unwrapped
output and explicit JSON_QUERY promotion, prepared reuse, variables (DECLARE,
SET and SELECT assignment), INSERT and views. A 6000-row nested result covers
Unicode and omitted NULLs across native batches. A native sequence test verifies
that preparation never executes a JSON variable initializer and that runtime
source expressions execute once per row. Known local catalog/derived-table stars and inherited nonrecursive CTE stars are
supported. Enclosing CTE snapshots retain renamed columns, MONEY identity and TIME
scale across declaration chains. Each definition receives the preceding scope;
body subqueries receive the completed scope. Unresolved/self-recursive declarations
shadow same-named base tables without contributing guessed columns. A star whose
source metadata is only partially known is rejected instead of dropping columns.
Correlated named-column references now inherit enclosing row metadata as well.
Qualified stars can now resolve enclosing row sources; complete recursive CTE
inference remains open.

## Adapter work remaining

- Complete fragment provenance through views, set operations, OPENJSON AS JSON
  and compound expressions. Direct JSON_QUERY and nested FOR JSON arrays now
  retain provenance through derived/CTE columns, aliases and stars.
- Complete unresolved-source inference and recursive CTE types. Known correlated
  column references, qualified stars and inherited nonrecursive CTE stars retain
  money identity and temporal scale through explicit snapshots.
- Implement AUTO-mode source grouping, joins and parent/child nesting.
- Support FOR JSON over set operations and a FOR JSON clause directly combined
  with SELECT INTO/variable-assignment projections. Nested scalar JSON expressions
  already work in assignments and INSERT. Unsupported contexts report errors.
- Complete logical result inference for all SQL_VARIANT families and computed
  temporal expressions. Currency uses exact decimal-number serialization;
  it no longer requires a separate currency flag in the JSON descriptor.
- Implement SQL Server's large-result row chunking and verify DONE row counts.
- Compare alias case/collation behavior, nesting limits, numeric/temporal spellings,
  empty results and diagnostic state/precedence against a live SQL Server.

The plan follows the ordered path-tree approach in upstream mssqlite
`packages/transpile/src/for-json.ts`, reviewed at the revision in
[reference review](reference-review.md). Its SQLite-specific JSON aggregation and
patching code was not copied into the deterministic core.

References:

- [Microsoft FOR JSON options](https://learn.microsoft.com/en-us/sql/relational-databases/json/format-query-results-as-json-with-for-json-sql-server)
- [Microsoft PATH nesting](https://learn.microsoft.com/en-us/sql/relational-databases/json/format-nested-json-output-with-path-mode-sql-server)
- [Microsoft JSON errors, including path conflicts and incompatible options](https://learn.microsoft.com/en-us/sql/relational-databases/errors-events/database-engine-events-and-errors-13000-to-13999)

## Initial outer-query verification (2026-09-22)

Formatting, all 242 Rust tests, Clippy with warnings denied, and all 292 independent
client/harness tests passed. The local capture completed 190 cases. The new JSON
case's four result sets were checked exactly, including NVARCHAR(MAX) metadata.
Of 189 prior executions, 188 were identical; the un-ordered `derived table apply`
case returned the same two rows in reversed order. The raw difference was retained,
not normalized into a match. No live SQL Server differential run was available.


## Nested-query verification (2026-09-22)

Formatting, all 244 Rust tests, Clippy with warnings denied, and all 295 client
and harness tests passed after the nested-query implementation. The local audit
completed 191 cases. The new correlation/promotion/variable case's three result
sets matched the asserted JSON strings exactly. Of 190 prior executions, 189
were identical; the un-ordered `derived table apply` case again reversed its two
rows. Its raw difference was retained. These checks are local evidence, not a
live SQL Server differential run.

## Inherited CTE verification (2026-09-22)

Formatting, all 245 Rust tests, Clippy with warnings denied, and all 296 client
and harness tests passed. The new client regression reproduced the previous
unknown-star failure before the change, then passed with CTE chains, renamed
columns, money/time metadata, base-table shadowing and prepared reuse. The catalog
regression also verifies that unresolved declarations cannot borrow later/base
metadata and that partially known star projections remain unknown.

The local capture completed 192 cases. All 191 previous executions were identical
to the preceding capture. The added CTE JSON case's two result sets matched the
expected strings exactly. This remains local evidence, not a live SQL Server
comparison.


## Correlated row metadata

Nested JSON binding now receives enclosing FROM-source snapshots as well as CTE
snapshots. Qualified and implicit outer column references retain MONEY identity
and TIME scale through multiple scalar-subquery levels. Local columns take
precedence, including a local column whose logical type is unknown. Ambiguous
names, matching local qualifiers and unresolved source scopes stop lookup rather
than borrowing a farther outer declaration. CTE definitions do not inherit the
containing row's scope. These are metadata rules; existing SQL binding still
controls whether an expression may reference an outer row.

The client regression reproduced MONEY being emitted as a number and TIME(2)
being expanded to seven fractional digits. It now checks exact strings, NULLs,
local DECIMAL shadowing, implicit outer qualification, CTE-sourced rows, multiple
nesting levels and prepared reuse. A catalog regression separately covers unknown
local types, qualifiers, ambiguity and unresolved-scope barriers.

## Correlated type verification (2026-09-22)

Formatting, all 246 Rust tests, Clippy with warnings denied, and all 297 client
and harness tests passed. The new client regression failed before the change
with a numeric MONEY value and seven-digit TIME text, then passed with exact
strings, shadowing and prepared reuse. The local audit completed 193 cases; all
192 previous executions were identical. The new correlated-type case's two
result sets matched the expected JSON strings exactly. No live SQL Server
comparison was performed.

## Forwarded JSON fragments

Projection fields now carry expression provenance separately from their SQL type.
Direct JSON_QUERY results and array-wrapped FOR JSON results retain this marker
through derived tables, CTE aliases, star expansion and direct correlated column
references. Both outer serialization and nested native serialization consume it.
Generated array wrappers retain the marker after AST lowering.

Plain text, persisted NVARCHAR columns and WITHOUT_ARRAY_WRAPPER results are not
promoted merely because their contents happen to be valid JSON. Local text columns
shadow outer fragments, and ambiguous or unresolved scopes stop provenance lookup.
The marker is not persisted in sys.columns. JSON_QUERY can explicitly promote an
unwrapped result. This follows Microsoft's documented
[JSON_QUERY promotion rule](https://learn.microsoft.com/en-us/sql/relational-databases/json/solve-common-issues-with-json-in-sql-server?view=sql-server-ver17);
the forwarding tests are local regressions, not live SQL Server comparisons.

The client regression first reproduced a derived JSON_QUERY result being double
escaped, then checks CTE chains, nested arrays, unwrapped text, correlated columns,
local shadowing, storage, NULL and prepared reuse. A pure SQL-crate regression
covers alias/star propagation and ambiguous/unknown scope barriers. General
expression, set-operation, view-definition and OPENJSON AS JSON provenance remain
unimplemented; this marker does not claim complete JSON-expression inference.

## Forwarded fragment verification (2026-09-22)

The new client regression failed before the change with a quoted derived fragment,
then passed with direct forwarding, alias/star propagation, nested and correlated
output, text shadowing, persisted text, NULLs and prepared reuse. Formatting,
254 workspace Rust tests, strict Clippy and 298 client/harness tests passed.
All 194 audit cases completed; the prior 193 execution captures were unchanged.
The new case's three result sets matched expected JSON strings exactly. General
expression provenance and live SQL Server comparison remain outstanding.

## Correlated qualified stars

Nested PATH queries can now select an enclosing row with `p.*`. Source resolution
searches from the nearest local scope outward. A matching local alias shadows an
outer alias; ambiguous qualifiers and unresolved-source scopes stop lookup. CTE
definitions do not borrow outer row bindings. Field order, MONEY identity, TIME
scale and JSON-fragment provenance are retained.

DuckDB rejects a correlated star with no local FROM clause, so the SQL crate
expands bound qualified stars into explicit column references before JSON lowering.
References retain their original qualifier and quote each column name. The pass
preserves unqualified stars and other projection expressions, and never copies
source expressions. The root adapter supplies catalog snapshots and invokes the
pass; no database access or expression evaluation occurs inside expansion.

This follows the SELECT qualification and correlated-name rules described in
Microsoft's [SELECT clause](https://learn.microsoft.com/en-us/sql/t-sql/queries/select-clause-transact-sql?view=sql-server-ver17)
and [subquery reference](https://learn.microsoft.com/en-us/sql/relational-databases/performance/subqueries?view=sql-server-ver17).
Tests cover local alias shadowing, nested scopes, mixed local and outer projection
items, quoted names, NULLs, logical types, fragments and prepared reuse. A native
sequence regression covers preparation and single evaluation across 6,000 rows.
These are local checks, not a live SQL Server differential result.

## Qualified-star verification (2026-09-22)

The client regression first reproduced missing outer-source metadata; after lookup
was added, it reproduced DuckDB's correlated-star binding error. Explicit expansion
then passed the full regression. Formatting, strict Clippy, all 256 workspace Rust
tests and all 299 client/harness tests passed. The native sequence test uses a
cataloged 6,000-row input and verifies preparation performs no evaluation and
execution advances the sequence exactly once per source row. Unknown table-function
metadata remains outside supported star inference.

All 195 audit cases completed. The prior 194 execution captures were unchanged,
and the new correlated-star case's two result sets matched expected strings exactly.
No live SQL Server differential comparison was performed.

## Consistent CTE metadata visibility

Standalone projection binding and nested-query visitors now share declaration
snapshot construction. Unresolved self/forward references cannot borrow a
same-named base table or enclosing CTE's columns. Schema-qualified base references
and valid preceding-declaration chains remain available. A pure regression
reproduced the prior mismatch between the two paths and checks both APIs against
the same explicit catalog. This fixes metadata visibility; complete CTE validation,
recursive type inference and canonical diagnostics remain separate work.


## Live currency serialization correction (2026-09-22)

Live SQL Server 2025 RTM-CU7 captures supersede the historical assumptions above
that MONEY should serialize as a string. MONEY and SMALLMONEY serialize as JSON
numbers, retaining all four fractional digits, including zero and both range
endpoints. Explicit character conversions remain strings. The reference capture
is `artifacts/compatibility/sql-server-json-currency.json`.

The adapter now sends currency's exact decimal spelling through the core's
validated lexical-number writer, without any floating-point conversion. The
currency-name exception and nested descriptor's currency flag were removed;
fragment promotion and temporal scale descriptors remain. Raw JSON assertions
cover endpoints beyond JavaScript's exact-number range, nested output, NULLs,
prepared execution and explicit character conversion. Existing CTE, correlated
and star tests now assert numeric currency output.


The corrected local audit completed all 274 cases. Of 273 preceding executions,
268 were unchanged; four JSON cases changed currency fields from quoted text to
numbers. Three now have rows identical to the preserved SQL Server rows; the
fourth (`FOR JSON PATH typed projection and options`) retains the independent
empty-result difference. The unordered `derived table apply` query returned its
same rows in reverse order. Raw differences remain in
`artifacts/compatibility/json-currency-local-diff.json`; none were normalized.
Both result sets of the new endpoint probe exactly match the live reference
rows. These are row comparisons, not claims of complete metadata/event matches.


Final verification: formatting, strict Clippy, all 323 workspace Rust tests and
all 338 client/harness tests passed, with zero failures, cancellations or skips.
The isolated live reference container was removed and its VM stopped; the
original Docker context remained unchanged.
