# DATETIME2 casts, storage and RPC

An exact Rust value now stores 100ns ticks since 0001-01-01. The entire SQL
Server year 1–9999 range fits in a signed 64-bit integer in these units.
Construction validates dates and times. Calendar conversion, strict ISO
parsing/formatting, scale rounding with day/year carry, and bare TDS encoding
and decoding support scales zero through seven without microsecond truncation.

The existing timestamp result encoder uses this codec and rounds backend
nanoseconds to the nearest 100ns, including negative Unix timestamps. It still
emits scale-seven DATETIME2 metadata. Rust tests exhaustively round-trip every
supported calendar date and test independent wire bytes, all scales, invalid
payloads, range endpoints and rounding carry. A tedious test checks the actual
wire result for backend nanosecond values around the Unix epoch.

SQL CAST/TRY_CAST and style-free CONVERT/TRY_CONVERT now return a tagged DuckDB
STRUCT containing exact BIGINT ticks. The field tag carries the declared scale,
which survives projections, NULL/empty results and stored views into TDS
metadata. Native vector conversion accepts strict ISO text, date/time/timestamp
inputs and existing tagged values, preserving precision through nested casts.
Invalid calendar text and range overflow report 241; TRY conversion returns
NULL for these conversion failures. Parent and child validity are both set for
NULL structs so DuckDB field extraction cannot expose an uninitialized value.

DATETIME2 RPC input validates scale and payload bounds with the same codec and
binds an exact ISO string through the tagged conversion. Tedious tests cover
full-range/seven-digit RPC and prepared execution, all NULL metadata scales,
casts, local variable declaration/assignment, rounding, invalid dates and conversion recovery. Native tests cover
nullable and malformed text over several chunks and every target scale.
The audit's former DATETIME2 gap probe now exercises these working casts.
Audit serialization retains the client's sub-millisecond date component.

CREATE TABLE and ALTER TABLE ADD use the same tagged storage type. INSERT
(VALUES, SELECT, prepared execution and DEFAULT VALUES) and UPDATE convert
values to the target scale. Declared defaults are converted when stored in the
schema. ALTER COLUMN converts existing values with an explicit conversion
expression; a failed conversion rolls back the column change. Client tests
cover every scale, nullable rows, exact range endpoints, defaults, multirow
write atomicity and ALTER failure recovery. The local audit includes a storage
and assignment probe.

DATE casts (including TRY variants and style-free CONVERT), YEAR/MONTH/DAY and
EOMONTH now accept exact tagged values. A native helper extracts the civil date
without rounding through a timestamp. NULL and empty results keep DATE or INT
metadata. Tests cover both range endpoints just before midnight, leap days,
stored columns, CTEs, local variables, RPC, prepared execution and EOMONTH
overflow recovery. Vector tests check NULLs at every supported scale; sequence
tests check single evaluation across chunks.

TIME casts and style-free CONVERT (including TRY variants) extract integer
time ticks before target-scale rounding. This retains all seven digits and
permits midnight carry without overflowing the discarded date. Stored columns,
RPC and prepared queries are covered. TIME results still advertise scale 7;
see [TIME limitations](time-rpc.md) for storage and metadata gaps.

Binary comparisons (=, <>, <, <=, >, >=), BETWEEN, IN lists and simple CASE
comparisons normalize known DATETIME2 operands to exact scale-seven tick keys,
so scale tags cannot change equality or ordering. Extracting the integer key
also avoids DuckDB's unsupported STRUCT BETWEEN filter path.
The source resolver recognizes table/view columns, CTE projections and uniformly
typed VALUES sources, including UPDATE/DELETE predicates. Text and DATE operands
convert through the exact cast when compared with a known DATETIME2 value.
Tests cover all 64 scale pairs, one-tick differences, NULL predicates, joins,
prepared input errors/recovery, writes and single evaluation of binary
comparison operands across chunks.
Native truth-table tests cover 6,000 rows of NULL/value combinations in
projections and filters, including NOT BETWEEN, NOT IN and simple CASE.
Client tests cover inclusive one-tick bounds, typed NULLs, stored columns,
prepared text bounds, CTEs, write predicates and unchanged CASE result types.

CASE, COALESCE, IIF and CHOOSE now convert known DATETIME2 result branches to
the highest declared scale among those branches. NULL and empty results retain
that scale in TDS metadata. Scale inference follows these expressions through
CTEs, scalar subqueries with known outputs, and type-preserving functions such as MIN/MAX and value windows. Tests
cover all 64 scale pairs with typed NULLs, precise stored values, prepared
conditional conversion/recovery, views, aggregate inputs and 6,000 rows of
conditional STRUCT validity. These result-scale rules still need live reference
comparison alongside the rest of the implementation.

ISNULL preserves the first known DATETIME2 argument's scale and converts its
replacement to that scale. A literal NULL first argument still inherits the
replacement type. Tests cover all 64 scale pairs, rounding, nested calls,
NULL/empty metadata, source columns, CTEs, views, prepared conversion errors
and range overflow recovery. A native sequence test verifies each needed
operand is evaluated once over 6,000 rows.

NULLIF compares known DATETIME2 operands using exact ticks and retains the
first argument's result scale. Equal instants at different scales return NULL;
a one-tick difference remains distinct. Column, CTE, nested conditional,
empty-result and prepared-execution coverage is described in [NULLIF](nullif.md).

IN and NOT IN subqueries with a resolved single-column output compare exact
ticks when either side has a known DATETIME2 type. Wrapping the complete query
preserves DISTINCT, ordering and row limits. Tests cover all scale pairs,
correlation, CTEs, NULL/empty subqueries, reversed operand scales, DML filters,
invalid multi-column inputs and prepared conversion recovery. This follows
Microsoft's [IN semantics](https://learn.microsoft.com/en-us/sql/t-sql/language-elements/in-transact-sql).
The inspected upstream expression translator also infers the first subquery
projection type when choosing comparison conversion.

UNION, UNION ALL, INTERSECT and EXCEPT normalize known DATETIME2 result
columns to a common maximum scale before duplicate comparison. Mixed-scale
VALUES columns use the same maximum scale. Tests cover all 64 scale pairs,
NULL equality in sets, one-tick differences, wildcard projections, output-name
casing, CTEs, empty results, range endpoints and prepared error recovery.
The upstream `coercedSet` implementation in `packages/transpile/src/statement.ts`
also infers positional common types; this implementation uses tagged DuckDB
values and AST wrappers to retain the original branch queries.

DATEPART extracts exact calendar, clock and fractional fields; see [coverage](datepart.md).

Remaining integration includes
broader expression type inference, other
conversions out of the tagged type, locale/style-dependent parsing, temporal functions, fuller
error fidelity and live reference comparison. Indexes and key constraints on
the tagged column also require integration beyond DuckDB's STRUCT support.
Microsecond TIMESTAMP cannot retain the seventh digit; TIMESTAMP_NS cannot
cover the full date range. Merely aliasing DATETIME2 to either type is insufficient.

The inspected mssqlite date-time codec also rounds before encoding time plus
date and checks rollover beyond year 9999. Its TypeScript implementation uses
separate civil parts; this Rust implementation uses exact integer ticks.
Upstream `packages/transpile/src/type.ts` maps DATETIME2 storage to datetime,
while `packages/engine/src/metadata.ts` retains the RPC scale separately.
The DuckDB implementation instead retains both ticks and scale in its tagged
column type, so native timestamp precision does not limit stored values.
The exact parser also accepts 24-hour time-only `HH:MM` and
`HH:MM:SS[.fffffff]` text, supplying 1900-01-01. It retains all fractional
digits before target-scale rounding, including carry into 1900-01-02.
Unit tests exercise every whole-second clock position; client tests cover
casts, defaults, INSERT/UPDATE, comparison, DATEPART and prepared recovery.
Invalid clock fields fail with 241, while TRY conversion returns NULL.
The inspected upstream TDS `date-time.ts` likewise supplies the 1900 base date
for time-only values. Other string formats and locale-dependent
input rules remain unfinished.
The inspected upstream `packages/transpile/src/implicit.ts` widens a common
type by taking the maximum declared type arguments, including DATETIME2 scale.
Its inference also tracks whether every input type is known; broader expression
inference remains an explicit integration task here.

References: Microsoft [DATETIME2](https://learn.microsoft.com/en-us/sql/t-sql/data-types/datetime2-transact-sql)
and the copied [TDS date/time specification](../.agents/skills/tds-protocol/data-types.md).

IS DISTINCT FROM and IS NOT DISTINCT FROM normalize known DATETIME2 operands to
exact ticks, ignoring declared scale when values are equal. NULL/NULL is not
distinct; one NULL is distinct. Native 6,000-row truth tables and client tests
cover cross-scale equality, NULL combinations and a one-tick difference.

Typed DATETIMEOFFSET converts to DATETIME2 by validating UTC/offset payloads,
copying the local date/time ticks, dropping the offset, and applying the target
DATETIME2 scale. This works through explicit CAST/CONVERT and TRY variants,
assignments and ALTER COLUMN. Tests cover all 64 source/target scale pairs over
6,000 rows, NULLs, single evaluation, local/UTC date differences, midnight carry,
prepared calls and range overflow. DATEDIFF retains its separate UTC adapter.
The DATETIME2 reference documents copying local components; the DATETIMEOFFSET
reference's reduced-scale wording/example is inconsistent. The implementation
uses existing half-up DATETIME2 rounding; live reduced-scale comparison remains
required. Offset-bearing ISO text parsing is covered below.

DATETIME2 SQL text conversion now accepts ISO date/time or time-only text with
Z or signed HH:MM offsets, validates the suffix, and keeps local clock fields.
Time-only values retain the 1900-01-01 base date. Local range checks apply without
requiring the discarded UTC instant to fit; UTC range checks still apply when
constructing DATETIMEOFFSET. Date-only text with a zone is rejected. Native tests
cover all scales, malformed offsets, NULLs and single evaluation across chunks;
client checks cover prepared calls, assignment, rollover and local range limits.
Broader formats, timezone-only literals and live boundary comparison remain open.
