# SQL Server compatibility work

The objective remains a fully functional Microsoft SQL Server compatible
server in Rust backed by DuckDB. This list tracks implementation, not a
redefinition of that objective. No complete-compatibility claim is justified
by the initial smoke tests.

## Current foundation

- Rust workspace, bundled DuckDB, SQL Server AST parser/translator. Optional
  SSH build/test offloading with an isolated Linux cache and gitignored local
  configuration; see docs/remote-build.md.
- Deterministic core, SQL and TDS crates; backend-independent catalog type
  records, declaration-shape rules and lexical column lookup. Batch parsing,
  syntax normalization, RPC declarations and batch declaration/reference preflight
  now run in `msduck-sql`, alongside view and DDL syntax checks. Projection inference,
  operand binding and shared expression metadata rules run there over explicit
  snapshots without a database connection. Acquisition, remaining execution lowering, wire
  result metadata and execution still live in the root.
- TDS packet reassembly/splitting with bounds checks; PRELOGIN, LOGIN7,
  environment changes, login acknowledgement, error/completion tokens.
- Shared database with independent client sessions; basic relational batches,
  DDL/DML, TOP, bracket identifiers, Unicode literals, transactions,
  correlated CROSS/OUTER APPLY, SQL Server NULL ordering, COUNT/COUNT_BIG.
- sp_executesql RPC with bound integer/boolean/float/Unicode/binary values,
  exact decimal/numeric inputs up to 38 digits, scaled money inputs, and
  Windows-1252 varchar inputs (including PLP), DATE, and exact TIME inputs.
- Legacy datetime/smalldatetime RPC decoding with range/width validation;
  microsecond storage and datetime2 result metadata remain approximations.
- Uniqueidentifier RPC inputs and typed GUID output, UUID storage and scalar
  rebinding, plus volatile NEWID expressions and persisted defaults; SQL Server GUID ordering and conversion rules remain unfinished.
- Connection-local prepare/execute/prepexec/unprepare RPCs, validated without
  execution, typed binding, handle outputs, release and resource bounds.
- Transaction-manager begin/commit/rollback with shared SQL session state,
  nested counts, names, descriptor notifications/validation, and restart flags.
- Batch-local scalar DECLARE/SET/SELECT assignment, scalar subqueries and
  parameterized reuse in SELECT/DML; duplicate declaration preflight.
- IF/ELSE and BEGIN/END execution with batch scope, variable preflight and
  final completion handling for skipped branches.
- Nested WHILE/BREAK/CONTINUE, single-statement and grouped bodies, execution
  step limits and a whole-batch response cap.
- RETURN batch exit, signed RPC status, NULL-status INFO diagnostics and
  transaction-preserving early completion.
- PRINT informational diagnostics with known character-family length limits;
  full type inference and live token comparison remain unfinished.
- Explicit THROW with structured application error number/state, bounded
  Unicode messages, and batch-aborting error completion.
- TRY/CATCH runtime recovery, nested ERROR_* contexts, bare rethrow and
  context restoration across loop exits and RETURN; line attribution remains approximate.
- Session-local @@ERROR for batch diagnostics, structured THROW numbers and
  successful-statement reset, with initializer preservation.
- Compound SET operators, known-integer division/modulo with zero-divisor
  errors, bitwise XOR and known-string concatenation.
- Compound SELECT variable assignments with typed conversion and empty-result
  preservation; dependent evaluation order and full operand typing remain unfinished.
- Prepared ordinary/compound SELECT assignments to RPC inputs, validated without
  execution and rebound per invocation; application OUTPUT parameters remain unfinished.
- Prepared scalar DECLARE/SET batches with nonexecuting initializer validation
  and fresh local scope; prepared DDL remains unfinished.
- Prepared IF/WHILE/block/TRY-CATCH traversal, loop control, RETURN/THROW/PRINT
  and supported transactions without execution; prepared IF/WHILE check bound
  Boolean metadata, while full predicate grammar/type distinctions remain unfinished.
- Prepared supported session settings with shared validation and deferred
  NOCOUNT changes; complete SET behavior and nested option scopes remain unfinished.
- Search-condition preflight for control flow, query/DML filters, joins and
  searched CASE, rejecting scalar BIT/numeric coercion; specialized contexts
  and complete scalar/predicate operand rules remain unfinished.
- CREATE TABLE CHECK predicate validation, UNKNOWN acceptance and error 547
  with atomic failed writes; ALTER/trust/catalog and full constraint semantics remain unfinished.
- IIF-to-CASE lowering, predicate and NULL-constant validation, and ten-level
  CASE/IIF nesting; complete result coercion and evaluation semantics remain unfinished.
- CASE/IIF known integer/character result precedence using declared/cast/literal
  types, simple CASE comparison conversion and all-NULL result validation;
  full source inference and other type-family promotion remain unfinished.
- CHOOSE indexed CASE lowering with INT conversion, NULL bounds and known
  integer/character result precedence; full type/evaluation semantics remain unfinished.
- DATETIME2 time-only text uses the 1900-01-01 base date with exact fractions
  and rounding carry; locale-dependent formats remain open.
- DATENAME returns English names and exact numeric text through the temporal
  conversion path with bounded NVARCHAR(30) metadata for known projections;
  other languages and broader descriptor propagation remain open.
- DATEPART now extracts exact calendar/clock fields and ISO weeks with INT metadata;
  DATEFIRST 1–7 is session-local and visible through @@DATEFIRST; integer day offsets
  use checked legacy datetime conversion. Fractional numeric inputs
  and full diagnostics remain open (docs/datepart.md). Missing DATE/TIME fields
  now report 9810 before their conversion can supply default components.
- Explicit NVARCHAR casts enforce lengths 1–4000 (default 30), text truncation,
  numeric display overflow and TRY NULLs, with bounded result metadata. Stored
  column widths, full formatting and collation behavior remain open (docs/nvarchar.md).
- ISNULL known NVARCHAR results now bound replacements by UTF-16 units;
  isolated-surrogate truncation and complete collation behavior remain open.
- ISNULL first bound type and literal-NULL fallback, integer replacement
  conversion, prepared parameters and stored views (widths/full conversions pending).
- NULLIF comparison/result type separation for known integer/character and DATETIME2 inputs
  and literal-NULL first argument validation; full comparison semantics pending.
- Ordinary comparisons share known integer/character coercion across predicate
  contexts, IN/NOT IN lists and BETWEEN; source-column inference and full type/collation
  semantics pending.
- Mixed known integer/character arithmetic converts operands before evaluation,
  with nested INT/BIGINT and character result inference; full arithmetic pending.
- Native integer SUM/AVG use bounded accumulators, exact truncated averages,
  typed NULLs, DISTINCT, grouping, windows and parallel state combination.
  Catalog and CTE/derived projection inference supplies integer input types;
  other numeric families, complete inference and arithmetic options remain pending.
- PERCENT_RANK/CUME_DIST validate ordered, frameless windows and retain FLOAT(53)
  wire results, including ties, NULLs and singleton partitions. See
  [distribution windows](docs/distribution-windows.md); live reference comparison
  and broader floating-point coercion remain pending.
- Window offsets require unsigned integer literals before batch execution;
  reversed frame boundaries report 4193. See [window frames](docs/window-frames.md)
  for coverage and remaining size-limit and diagnostic verification.
- STDEV/STDEVP/VAR/VARP translate to sample/population aggregates with FLOAT(53)
  results and shared validation. See [statistical aggregates](docs/statistical-aggregates.md)
  for tests and remaining input-type and floating-point reference checks.
- PERCENTILE_CONT/DISC translate partitioned ordered-set windows, preserving
  floating interpolation and discrete integer types. See [percentiles](docs/percentiles.md)
  for covered cases and remaining input validation, type and diagnostic gaps.
- An exact DATETIME2 core and scale-aware TDS codec cover year 1–9999 at 100ns.
  SQL casts, RPC and table columns use exact tagged values and declared-scale
  result metadata. INSERT/UPDATE, defaults and ALTER COLUMN coerce target scales;
  DATE/TIME extraction, YEAR/MONTH/DAY and EOMONTH accept the tagged type. Binary
  comparisons, BETWEEN, IN lists and simple CASE normalize known operands across
  scales. CASE/COALESCE/IIF/CHOOSE choose the highest known result scale;
  ISNULL and NULLIF preserve their first argument's scale. IN subqueries compare known DATETIME2 values through exact ticks. Set operations and VALUES combine known DATETIME2 scales. Broader inference, indexes
  and other temporal operations remain open.
  See [DATETIME2 implementation](docs/datetime2.md).
- ABS applies known operand result widths, DECIMAL precision and scale, and
  integer overflow; source-column and general function inference remain pending.
- CEILING/FLOOR use known numeric result types with exact BIGINT identities,
  decimal rounding and nested numeric inference; full inference remains pending.
- SIGN retains known numeric result types including DECIMAL precision/scale;
  source-column inference and invalid-type diagnostic parity remain pending.
- Numeric functions infer decimal/scientific literals and unary signed inputs;
  ordinary expressions also type large integers and decimal literals explicitly;
  full decimal arithmetic and automatic parameterization remain pending.
- Shared INT conversion now distinguishes valid text overflow (248) from malformed
  text (245) and numeric overflow (8115); other-width text diagnostics remain open.
- Explicit NCHAR casts now truncate/pad UTF-16 widths and retain fixed metadata;
  known CASE/COALESCE/IIF/CHOOSE branches pad to their largest width. Stored
  widths remain open; known set branches now pad before duplicate comparison.
- NCHAR produces BMP characters with NCHAR(1) metadata and known conditional
  result inference. Isolated surrogates and SC collations remain open (docs/nchar.md).
- UNICODE uses a native first-UTF-16-unit function for current non-SC semantics,
  including columns, defaults and typed NULLs; full collation support remains pending.
- SPACE implements integer count conversion, negative/NULL results and bounded
  generation. Direct projections retain bounded VARCHAR result descriptors;
  known CHAR/SPACE set-operation branches combine their descriptors. Wildcard,
  general set-operation and stored-object descriptor propagation remain pending.
- CHAR decodes Windows-1252 codes with NULL/range behavior and direct CHAR(1)
  metadata; other code pages and full conversion/descriptor propagation remain pending.
- CASE/COALESCE/IIF/CHOOSE combine known text result descriptors and NULLIF
  preserves its first descriptor; general character inference remains pending.
- ISNULL truncates/pads inferred CHAR/SPACE replacements to the first width
  and retains that descriptor; declared/Unicode character widths remain pending.
- Known TINYINT negation promotes to SMALLINT with unary integer type propagation;
  complete source inference and unary operator parity remain pending.
- Native bitwise NOT overloads preserve BIT and integer widths, including source
  columns and typed NULLs; binary operands/full conversion rules remain pending.
- Native AND/OR/XOR overloads cover every BIT/integer pairing with width promotion
  and NULL propagation; binary operands/full conversion rules remain pending.
- SELECT INTO persistent typed destinations, prepared binding, completion counts,
  set-operation sources and two-step failure/transaction behavior; full metadata
  and complete identity semantics pending. Integer IDENTITY now uses persistent
  sequences with shared, non-rollback allocation and explicit-write checks; see
  docs/identity.md for remaining catalog, retrieval, reseed and lifecycle work.
  sys.identity_columns exposes persisted definitions and live last values through
  integer SQL_VARIANT results. SQL_VARIANT_PROPERTY supports integer base-type,
  precision/scale and length properties. Explicit integer casts and TRY conversion
  unwrap integer variants; reverse casts retain integer base types through SELECT
  INTO and views. Declared variant columns support integer assignments, defaults
  and ALTER conversion. Integer predicates and direct ordering compare by value;
  conditional results preserve integer variant payloads. General variant
  operations and deduplication remain unfinished. BIT payloads now support storage,
  properties, predicates and explicit BIT/integer conversion.
  ALTER TABLE ADD can populate an integer identity column with transactional
  cleanup and WAL recovery. DROP TABLE removes the private sequence transactionally. IDENT_SEED/IDENT_INCR
  read persistent original definitions with NUMERIC(38,0) metadata. IDENT_CURRENT
  reports cross-connection allocation state, including rollback and reopen.
- TOP branch limits for UNION/INTERSECT/EXCEPT, including prepared queries,
  stored views and SELECT INTO; NULL/negative integer count validation and
  OFFSET/FETCH conflict rejection; PERCENT/WITH TIES and full argument rules pending.
- OFFSET/FETCH expression parsing, prepared counts, integer ranges and required
  query-clause checks, including nested FETCH count subqueries; noninteger typing
  and full diagnostics remain pending.
- COALESCE known integer/character precedence and untyped-NULL validation;
  full type inference, metadata and evaluation semantics remain unfinished.
- Persisted CREATE VIEW/CREATE OR ALTER VIEW queries, atomic replacement,
  transactional creation/alteration and DROP VIEW; full SQL Server view semantics remain unfinished.
- ALTER TABLE ADD/DROP and ALTER COLUMN type/nullability with atomic DDL groups and nullable/default
  population rules including WITH VALUES; remaining constraint forms are explicit gaps.
- User CREATE/DROP SCHEMA namespaces with qualified tables/views, transactional
  DDL and persistence; sys.schemas and schema lookup functions expose persistent IDs.
  User table/view object IDs now persist through transactional DDL and restart,
  exposed through sys.objects and OBJECT_ID/OBJECT_NAME/OBJECT_SCHEMA_NAME.
  sys.columns retains table declarations across CREATE/ALTER, rollback and restart,
  including byte lengths and numeric/temporal precision. Views and SELECT INTO
  propagate direct source/cast types through aliases, joins, wildcards, derived
  tables and non-recursive CTEs; full expression inference, constraint IDs and
  advanced column metadata remain unfinished.
  sys.types and TYPE_ID/TYPE_NAME expose the 34 reviewed built-in definitions;
  user-defined types and version-specific type catalogs remain unfinished.
  sys.tables/sys.views expose ordinary objects and typed default feature flags;
  physical storage metadata and exact catalog descriptors remain unfinished.
  Column IDs retain gaps through ALTER and restart; COL_NAME and COLUMNPROPERTY
  ColumnId/AllowsNull/IsIdentity, declared Precision/Scale, UsesAnsiTrim and core
  feature flag lookups are implemented; other properties remain unfinished.
  Ownership, complete object/column catalogs and embedded elements remain unfinished.
- Whole-table TRUNCATE with transaction rollback, preserved defaults and
  incoming foreign-key checks and transactional integer identity reset;
  partition/self-reference behavior remains unfinished.
- LEN trailing-space/UTF-16 behavior, known MAX result typing and binary
  literal translation; collation and full column-type inference remain unfinished.
- ASCII-space default LTRIM/RTRIM/TRIM and explicit trim character sets;
  compatibility-level and full type/collation rules remain unfinished.
- YEAR/MONTH/DAY INT results for dates, timestamps, time-only inputs and
  integer base-date offsets, with stored defaults, prepared parameters and
  single input evaluation through calendar conversion;
  full numeric and date-string conversion semantics remain pending.
- EOMONTH native DATE results with month offsets, calendar bounds, NULLs,
  error recovery and persisted defaults; full date-input conversion remains pending.
- DATEFROMPARTS native DATE results, Gregorian validation, NULL propagation,
  catchable calendar errors and persisted defaults; implicit argument conversion remains unfinished.
- Integer CAST/TRY_CAST and style-free CONVERT/TRY_CONVERT with numeric
  truncation, exact decimal boundaries and known money rounding; implicit DML conversions and full provenance remain unfinished.
- FLOAT precision buckets and REAL widths across casts, declarations, storage
  and ALTER COLUMN; full floating-point range/formatting semantics remain unfinished.
- Integer INSERT target conversion for VALUES/SELECT/CTE sources, defaults,
  prepared inputs and atomic failures; full source-type inference and MERGE conversions remain unfinished.
- Integer UPDATE assignment conversion, defaults, CTE completion and atomic
  failure checks; full target resolution and MERGE remain unfinished.
- Compound UPDATE arithmetic, bitwise and known-string assignments with catalog
  target typing; full operand inference and numeric promotion remain unfinished.
- UPDATE target aliases in flat INNER/CROSS FROM join trees with preserved
  predicates; outer/lateral trees and updatable view/CTE targets remain unfinished.
- MERGE parse-time terminator and match-family action validation, including
  CTE-prefixed statements, nested blocks and preparation; execution remains pending.
- DELETE optional/two-FROM syntax, flat INNER/CROSS target aliases and CTE
  completion counts; TOP and writable CTE/view targets remain unfinished.
- OUTPUT native inserted/deleted images, typed OUTPUT INTO destinations and
  parameterized projections over captured images, including joined old/new/source
  UPDATE images with key changes and outer joins. Joined DELETE, writable derived
  targets and generated-column images,
  complete failure streams and statement undo within
  explicit transactions remain unfinished. Declared NVARCHAR/NCHAR storage also
  needs lossless raw UTF-16 writes; see docs/output.md and
  reference/unicode-character-storage.json.
- Typed nullable result metadata, PLP text/binary, decimal/date/time codecs.
- Reference skills and compatibility corpus preserved with provenance; all
  17 cases plus twenty-seven focused probes executable through a local capture harness.
  An opt-in isolated-database SQL Server comparison records exact differences;
  a live reference run remains outstanding.

See [the initial verification and corpus baseline](docs/compatibility-baseline.md)
for concrete evidence and observed failures.

## Required next work and evidence

1. Execute copied corpus through a reusable capture harness against msduck and
   a real SQL Server. Compare metadata, rows, tokens, errors and reuse, retaining
   exact discrepancies. Extend independent tiberius/tedious/ODBC/.NET tests.
2. Complete typed SQL semantics: catalog of declarations, exact character and
   numeric widths, nullability, conversion/precedence, collation/padding,
   aggregates, arithmetic, dates/timezone, GUIDs, money, XML and variant.
3. Complete language: error control flow, dynamic SQL, stored procedures,
   scalar/table functions, views, triggers, cursors, identity/sequences,
   OUTPUT/MERGE, temp objects, error handling, all supported session settings.
4. SQL Server catalogs and information schema, metadata procedures, databases,
   schema/object resolution and persistence/restart coverage.
5. Remaining RPC types and application output parameters, prepare-time result
   metadata/native plan caching, TVPs, transaction
   manager savepoints/distributed transactions and full isolation/error semantics.
6. TLS and SQL authentication, negotiated features/version handling, MARS,
   bulk-load streaming/atomicity, cancellation/interrupt, reset semantics.
7. SQL Server transaction/error behavior, isolation/concurrency, rollback and
   disconnect, savepoints, nested transaction state, statement-vs-batch errors.
8. Bound and stream results; connection admission, timeouts, clean shutdown,
   observability, malformed-input fuzzing and multi-client stress tests.

Current explicit gaps include SQL-managed principals/permissions and advanced TLS modes, unsupported advanced
statements/catalogs, no active-query cancellation, materialized responses,
16 MiB limits, approximate metadata/errors and backend semantic differences.
Never silently convert an unsupported feature into a successful no-op.

- Explicit VARCHAR CAST/CONVERT/TRY forms support CP1252-representable text,
  byte limits, numeric overflow and bounded/MAX result descriptors. Lossy/best-fit
  conversion, collations, formatting styles and full source-type formatting remain open.

- Explicit CHAR/CHARACTER casts and no-style CHAR CONVERT/TRY forms pad fixed
  CP1252 byte widths and retain typed results. Known CHAR conditional and set
  expressions now normalize common fixed widths; storage enforcement, mixed
  type families and full collation behavior remain unfinished.

Verified increment: IS [NOT] DISTINCT FROM uses numeric keys for integer/BIT
variants and exact ticks for DATETIME2, preserving NULL equality across different
base tags/scales. Full variant DISTINCT/GROUP BY, set and index equality remain
open; local audit evidence is not live SQL Server comparison.

Verified increment: variant ORDER BY output aliases and ordinal positions use
numeric payload keys, preserving projected base types and volatile evaluation,
with TOP/OFFSET/FETCH applied after sorting. Full variant set/group/index equality
and ordering with unknown output provenance remain open.

Verified increment: integer/BIT variant common types in UNION ALL and VALUES,
including CTE/derived inference, predicates, ordering, paging, prepared inputs and
assignments. UNION DISTINCT/INTERSECT/EXCEPT equality remains open.

Verified increment: SELECT DISTINCT on known integer/BIT variants compares
numeric payloads and repeated NULLs, preserving representative payload types and
applying TOP/paging afterward. GROUP BY, DISTINCT aggregates and set-operation
deduplication remain open.

Verified increment: DISTINCT variant ordering through ordinary/qualified stars
and mixed projections now retains source identity, aliases and ordinal positions
across joins, TOP and prepared CTE paging. Unknown projection provenance and
broader variant grouping/set/index semantics remain open.

Verified increment: UNION numeric variant deduplication with representative
payloads, NULL equality, tuple keys, nested ALL boundaries and outer ordering.
INTERSECT/EXCEPT, grouping, distinct aggregates and index equality remain open.

Verified increment: integer/BIT variant INTERSECT/EXCEPT membership, NULL equality,
complete tuple keys, left-side representatives, CTEs and prepared paging. Broader
families, exact nullability metadata, grouping and index equality remain open.

Verified increment: COUNT/COUNT_BIG DISTINCT numeric variant equality, NULL and
empty-input behavior, grouped/HAVING and prepared expression paths; known variant
APPROX_COUNT_DISTINCT inputs reject with 8117. Other aggregate/group/index semantics
and noninteger variants remain open.

Verified increment: numeric aggregate variant type validation returns 8117 before
execution or preparation, including empty inputs, windows, DISTINCT and CTEs.
Explicit numeric casts remain supported.

Verified increment: MIN/MAX numeric variant extrema preserve base types through
grouped and window aggregation, CTE inference, prepared conditionals and NULLs.
Other payload families and aggregate catalog type
propagation remain open.

Verified increment: integer/BIT variant window partition keys use numeric equality
across stored base types. Inline and inherited named windows, multiple keys, NULLs,
CTEs and prepared conditional keys are covered. Broader payload families remain
unfinished.

Verified increment: numeric integer/BIT variant GROUP BY keys, projected payloads,
HAVING, qualified names, wildcard projections, grouped windows, CTEs and prepared
conditional keys. ROLLUP/CUBE/GROUPING SETS preserve subtotal NULLs and duplicate
sets. Noninteger families, correlated grouped subquery resolution, full grouping
syntax validation/limits and exact GROUPING/GROUPING_ID metadata remain open.

Verified increment: GROUPING/GROUPING_ID return TINYINT/INT with typed empty results,
conditional inference and SELECT INTO catalog declarations. Tests cover reversed
argument bit order, NULL subtotals, prepared HAVING and variant keys. Complete
context validation, exact error messages and 32-bit mask boundary behavior still
need SQL Server ground truth.

Verified increment: grouping-set counts are bounded before expansion, including
products and duplicate totals (10703); advanced grouping checks 32 distinct
expressions using resolved source identities (10706). Nested CUBE/ROLLUP inside
GROUPING SETS lower to explicit bounded sets. Boundary validation, prepared
rejection, failed INSERT atomicity and qualified duplicate references are tested.
Legacy grouping modifiers, complete syntax validation and live SQL Server
comparison remain unfinished.

Verified increment: legacy WITH CUBE/WITH ROLLUP parsing and lowering, duplicate
expression removal using source identities, NULL subtotals, DISTINCT aggregates,
variant keys and prepared expressions. Mixed legacy/modern constructs return
10702; the legacy 12-expression boundary is enforced. GROUP BY ALL semantics,
exact boundary error diagnostics and live SQL Server comparison remain open.

Verified increment: explicit GROUP BY ALL parsing and retained-group semantics.
WHERE is materialized once per source row and aggregate inputs are conditional;
excluded groups return zero counts and NULL sums/extrema. Coverage includes
HAVING, prepared NULL parameters, joins, CTEs, wildcard projections, windows,
variant keys and a 6,000-row volatile predicate. Correlated grouped subqueries,
remote/FILESTREAM restrictions and live SQL Server comparison remain open.

Verified increment: SELECT alias visibility checks for WHERE/GROUP BY/HAVING in
resolved source scopes, including prepared rejection and failed INSERT atomicity.
Tests preserve source-name collisions, ORDER BY aliases, CTE/derived columns,
nested scopes, parameters and datepart keyword syntax. Unknown-source and
correlated outer-alias binding, complete grouping validation and live SQL Server
comparison remain open.

Verified increment: GROUP BY aggregate/subquery rejection (144, severity 15)
before lowering, including CASE/COALESCE, quoted aggregate names, grouping sets,
legacy syntax and GROUP BY ALL. Prepared rejection and unchanged INSERT targets
are covered; valid derived aggregate outputs and SELECT-list subqueries remain
accepted. User-defined aggregate resolution, remaining grouping binding rules and
live SQL Server comparison remain open.

Verified increment: constant-only GROUP BY expressions reject with 164, severity
15, including datepart syntax, grouping sets and legacy/ALL forms. Empty grouping
sets and constant-valued derived columns remain valid. This syntactic check does
not establish whether an identifier binds only to an outer query; correlated
binding and live SQL Server comparison remain open.

Verified increment: error 164 for grouping keys bound exclusively to enclosing
expression queries, including qualified/unqualified names, nested correlations,
CTE source outputs and prepared calls. Local shadowing and mixed local/outer
expressions remain valid. CTE definitions do not inherit the containing SELECT's
source scope. Unknown-source resolution, complete correlation support across
APPLY/set-query boundaries and live SQL Server comparison remain open.

Verified increment: parenthesized query/set branches preserve grouping correlation
and error 164 across UNION, INTERSECT and EXCEPT. The dialect accepts scalar
subqueries beginning with parenthesized set branches, with recursion limits and
arithmetic fallback preserved. Multiple CTE definitions retain separate binding
boundaries. Complete APPLY and unknown-source binding and live SQL Server
comparison remain open.

Verified increment: APPLY grouping scopes include the left input and enclosing
queries while excluding later joins. Direct/prepared outer-only keys reject with
164, and valid local or mixed keys preserve CROSS/OUTER APPLY row semantics.
Tests cover chained APPLY, local shadowing, nested correlation and failed INSERT
atomicity. Parenthesized join-tree resolution, table-valued function source types,
unknown sources and live SQL Server comparison remain open.

Verified increment: unaliased parenthesized join trees resolve source columns
recursively and retain source order, qualifiers, type annotations and ambiguity.
APPLY lowering and scope traversal recurse into nested join trees. Tests cover
INT aggregate metadata, variant grouping, alias/outer-only rejection, CROSS APPLY
row omission and OUTER APPLY NULL extension. Aliased join groups, APPLY with a
joined-tree right input, unresolved table-valued sources and live SQL Server
comparison remain open.

Verified increment: joined APPLY right inputs retain external grouping scope
across nested constituents and add the appropriate internal left scope for an
inner APPLY. Direct/prepared rejection, later-source isolation, duplicate rows,
NULL extension, rebinding and failed INSERT atomicity are covered. Aliases on
parenthesized join groups are absent from the documented T-SQL grammar; their
exact rejection diagnostics remain unverified. Complete binding for table-valued
sources and join-predicate subqueries and live SQL Server comparison remain open.

Verified increment: temporary ON-expression scopes isolate current join inputs,
including nested joins and joined APPLY dependencies. Direct/prepared tests cover
outer-only keys, later-source collisions, nested subqueries, local shadowing,
WHERE scope restoration and failed INSERT atomicity. Missing-name diagnostics,
DML-specific join scopes, unresolved table-valued sources and live SQL Server
comparison remain open.

Verified increment: UPDATE/DELETE join predicates bind before target-tree lowering.
Parsing still checks unsupported target shapes early on a copy. Original DML
source frames use a resolved target scope and preserve ON/APPLY visitation.
Tests cover direct/prepared 164 rejection, later-qualifier 4104, CTE UPDATE,
unchanged rows and valid prepared rebinding/deletion. Complete unqualified-name
binding through DML lowering, unresolved sources, full 4104 coverage and live
SQL Server comparison remain open.

Verified increment: ON column bindings survive UPDATE/DELETE lowering by retaining
resolved source qualifiers. The previously captured unqualified correlated
GROUP BY failure is addressed. Tests cover local shadowing, filters, prepared
rebinding, sort expressions, output alias precedence and DATEPART unit/column
name collisions. Ambiguous and unresolved bindings remain for the binder.
Complete missing-name coverage, unresolved table-valued sources and live SQL
Server comparison remain open. A separate runtime probe also confirmed DATEADD
is currently unresolved (208); implementing it remains part of compatibility work.

Verified DATEADD increment: typed DATE arithmetic now covers calendar dateparts,
month-end clamping, signed fractional INT truncation, NULLs, empty metadata,
prepared rebinding, column/unit-name collisions, views and stored defaults.
Native tests cover chunk boundaries and DATE range limits. The prior unresolved
DATEADD probe is addressed for typed DATE. DATETIME2/TIME/DATETIMEOFFSET,
legacy DATETIME/SMALLDATETIME, string literals and newer BIGINT offset behavior
remain open; other input families reject explicitly. Live SQL Server comparison
and precise invalid-argument/NULL error precedence remain unverified.

Verified DATEADD extension: DATETIME2 scales 0–7 retain exact ticks through
calendar/subday additions, NULL/empty results, derived/scalar/correlated queries,
prepared calls, mixed-scale comparisons/CASE, views and stored defaults.
Arithmetic checks large offsets before narrowing and returns 517 for range
violations. Negative nanosecond ties and coarse-scale rounding have local audit
captures but still need live SQL Server comparison. Legacy temporal families,
TIME, DATETIMEOFFSET, string-literal return semantics and BIGINT offsets remain.

Verified binding correction: temporal source columns named after dateparts no
longer rewrite function keywords during generic type annotation. DATEADD,
DATEPART and DATENAME work inside aggregates, CASE/COALESCE, predicates,
grouping and windows with same-named value arguments preserved. The local audit
now captures the previously failing aggregate/CASE/predicate forms. Full
unresolved-source binding and live SQL Server comparison remain open.

DATEDIFF/DATEDIFF_BIG now count exact datepart boundaries for DATE, DATETIME2,
TIME, ISO text and integral day offsets. Coverage includes Sunday weeks under
DATEFIRST changes, 100ns precision, signed INT/BIGINT overflow, metadata,
prepared rebinding, aggregate typing, defaults/views and atomic DML failure.
DATETIMEOFFSET, legacy DATETIME/SMALLDATETIME coercion, fractional numeric dates,
locale/dateformat-sensitive input, exact diagnostics and live comparison remain.

DATETIMEOFFSET transport foundation added: exact UTC ticks plus retained signed
offset, both local/UTC range checks, scale rounding with day carry, bounded TDS
0x2B RPC decoding and result metadata/value encoding. Reused upstream mssqlite
wire vectors and exercised all offsets/scales. SQL storage/casts/assignment,
UTC comparison/grouping, date-function integration and live reference validation
remain required before claiming DATETIMEOFFSET SQL support.

DATETIMEOFFSET SQL integration now covers casts/TRY conversions, typed results,
RPC parameters, variables, columns/defaults, INSERT/UPDATE, ALTER scale rounding
and persistence. UTC keys drive scalar predicates, joins, IN/subqueries and
simple CASE. Offsets survive storage and restart. The earlier cast-208 audit case
is addressed. UTC GROUP BY/DISTINCT/set equality, mixed-scale conditional/set
results, conversion out to other types, date functions and live comparison remain.

DATETIMEOFFSET GROUP BY now merges equal UTC instants across offsets, retains a
typed representative payload, and preserves actual NULL groups versus subtotal
NULLs. GROUPING indicators, rollups, repeated grouping sets, HAVING/order aliases,
wildcards, mixed variant keys and prepared calls have coverage. Redundant
same-scale casts are canonicalized without discarding intervening rounding.
DISTINCT/set operations, mixed-scale results, date functions and live SQL Server
comparison remain unfinished.

DATETIMEOFFSET DISTINCT and set operations now compare by UTC, normalize offset
scales and retain representative payloads. Native vector tests verify single
evaluation and offset preservation; tedious tests cover NULLs, tuples, metadata,
prepared queries and paging. Full mixed temporal coercion/metadata, conditional
results, date functions and live SQL Server comparison remain open.

DATETIMEOFFSET conditional results now normalize scales for CASE/COALESCE and
lowered IIF/CHOOSE, retain ISNULL's first-argument scale, and preserve NULLIF's
first result type. Native tests cover lazy evaluation and retained offset payloads
across chunks. Full mixed temporal coercion, date functions and live SQL Server
comparison remain open.

DATETIMEOFFSET DATE/TIME extraction and assignment now retain local components
and exact ticks, including offsets crossing UTC date boundaries. YEAR/MONTH/DAY
share local extraction. DATETIME2 conversion, legacy temporal conversions, text
formats, complete TIME assignment-scale handling and remaining date functions
still need work and live SQL Server comparison.

Audit evidence: TIME(3) values round correctly but result metadata still reports
scale 7 (Type::Time is unparameterized and its TDS descriptor hardcodes 7).
Preserving declared TIME scale through expressions, storage and wire encoding
is required; the 145-case audit retains this mismatch.

TIME wire types now carry scale and use the matching 3/4/5-byte payload. Direct
CAST/CONVERT, supported conditional/set expressions, bound TIME parameters and
catalog-backed direct columns (including wildcards and CTEs) retain declared
result scales. Broader expression provenance and storage assignment rounding
still need work; this descriptor change does not establish their completeness.

TIME INSERT/UPDATE target conversion now reads declared scale from sys.columns
and rounds before storage. ALTER COLUMN applies its original TIME declaration
to existing values. ADD WITH VALUES uses a rounded default for population but
restores the original default expression, preventing double rounding after a
later scale change. Tests compare stored values in predicates and grouping;
complete TIME expression provenance remains open.

TIME persistence now has explicit restart and rollback coverage: native raw ticks,
retained defaults and catalog scales across two reopens, plus client checks for
failed-update atomicity and rollback of values, descriptors and added columns.
173 Rust tests and 252 client tests pass. Broader TIME expression provenance and
remaining temporal operations still require implementation and reference checks.

TIME ISNULL now converts its replacement to the first argument's scale before
predicates, assignments and materialization. Catalog TIME declarations participate
in expression annotation; ISNULL column results retain first-argument metadata.
Native tests verify single evaluation across chunks, and client tests cover
rounding, midnight carry, lazy branches, prepared rebinding and empty results.
Broader mixed temporal conditional precedence and TIME provenance remain open.

TIME CASE/COALESCE and lowered IIF/CHOOSE now convert selected text branches to
the inferred TIME scale before predicates or materialization. Conditional column
metadata is retained through catalog inference for supported TIME/text sources.
Native vector tests check NULLs, exact stored ticks and lazy branch evaluation;
client tests cover mixed scales, text, SELECT INTO, prepared reuse and empty sets.
Complete mixed temporal precedence and general expression provenance remain open.

DATETIMEOFFSET COUNT DISTINCT and COUNT_BIG DISTINCT now use exact UTC equality.
Tests distinguish equal instants across offsets from adjacent 100ns instants,
retain INT/BIGINT result widths and verify NULL/empty/grouped/prepared behavior.
A native test checks single evaluation over 6,000 inputs. Full temporal function
coverage and live SQL Server comparison remain open.

DATETIMEOFFSET window partition and ordering keys now use UTC instants. Ranking
and RANGE frames share equal-instant peers across offsets, preserving NULL and
100ns distinctions. Tests cover aggregate/ranking windows, mixed tuple keys,
prepared execution and native vector chunks. Volatile PARTITION BY expressions
are evaluated twice per row in the DuckDB 1.5.5 baseline; single evaluation and
SQL Server reference behavior remain to be resolved. Broader
temporal functions and live SQL Server reference comparison remain open.

ORDER BY alias precedence now survives typed equality-key rewriting. A SELECT
alias that shadows a DATETIMEOFFSET or SQL_VARIANT source column is ordered by
the projected value; window ORDER BY expressions retain source binding. The
regression checks numeric/text aliases, DISTINCT and paging. The prior offset
rewrite incorrectly sorted by the hidden source column in this situation.

DATEDIFF/DATEDIFF_BIG now accept typed DATETIMEOFFSET through an exact UTC adapter,
retaining NULLs and existing checked INT/BIGINT boundary counts. Client and native
tests cover equal instants, offsets, 100ns precision, source scales, overflow and
prepared reuse. Local-clock DATETIME2 casts and remaining offset-aware date
functions remain separate work; live reference comparison remains open.

DATEPART/DATENAME now accept typed DATETIMEOFFSET at scales 0–7, extract local
calendar/clock components, and preserve signed timezone offsets as minutes or
HH:MM text. Native chunk checks and client tests cover UTC/local date boundaries,
NULLs, metadata, DATEFIRST, scale rounding and single evaluation. DATEADD,
local-clock DATETIME2 casts, broader language/text formats and live reference
comparison remain open.

DATETIMEOFFSET-to-DATETIME2 conversion now retains local ticks and discards the
offset for casts, TRY conversions, assignments and ALTER COLUMN. Native tests
cover every source/target scale pair, vector chunks and single evaluation;
client tests cover metadata, prepared reuse, midnight carry and overflow.
Reduced-scale behavior uses DATETIME2 rounding and still needs live comparison
because the documentation's fractional conversion wording is inconsistent.
Offset-bearing text, DATEADD and remaining mixed temporal semantics stay open.

DATETIMEOFFSET DATEADD now preserves local calendar arithmetic, exact fractional
ticks, input scale and fixed offset. Source-column annotation now includes
DATETIMEOFFSET, fixing its prior fallback to the DATE-only path. Native chunk
and client tests cover NULLs, month-end clamping, nested calls, prepared execution,
updates, views and both local/UTC overflows. TIME/legacy DATEADD, BIGINT amounts
and live reference checks remain open.

TIME DATEADD now supports hour, minute, second, millisecond, microsecond and
nanosecond arithmetic with midnight wraparound and scale preservation. Calendar
units reject with 9810. Source-column binding and result catalog inference retain
TIME metadata for stored/view/empty results; native tests cover INT limits,
chunked NULL inputs and single evaluation. Legacy temporal DATEADD, BIGINT
amounts and live reference checks remain open.

TIME aggregates and value windows now retain their input scale instead of
falling back to TIME(7). Type inference supports MIN/MAX, FIRST_VALUE/LAST_VALUE
and LAG/LEAD through composed expressions and catalog-backed views. LAG/LEAD
TIME defaults now convert to the first argument's scale before consumers see
them, fixing finer-scale values hidden by wire rounding. Client tests cover
metadata, defaults, DATEPART/DATEADD, NULLs, empty results and prepared reuse;
a 6,000-row native test checks physical default rounding and one evaluation.
Other temporal default families and broader descriptor provenance remain open.

The aggregate/window audit additionally exposed lost TIME declarations in inline
VALUES sources. Homogeneous TIME/NULL VALUES columns now infer the maximum
TIME scale for binding and result catalog metadata, including window defaults.
Mixed non-TIME VALUES columns remain outside this inference path.

LAG/LEAD now convert DATETIME2 and DATETIMEOFFSET defaults to the first value's
declared scale through native temporal converters. This fixes backend STRUCT
cast failures for mixed-scale defaults and enables ISO text defaults. Native
checks exercise all scales across 6,000 rows, retained offsets and single
fallback evaluation; client checks cover exact values, NULLs, metadata, outer
DATEPART/DATENAME expressions and prepared reuse. Default expression error timing
and live SQL Server comparison remain open.

EOMONTH integer dates now use checked day offsets from 1900-01-01 instead of
DuckDB's unsupported integer-to-DATE cast. Tests cover range endpoints, NULLs,
DATE metadata, prepared execution and one input evaluation across vector chunks.
A probe also verifies the existing local-month behavior for DATETIMEOFFSET.
Fractional numeric coercion and live reference comparison remain open.

DATETIME2 text casts now accept offset-bearing ISO date/time and time-only
strings while retaining local fields. Shared suffix validation rejects malformed
or out-of-range zones; DATETIMEOFFSET still separately checks UTC/local ranges.
The DATETIME2 path preserves its full local range and scale rounding. Tests cover
all scales, vector chunks, NULLs, TRY conversion, assignments and prepared reuse.
Timezone-only literals, locale formats and live reference checks remain open.

SWITCHOFFSET now changes fixed offsets while retaining exact UTC ticks and the
input DATETIMEOFFSET scale. Native code handles signed integer minutes and
±HH:MM text, NULLs and UTC/local range validation, with errors 9812/9813. Tests
cover all scales and offsets across chunks, single input evaluation, source
columns, UPDATE, prepared calls, metadata and date-boundary changes. Broader
numeric/text coercion, exact error precedence and live SQL Server checks remain.
The inspected upstream date-functions module has no SWITCHOFFSET implementation;
this path uses the existing Rust DATETIMEOFFSET codec and Microsoft's reference.

TODATETIMEOFFSET now attaches signed-minute or ±HH:MM offsets to local DATETIME2
fields, preserving scale and deriving exact UTC ticks. It shares native offset
validation with SWITCHOFFSET while using a separate local-to-UTC construction
path. Tests cover all scales/offsets, vector NULLs, single evaluation, UPDATE,
prepared execution, result metadata, composition and boundary overflow. No
TODATETIMEOFFSET implementation was found in the inspected upstream engine or
transpiler; behavior follows Microsoft's function reference and the Rust codec.
Broader coercions, error precedence and live SQL Server comparison remain open.

TIME set-source inference now retains the maximum scale when both branches have
known TIME declarations. This repairs stored-column set metadata and composed
CTE/derived/view queries, aggregates, DATEADD and LAG/LEAD default conversion.
Tests cover UNION ALL/UNION/INTERSECT/EXCEPT, nested sets, NULL/empty results and
6000-row physical rounding with one default evaluation per row. Unknown and
non-TIME branches remain outside this inference; mixed-family coercion and live
SQL Server comparison remain open. The copied upstream TIME cast smoke test
does not cover these composed set descriptors. Reference: Microsoft's
[TIME](https://learn.microsoft.com/en-us/sql/t-sql/data-types/time-transact-sql)
and [UNION](https://learn.microsoft.com/en-us/sql/t-sql/language-elements/set-operators-union-transact-sql)
documentation; the existing direct-cast common-scale rule is now propagated to
source metadata.

TIMEFROMPARTS now has an exact native constructor for all eight TIME scales,
with integer component conversion, checked ranges, error 289 and NULL handling.
Tests cover 6000-row batches at every scale, independent input evaluation,
prepared execution, typed defaults/storage, views, composed sets and empty
metadata. Upstream provides related DATEFROMPARTS/DATETIMEFROMPARTS coverage but
no TIMEFROMPARTS implementation was found. Precision-diagnostic parity, broader
conversion/error precedence and live reference comparison remain open; see
[implementation notes](docs/timefromparts.md).

DATETIME2FROMPARTS now constructs exact DATETIME2(0–7) values, validates calendar
and fractional fields with error 289, and propagates NULL components. Constant
integer precision expressions are evaluated before lowering; unsupported or
invalid scale expressions report 10760. Tests cover all scales across 6000-row
chunks, single input evaluation, year bounds/leap dates, prepared calls,
default/storage rounding, views and temporal/set composition. No corresponding
implementation was found in the inspected upstream packages. Full constant
expression forms, exact diagnostics/coercions and live comparison remain open;
see [constructor notes](docs/datetime2fromparts.md).

TIMEFROMPARTS now shares DATETIME2FROMPARTS's checked constant precision evaluator
and error 10760 mapping. The lowering runs before runtime arithmetic transforms,
so arithmetic/bitwise constants retain their declared scale through prepared
calls, views, set queries and window defaults. Tests cover these compositions,
NULL/empty metadata and rejection of volatile precision without evaluation.
Additional constant forms, exact range/overflow diagnostics and live SQL Server
comparison remain open. This follows the Microsoft 10760 catalog's constant
expression rule; no corresponding upstream precision implementation was found.

DATETIMEOFFSETFROMPARTS now constructs exact UTC ticks and signed offsets at all
eight scales, reusing calendar/fraction and constant-precision validation. It
checks matching offset signs, ±14-hour bounds and both UTC/local date ranges.
Tests cover every valid offset, NULL vectors, single input evaluation, prepared
execution, defaults/storage, views and composed temporal/set queries. No upstream
implementation was found. Live comparison, NULL-offset/error precedence, exact
diagnostics and wider coercion/precision expressions remain open; see
[constructor notes](docs/datetimeoffsetfromparts.md).

ISJSON now implements JSON syntax validation and default/explicit root types.
Its iterative scanner adapts upstream lexical rules while avoiding recursive
calls, numeric conversion and tree construction. Native tests cover malformed
syntax, duplicate keys, deep nesting, NULL vectors and single evaluation;
client tests cover root constraints, Unicode, prepared calls, CHECK constraints,
integer expression inference and empty metadata. Live reference comparison,
noncharacter coercion, depth/Unicode edges and compatibility-level gating remain
open. OPENJSON and extraction functions are still unfinished; see
[JSON validation](docs/isjson.md).

JSON_VALUE/JSON_QUERY now share source-preserving path extraction adapted from
upstream JSON rules. Native functions support scalar/container results, first
matching duplicate keys, lax/strict modes, typed NULLs and JSON_VALUE's UTF-16
width limit. Tests cover vectors, single input evaluation, prepared paths,
Unicode bounds, views and empty metadata. Full-document prevalidation differs
from SQL Server's possible early-match behavior; exact error details, complete
path grammar, collation, isolated surrogates and live comparison remain open.
OPENJSON remains unfinished. See [extraction notes](docs/json-extraction.md).

JSON extraction now follows paths without prevalidating unrelated suffixes. It
validates preceding siblings and selected values, and validates the full document
for missing paths and root extraction. Tests cover nested/array early matches,
errors before matches, malformed missing-path documents, prepared recovery and
6000-row NULL vectors. This replaces the copied upstream full-tree parsing order
using Microsoft's documented search behavior. Exact validation order at wrong
kinds, truncated ancestors/root scalars and live comparison remain open.

JSON extraction diagnostics now share number/state/message classification for
uncaught execution, control-flow expression failures, TRY/CATCH and bare rethrow.
Strict wrong-kind errors retain state 2, using the inspected upstream error
identities; native backend prefixes are removed from known JSON messages.
Tests cover direct/prepared errors, nested catch restoration and explicit THROW
identity. Syntax-error position details, remaining precedence and live SQL Server
comparison remain open.

OPENJSON default-schema extraction now adapts the upstream row/type rules with
native LIST/STRUCT vectors and lateral relational lowering. Tests cover source
slices, duplicate keys, array indexes, strict error states, large child vectors,
single input evaluation, prepared recovery, long values, APPLY and view/empty
metadata. The client fixture also exposed optional INSERT INTO translation,
which is now handled. Explicit WITH conversion, variable paths, exact key/value
collations and key limits, syntax positions/validation order and live comparison
remain open; see [OPENJSON notes](docs/openjson.md).

OPENJSON WITH now uses source-row slices and separate scalar/fragment extraction
before declared-type conversion. It adapts upstream case-sensitive paths, strict
column error states and AS JSON semantics, enforces NVARCHAR(MAX) fragments, and
rejects SQL Server's prohibited schema types. Native tests cover chunk boundaries,
single evaluation and long scalar text; client tests cover typed projections,
prepared recovery, nested APPLY and empty/view metadata. Binary Base64 conversion
is explicitly unsupported, and variable paths, full coercion, collations, exact
validation order/diagnostics and live comparison remain open.

OPENJSON variable paths now use a token-level parser adaptation limited to the
second argument, followed by normal variable binding. Batch validation and view
validation inspect the parser's path field explicitly. Tests cover prepared path
changes, NULLs, error recovery, unexecuted-branch validation, comments/nesting and
view capture prevention. Arbitrary path expressions and full path grammar, binary
conversion, collation fidelity and live SQL Server comparison remain open.

OPENJSON BINARY/VARBINARY schemas now decode Base64 JSON strings with size checks
and fixed-width padding. Native functions return BLOB vectors; bounded binary
result descriptors and TDS encoding preserve declared widths through source
metadata, views and stored imports. Tests cover NULL vectors, single evaluation,
malformed/padded encodings, overflow, prepared recovery and long PLP values.
Non-string coercion, exact permissive encoding rules, general binary casts/storage
and live SQL Server comparison remain open.

OPENJSON WITH character lengths now use declaration defaults before cast lowering,
fixing unbounded VARCHAR/CHAR and 30-unit Unicode defaults. Shared normalized
types drive source inference and catalog metadata; bounded character provenance
restores VARCHAR/CHAR/NCHAR result families as well as NVARCHAR. Client tests cover
empty/NULL values, explicit widths/MAX, aliases, prepared execution, views and
unchanged ordinary CAST defaults. General character storage enforcement,
collation propagation and live reference comparison remain open.

Character storage targets now restore declared families and widths from catalog
metadata before INSERT/UPDATE. Native converters count Windows-1252 bytes or
UTF-16 units, preserve NULLs, pad fixed types and allow excess trailing spaces
under supported ANSI_WARNINGS ON behavior. DDL defaults and ALTER COLUMN use the
same conversion. Tests cover atomic writes, prepared recovery, failed narrowing
and persisted declarations/defaults. Legacy 8152 is emitted; verbose 2628,
ANSI_WARNINGS OFF/legacy ANSI_PADDING, noncharacter coercion, additional code
pages and failed-transaction control-flow evaluation remain open. See
[storage notes](docs/character-storage.md).

DATALENGTH now distinguishes Windows-1252, UTF-16 and binary storage and preserves
NULL/empty values, trailing spaces and MAX result widths. Catalog scopes restore
character and binary declarations for tables, views, CTEs and OPENJSON schemas.
BIT/integer widths are supported. Decimal, floating-point, temporal, SQL_VARIANT,
general computed character inference, additional collations and live reference
comparison remain open; see [DATALENGTH notes](docs/datalength.md).

Length-function inference now retains character families and capacity through
UPPER/LOWER and LTRIM/RTRIM/TRIM. LEN resolves catalog-backed MAX columns and
returns BIGINT through CTEs, views and empty results. Persisted function result
descriptors retain variable-width VARCHAR/NVARCHAR families. Vector and prepared
checks cover NULLs and single input evaluation. Other character functions,
conditional/concatenation inference, exact collation-dependent case conversion
and live reference comparison remain open.

DATALENGTH now uses declared SQL numeric storage widths rather than DuckDB's
physical sizes. Decimal precision bands, money/smallmoney and real/float are
covered through literals, casts, parameters, tables, views and CTEs, with typed
NULL/empty results. Catalog scope restoration distinguishes money from decimal
backing storage. Vector tests retain NULLs and single volatile-input evaluation.
Temporal/variant representations, general arithmetic-result inference and live
SQL Server comparison remain open.

The Rust workspace now extracts deterministic temporal/JSON rules into
`msduck-core` and deterministic TDS codecs into `msduck-tds`. The root retains
DuckDB/Arrow adapters, sessions, RPC dispatch and transport I/O. Pure tests run
without DuckDB; root-side integration tests and public import paths are preserved.
See [architecture](docs/architecture.md) for measured local build observations,
dependency rules and the remaining SQL compiler/execution boundary work.

DATALENGTH now covers DATE, DATETIME, SMALLDATETIME and all declared TIME,
DATETIME2 and DATETIMEOFFSET scales, including constructors and CAST/CONVERT.
Legacy datetime CONVERT without styles follows the existing CAST path. Tests
cover stored and prepared types, NULL/empty results, views, CTEs and chunked
single evaluation. SQL_VARIANT, general computed-expression inference and live
SQL Server comparison remain open.

Character conversion extraction now gives the core validated family/length values,
separate storage and CAST operations, typed overflow/encoding errors, and shared
Windows-1252 conversion. Unicode casts borrow their input when possible. TDS
reuses encoding via a one-way dependency on the core. AST/native-vector adapters
retain NULL/TRY behavior; nullable native VARCHAR policies no longer read invalid
vector slots. Full compiler type/parameter/diagnostic extraction remains open.

JSON path parsing, source selection and JSON_VALUE/JSON_QUERY extraction now run
in the deterministic core. OPENJSON shares those helpers directly, and pure
extraction tests run without DuckDB. AST lowering, vectors, NULLs, metadata and
backend diagnostic normalization remain adapters. Other SQL value rules remain candidates for further extraction.

OPENJSON default-row, explicit-schema selection and Base64 conversion rules now
live in the core, alongside their existing pure tests. SQL declarations, metadata,
coercion ASTs and native vector handling remain in adapters. Whole-document
validation and OPENJSON-specific diagnostic states are preserved. Backend-independent parameter/type contracts remain open.

A shared core SqlError now carries runtime error number/state/message across JSON
classification, session TRY/CATCH/THROW and TDS encoding. Typed identities survive
anyhow context without message reclassification. Backend wrapper removal and
legacy severity selection stay adapters. Full diagnostic coverage, severity and
line/procedure context, and backend-independent parameters/types remain open.

JSON_PATH_EXISTS now uses deterministic lexical path traversal for property,
index and array-wildcard paths. JSON null/empty containers count as present;
invalid inputs return 0 and SQL NULL propagates. Chunked single evaluation,
prepared/stored values, predicates, views/CTEs and empty INT metadata are tested.
Live validation-order comparison, advanced paths/native JSON, and exact argument
coercion/diagnostics remain open; see [path existence](docs/json-path-exists.md).

STRING_ESCAPE now has deterministic JSON escaping rules in the core, including
forward slashes, all control characters, Unicode and unbounded expansion. Root
adapters retain MAX metadata, NULLs, format/arity diagnostics and single argument
evaluation. Prepared recovery, views/CTEs, stored defaults and LEN/DATALENGTH
are tested. Live argument-policy/diagnostic comparison, collation details and
isolated surrogate handling remain open; see [STRING_ESCAPE](docs/string-escape.md).

FOR JSON PATH now has a deterministic typed planner/row writer in the core,
adapting upstream ordered path trees. It validates aliases, retains exact numeric
and JSON text, distinguishes SQL NULL from JSON null, and supports omission,
ROOT and array-wrapper options. This is a serialization foundation only: SQL
binding/execution, logical SQL value conversion, correlated queries, AUTO mode,
metadata/wire output and live comparison remain open. See [FOR JSON](docs/for-json.md).

RPC parameters and local variable bindings now carry backend-independent core
scalar values, including validated exact decimals with retained precision/scale.
Compiler helpers share `parameter::Parameter` instead of importing it from the
engine; DuckDB conversions live in a separate adapter. Parser declarations,
catalog reads and result type inference still need independent contracts before
the compiler can become its own crate. This extraction adds no SQL features.

Parameter declarations now use a shared core logical type model instead of parser
AST types. RPC decoding constructs it directly; parser conversion and backend
casts remain adapters. Omitted character declaration lengths now resolve to 1,
fixed CHAR/NCHAR families retain padding, and invalid scalar declarations are
rejected before earlier batch writes. DATALENGTH uses core scalar byte widths
and now handles known UNIQUEIDENTIFIER values/NULLs. Catalog snapshots, complete
result inference, alias/UDT identity, collation and canonical declaration error
numbers/states remain open; this is not a completed compiler extraction.

The workspace now includes `msduck-sql` for deterministic dialect/MERGE parsing,
logical parameter/type adapters, OPENJSON path tokens, AST builders and standalone
APPLY/TOP/window/grouping passes. Ten existing pure tests moved into its isolated
DuckDB-free loop; engine declaration and native integration tests remain root-side.
Root re-exports preserve existing call sites. Catalog binding, result inference,
batch orchestration and backend execution remain in `msduck`; a complete typed
plan/catalog snapshot boundary is still open. This extraction adds no SQL features.
Named-window definition validation now follows source order instead of hash-map
iteration, making multiple-error diagnostics deterministic while retaining
forward references. A repeated pure regression reproduced the prior unstable
selection and verifies the fix; live SQL Server multiple-error precedence remains
unverified.

Top-level FOR JSON PATH now connects the pure serializer to SQL execution, with
syntax policy in `msduck-sql`, Base64 in `msduck-core`, and catalog/Arrow/TDS work
in the root adapter. DESCRIBE binds aliases and expands stars without evaluating
source rows; source ordering and row limits remain in existing lowering. Typed
client coverage includes options, metadata, prepared queries, errors, temporal
values and 6000-row results. Nested/correlated queries, AUTO, expression-level
fragment/money provenance, set operations and SQL Server row chunking remain open.

Nested and correlated FOR JSON PATH now lower to typed native row serialization
and DuckDB aggregation, preserving source ordering, DISTINCT and paging. Scalar
JSON expressions work in prepared queries, variables, INSERT and views. The pure
SQL wrapper avoids alias capture; core aggregate framing preserves lexical values.
Nested WITHOUT_ARRAY_WRAPPER remains text unless JSON_QUERY promotes it, correcting
an over-broad promotion rule in the upstream reference approach. Source annotation
must precede generated wrapper lowering to preserve native variant payloads.
AUTO, set operations, complete inherited catalog and fragment/money provenance,
large-result row chunking and live SQL Server comparison remain open.

Nested FOR JSON now binds inherited nonrecursive CTE projections through explicit
catalog snapshots, preserving declaration order, renamed columns, MONEY identity
and TIME precision. Prepared reuse, CTE chains and base-table shadowing are covered.
Unknown/self-recursive declaration metadata shadows base tables; partially unknown
star projections are rejected instead of omitting unknown columns. Correlated
outer-row catalog inference, recursive types and full fragment provenance remain
open. Snapshot values still use root catalog Info; a backend-independent catalog
contract remains future architecture work.

Correlated FOR JSON named-column projections now retain outer MONEY identity and
TIME precision through explicit FROM-source snapshots. Local names, qualifiers,
ambiguity and unresolved-source barriers prevent metadata from leaking across
scopes; CTE definitions do not inherit outer rows. Tests cover qualified and
implicit references, multiple nesting levels, local DECIMAL shadowing, NULLs,
CTE sources and prepared reuse. Qualified outer stars now expand to explicit
references while preserving metadata and local shadowing. Recursive CTE types,
unknown-source inference and complete expression/fragment provenance remain open.

- FOR JSON fragment provenance now follows direct derived/CTE columns, stars and
  correlated references, with local shadowing and persisted-text barriers. General
  expressions, set operations, views and OPENJSON AS JSON provenance remain open.

- CTE output validation now checks explicit-list cardinality, duplicate names and
  unnamed expressions, including known star widths and left set-branch labels.
  Syntax-known failures are preflighted; catalog-dependent failures are checked
  during preparation/execution. Recursive CTE support, full name binding,
  collation-sensitive identifier equality and derived-table naming diagnostics
  remain unfinished.

- Duplicate names within one WITH list now report 239 before scope construction
  and batch writes. A runtime probe still found that `WITH earlier AS (SELECT n
  FROM later),later AS (SELECT 1 AS n) SELECT * FROM earlier` reads a preexisting
  base table `later`; an unqualified self reference can do the same. Pure metadata
  shadowing therefore does not yet enforce execution binding. A basic recursive
  anchor/UNION ALL CTE currently reaches DuckDB without WITH RECURSIVE and fails
  with a generic binder error. Correct recursive binding and execution, including
  types and MAXRECURSION semantics, remain required.

- Recursive CTE structural validation now rejects missing top-level UNION ALL,
  missing anchors, late anchors and multiple self references. This fixes silent
  same-named base-table fallback for malformed self references. Valid recursive
  execution, exact anchor/member types, forward-reference execution binding and
  MAXRECURSION remain unfinished. Manual depth-guard probes preserve exhaustion
  errors under projection, filtered COUNT(*) and INSERT, with INSERT rollback;
  see docs/architecture.md for the experimental lowering shape and limits.

- Common recursive CTE execution is now implemented with a private depth column
  and the default 100-step limit. Finite series, hierarchy joins, multiple
  recursive members, known stars, name hygiene, same-named base shadowing,
  prepared reuse, catchable 530 and failed-INSERT rollback are covered. Known
  anchor/member type mismatches report 240. Explicit MAXRECURSION hints, complete
  type/metadata inference, all recursive-member restrictions, forward binding and
  exact runtime diagnostic text remain unfinished. This supersedes the earlier
  blanket statement that valid recursive execution was unimplemented.

- MAXRECURSION hints now support literal limits 0–32767 on SELECT and CTE-prefixed
  INSERT/UPDATE/DELETE. Zero removes the limit; absent hints reset to 100 for each
  statement. Excessive values report 310 before batch writes. Runtime 530 reports
  the configured limit without DuckDB's prefix. Broader hint combinations,
  persisted-view hint propagation, complete recursive types/restrictions and
  forward-reference execution binding remain unfinished.

- Recursive-member preflight now reports 460 for DISTINCT, 461 for TOP/OFFSET,
  462 for outer joins, 465 for recursive references inside subqueries, and 467
  for GROUP BY/HAVING/known scalar aggregates. Anchor operations and recursive
  window aggregates remain valid. Full PIVOT/function/side-effect restrictions,
  recursive typing/metadata and multi-error precedence still need work.

- Recursive table-source checks now report 4150 for hints on the self reference
  and 4190 for PIVOT in the recursive member. These supersede the PIVOT gap above;
  function/side-effect restrictions, recursive type inference, mixed-error
  precedence and live comparison remain open. Base-table hint execution and
  anchor PIVOT execution are separate from these validator checks.

- Concrete follow-up from local recursive probes (2026-09-22):
  `WITH r(s,n) AS (SELECT CAST('a' AS VARCHAR(5)),1 UNION ALL SELECT
  CAST(s+'b' AS VARCHAR(5)),n+1 FROM r WHERE n<3) SELECT s,n FROM r`
  fails with a DuckDB `+(VARCHAR, STRING_LITERAL)` binder error. Recursive
  self-column character typing is lost before operator lowering. An explicit
  VARCHAR(6) member cast correctly raises 240 against the VARCHAR(5) anchor.
  SMALLINT and DECIMAL(5,2) anchors with recursive `n+1` currently execute,
  revealing incomplete arithmetic-result checks against the anchor type.
  Fix the shared recursive field/type propagation rather than special-casing
  these query strings. Probe log: `/tmp/msduck-recursive-next-metadata.log`.

- Recursive anchor field propagation now fixes the VARCHAR `+` failure noted
  above, including NVARCHAR, prepared invocations, empty result descriptors and
  later CTE/derived projections. Pure projection and backend expression binding
  share anchor extraction; bound arithmetic operands now reach scalar lowering.
  SMALLINT/DECIMAL recursive arithmetic-result mismatch detection and general
  common-type inference across multiple anchor members remain open.

  Follow-up probes after the binding fix confirm that two VARCHAR(5) anchors
  (`'a'` and `'x'`) followed by the same concatenating recursive member still
  fail: set-output merging discards character types even when both agree.
  Recursive TIME(2) with DATEADD(second,1,t) now executes with scale 2 preserved.
  SMALLINT and DECIMAL arithmetic mismatches still execute instead of rejecting.
  Evidence: `/tmp/msduck-recursive-types-next.log`.

- Multiple known character anchors now combine widths through a shared pure
  rule, fixing the two-VARCHAR-anchor failure above. Same-encoding fixed/varying
  families and MAX are covered, including empty metadata, prepared recursion and
  mismatched recursive widths. Mixed encodings, collation-label precedence,
  numeric anchor merging and full recursive arithmetic type validation remain
  unfinished.

- Known recursive numeric arithmetic now uses SQL Server precedence and decimal
  precision/scale formulas. SMALLINT + INT and widened decimal/numeric members
  reject with 240; matching casts and shape-preserving arithmetic remain valid.
  This supersedes the specific SMALLINT/DECIMAL binary-expression gap above.
  Complete parameter, unary, conditional/function and alias-type inference,
  numeric anchor merging and runtime decimal compatibility remain unfinished.

- Known numeric anchor sets now retain common types through both binding paths,
  with decimal precision/scale reduction, recursive mismatch checks, DATALENGTH,
  prepared execution and view metadata. Pure positional casts make differing
  numeric members use that type before comparison. MONEY/SMALLMONEY set conversion
  now checks their scaled integer bounds. General currency CAST/CONVERT,
  assignments and arithmetic still need that range coverage. Alias/unknown
  operands, broader expression inference and full collation semantics remain open.
- GENERATE_SERIES now exposes `value`, preserves known integer/decimal types,
  chooses descending defaults and supports correlated APPLY. Remaining work
  includes SQL Server comparison of mixed argument precision/coercion, NULL
  semantics and complete argument diagnostics, compatibility-level gating,
  metadata for unknown/correlated start expressions and decimal-series performance.

- Currency range policy now lives in the deterministic core and is reused by
  the native set-conversion callback. Separate probes on 2026-09-22 confirm the
  remaining integration gap: CAST and TRY_CAST of `'214748.3648'` to SMALLMONEY
  both return the out-of-range value; CAST of `'-922337203685477.5809'` to MONEY
  also succeeds; inserting `214749` into a SMALLMONEY column persists it.
  The next integration must reject ordinary conversion/storage overflow and
  return NULL for TRY conversion, including rounding across the endpoints.
  Probe output: `/tmp/msduck-money-core-next.log`. These are local observations;
  no live SQL Server comparison was performed.

- The currency conversion integration now rejects the ordinary CAST and storage
  overflow probes above, and TRY conversion returns NULL. INSERT/UPDATE, defaults,
  variable initialization and ALTER COLUMN/backfill have regression coverage.
  Numeric overflow reaching the core has error 8115. Full character-source
  diagnostics remain unfinished: SQL Server documents 235 for invalid money
  text and 236 for character-to-money overflow, while the current decimal-backed
  path can still report generic conversion or numeric-overflow errors. Currency
  symbols/lexical formats, very large inputs beyond the intermediate decimal,
  explicit styles, source-type restrictions and arithmetic remain open.

- Known currency result declarations now use nullable MONEYNTYPE with their
  four/eight-byte widths and exact scaled-integer bytes. Casts, TRY conversions,
  parameters, stored columns, views/CTEs and supported currency conditional/set
  projections have metadata coverage; decimal-precedence sets remain decimal.
  Complete expression/aggregate provenance and non-nullable fixed-type metadata
  still need work, alongside the conversion and arithmetic gaps above.

- Currency character conversion now parses directly to exact scale-four integers,
  accepts documented currency prefixes and ignored comma separators, rounds
  without a DECIMAL(38,4) intermediary, and distinguishes invalid money text (235)
  from money text overflow (236). TRY conversion returns typed NULLs. Native
  callbacks preserve numeric-source conversion and evaluate text producers once;
  casts, RPC/prepared inputs, columns, defaults, assignments and ALTER conversion
  have coverage. This supersedes the specific character diagnostic and prefix
  gaps above. Complete lexical corner cases/error-state precedence, SMALLMONEY
  narrowing diagnostics, explicit styles, numeric-source restrictions and full
  currency arithmetic still require SQL Server comparison and further work.

- Known MONEY/SMALLMONEY-to-character casts now use default two-place formatting;
  CONVERT styles 0/1/2 and the style 126 alias have dedicated exact lowering.
  Dynamic styles, grouping, rounding carry, fixed padding, typed NULL/empty results,
  RPC parameters and catalog-backed source columns are covered. This supersedes
  the blanket output-style gap for these known sources. Conditional/arithmetic/
  aggregate source provenance, signed-zero display, exact short-target diagnostics
  and broader style interactions still need work and live reference validation.

- Conditional currency identity now survives known numeric CASE/COALESCE/IIF/
  CHOOSE branches, and ISNULL/NULLIF retain their first-argument currency type.
  Shared pure inference supplies formatting, logical result descriptors and
  derived-source typing, with unknown and higher-precedence noncurrency barriers.
  Formatting enforces the inferred MONEY/SMALLMONEY range before display.
  Full conditional value coercion (especially mixed character branches), general
  arithmetic/aggregate identity and live reference validation remain unfinished.

- Known currency conditional results now convert character/numeric branches and
  ISNULL replacements before backend unification, using exact currency parsing,
  rounding and bounds. Errors occur inside predicates and assignments as well
  as result encoding. Existing TRY branches remain intact; selectors and
  conditions are not duplicated. Character concatenation classification and
  VALUES character declaration merging fix the observed source-type gaps.
  This supersedes the specific known-character conditional conversion gap above.
  General mixed/unknown source inference, NULLIF comparison coercion, full
  COALESCE evaluation semantics and live SQL Server comparison remain open.


- NULLIF now applies known currency comparison precedence and exact text/range
  conversion independently of its first-argument result type. Character widths,
  empty metadata, prepared reuse and bound-column paths are covered. General
  binary currency comparisons, full mixed-type coercion and live SQL Server
  comparison remain unfinished.


- Currency binary equality/order and NULL-safe comparisons now apply explicit
  precedence, exact text conversion and range checking in scalar, prepared,
  joined, CTE and correlated paths. DML predicate failures retain atomicity.
  BETWEEN/IN/subquery and simple CASE currency comparisons, general currency
  arithmetic typing and live SQL Server comparison remain unfinished.


- Currency BETWEEN/NOT BETWEEN, IN/NOT IN lists and simple CASE input/WHEN
  comparisons now use known common precedence and exact conversion. NULL/UNKNOWN,
  empty metadata, prepared reuse and atomic DML are covered. IN subquery and
  quantified-comparison projection coercion, broader currency expression typing
  and live SQL Server comparison remain unfinished.


- Known currency IN/NOT IN subqueries and ANY/SOME/ALL comparisons now convert
  one-column results through a complete-query wrapper. Correlation, original text
  ordering/limits, NULL/empty sets, prepared reuse and atomic DML are covered.
  Scalar-subquery expression typing, broader noncurrency quantified coercion and
  live SQL Server comparison remain unfinished.


- Scalar currency queries now retain logical declarations in result metadata,
  comparisons, conditional results and formatting. Projection inference carries
  CTE and outer-row scopes; SELECT INTO, prepared reuse, scalar cardinality and
  quantified multi-row behavior are covered. Full expression provenance,
  nullability/collation inference and live SQL Server comparison remain open.

- Known MONEY/SMALLMONEY SUM and AVG now return MONEY with exact scale-four
  accumulation, bounded sums, typed empty/NULL groups, DISTINCT and window
  support. Currency aggregate declarations propagate through formatting,
  derived queries and SELECT INTO; MIN/MAX retain their input family. Shared
  bounded state now lives in the deterministic core. General currency arithmetic
  provenance and live comparison of rounding/overflow ordering remain open.

- Recognized numeric runtime errors now expose canonical SQL text without
  DuckDB's prefix on the wire, in ERROR_MESSAGE and through bare rethrow.
  Explicit THROW number/state/text are preserved. Complete backend diagnostic
  mapping, SQL Server state selection and line attribution remain open.

- Currency-valued arithmetic now preserves MONEY/SMALLMONEY declarations for
  +, -, *, /, %, and unary negation, applies exact scaled arithmetic and bounds,
  and converts known lower-precedence operands before evaluation. Results flow
  through derived queries, aggregates, formatting and SELECT INTO. Native
  adapters preserve NULLs and single operand evaluation across chunks. The first
  live SQL Server comparison corrected division to truncate scale-four results
  toward zero; multiplication rounds. Broader evaluation-order compatibility
  remains open.

## Live reference findings (2026-09-22)

The first full SQL Server 2025 RTM-CU7 comparison completed all 273 paired
captures, with zero exact matches and 273 differences. Every reuse probe has
metadata differences, although reuse rows agree. Execution captures additionally
expose string-padding comparison, numeric CHAR alignment, FOR JSON currency
serialization, result-set boundaries around errors, and diagnostic differences.
These are verified gaps, not expected exceptions to exclude from comparison.
See [reference comparison](docs/reference-comparison.md#first-complete-live-comparison--2026-09-22)
for counts, raw artifacts and priorities. `npm run audit:docker` now provides a
repeatable owned-container reference workflow adapted from mssqlite.

The corrected local implementation passed 323 Rust tests and 337 client/harness
tests; all 273 local audit cases completed. Local passing tests do not establish
SQL Server compatibility. Next work should carry precise result declarations
through deterministic planning into effectful wire adapters, then resolve the
remaining value, event-boundary and diagnostic differences against live captures.


- Live FOR JSON currency validation corrected MONEY/SMALLMONEY output from
  quoted text to exact scale-four JSON numbers, including signed endpoints,
  nested/correlated queries and prepared values. Explicit text conversions remain
  quoted. Four existing audit captures improved; the new endpoint probe matches
  live SQL Server rows exactly. Empty-result behavior, metadata and other FOR
  JSON gaps remain open; see [FOR JSON evidence](docs/for-json.md#live-currency-serialization-correction-2026-09-22).


- Live currency character probes corrected MONEY/SMALLMONEY alignment in CHAR
  and NCHAR to leading-space padding; ordinary numeric/text conversions retain
  trailing-space padding. Style 126 now retains four places for Unicode targets
  too. All 275 local audit cases completed, with only the expected existing
  currency alignment value changed; the new combined probe matches reference
  rows. Currency short-target error numbers/states and broader character
  collation/comparison behavior remain unfinished.


- Result-column properties now cross the core/SQL/TDS boundaries explicitly.
  Catalog snapshots provide base-column nullability and identity; pure projection
  inference propagates aliases, CTEs, derived sources, outer joins and grouping
  extension. The TDS codec encodes properties rather than constant nullable flags.
  Live probes and raw audit comparison exposed and corrected overbroad computed
  flags. Fixed non-null wire encodings, complete expression and view inference,
  precise set/grouping distinctions and collation flags remain unfinished.
  See [result metadata](docs/result-metadata.md).


Result-property verification: 328 Rust tests and 340 client/harness tests passed;
formatting and strict Clippy passed. All 276 local audit cases completed. The
saved-reference comparison found 164 improved execution-column flags and no
regressions among previously matching flags at corresponding positions/names.
The declaration/join probe's flags match its live reference. These checks do not
establish complete metadata or overall SQL Server compatibility.

Transaction recovery has a live-confirmed backend gap: a caught runtime error
under XACT_ABORT OFF invalidates DuckDB's explicit transaction. See
[transaction recovery evidence](docs/transaction-recovery.md) for the required
statement rollback and doomed-state behavior. Metadata/signature fixes alone
do not resolve it.


TDS 7.x full-session TLS 1.2 now has a root transport adapter, deterministic
PRELOGIN policy, PEM CLI configuration and focused client tests. The full
verification is in progress; see docs/tls.md. SQL authentication and the remaining
TLS modes are still open requirements.


Bootstrap administrator password verification now runs over required TLS using
hash-only configuration, uniform LOGIN7 failures and file rotation. Full SQL
login DDL, principal catalogs and authorization remain open; see
docs/authentication.md for the exact boundary and verification status.
