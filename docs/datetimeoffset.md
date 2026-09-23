# DATETIMEOFFSET transport foundation

`DateTimeOffset` retains a validated UTC instant at 100ns precision and a signed
minute offset. Local civil time is derived without using the host timezone.
Both UTC and local values must remain within years 0001–9999, and offsets are
bounded to -840 through +840 minutes. Scale conversion rounds exact ticks and
validates both ranges again after a possible day carry.

The bare TDS value contains UTC time/date followed by a signed little-endian
offset. Scales 0–2 use 8 bytes, 3–4 use 9, and 5–7 use 10. RPC type 0x2B now
decodes into offset-preserving text with a DATETIMEOFFSET(scale) declaration.
Result metadata and value encoding also support the corresponding wire type.
NULL, invalid scale, malformed width, truncation, out-of-range time/date and
offset payloads are covered by tests.

This uses the copied TDS skill and reuses the exact vectors and boundary cases
from mirek/mssqlite `packages/tds/src/codecs.test.ts`, including a positive offset
whose UTC date is the previous day. Tests round-trip all 1,681 permitted offsets
at all eight scales across early, leap-day and late calendar values. Integration
tests cover RPC parameter descriptors and result metadata/length prefixes.

References: [TDS dates and times](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-tds/786f5b8a-f87d-4980-9070-b9b7274c681d)
and [DATETIMEOFFSET ranges](https://learn.microsoft.com/en-us/sql/t-sql/data-types/datetimeoffset-transact-sql?view=azuresqldb-current).

SQL casts and TRY conversions now produce a two-field tagged STRUCT containing
UTC ticks and signed offset minutes, with the declared scale in its type tag.
DATE/DATETIME2/TIME inputs acquire offset zero; DTO-to-DTO conversions preserve
the offset and round to the target scale. RPC parameters, local variables,
columns, defaults, INSERT/UPDATE and ALTER COLUMN share the conversion path.
Arrow result extraction retains both fields before TDS encoding. Restart tests
verify the stored offset, retained defaults and scale-change rounding.

Comparisons extract UTC keys after scale conversion, including joins, BETWEEN,
IN lists/subqueries, NOT IN and simple CASE. Native tests cover all scale pairs,
NULL truth tables and single evaluation. Client tests cover 100ns results,
metadata, prepared rebinding, views and equivalent instants at different offsets.
The audit's previously failing DATETIMEOFFSET SQL conversion case is now expected
to return a typed value; the audit remains local evidence rather than a live
SQL Server comparison.

The client transport test separately covers malformed metadata, widths, offsets
and dates, checking session recovery after each rejection.

Remaining work includes complete mixed temporal type precedence/scale
verification, conversions out to other temporal/text
types, DATEADD/DATEDIFF/DATEPART integration, full SQL Server date formats and
live reference comparison. The public value object's equality preserves the
offset; SQL predicate equality explicitly compares UTC instants.

SQL integration verification: 165 Rust tests and 245 client tests pass, with
clean formatting and Clippy. All 141 local audit captures are complete. The
previously failing cast now returns DATETIMEOFFSET(7) at UTC
2026-06-30 21:00:00.1234567; the UTC-equality case returns 1 and retains typed
NULLs and scale-3 rounding. Offset preservation is separately verified in native
storage tests because tedious exposes a UTC Date rather than the original offset.

GROUP BY now uses UTC equality, retaining a source DATETIMEOFFSET payload as the
representative value. Different offsets for one instant merge into one group;
no particular representative offset is promised. GROUPING/GROUPING_ID refer to
the UTC keys, while subtotal results are typed NULLs distinct from real NULL
input groups. The rewrite covers projection, HAVING and ORDER BY and skips
aggregate arguments and nested query scopes. Mixed variant/offset keys use the
same existing grouping rewrite.

Repeated same-scale casts introduced by annotation are collapsed so GROUPING
matches its group key. Conversions through another scale remain intact, including
their rounding. Tests cover ordinary groups, rollups, repeated grouping sets,
NULLs, qualified names, aliases, wildcards, prepared rebinding and derived output,
plus 6,000 rows across vector boundaries.

Inline VALUES sources infer DATETIMEOFFSET columns and normalize their values to
the highest declared scale before grouping. A mixed-scale inline regression
checks UTC equality, typed NULL input, subtotal NULLs and output metadata.

Verification after the grouping changes: 166 Rust tests and 246 client tests
pass; formatting and Clippy are clean. All 142 local audit captures are complete.
The inline UTC grouping capture returns counts 1 for the NULL input group, 2 for
the shared instant and 3 for the subtotal, retaining DATETIMEOFFSET(3) metadata.
These captures have not been compared against a live SQL Server reference.

DISTINCT and UNION/INTERSECT/EXCEPT now compare UTC keys while returning a retained
source payload. UNION ALL preserves duplicates, and DATETIMEOFFSET branches use
the highest declared DATETIMEOFFSET scale. Tuple comparison handles other columns
and integer variants alongside offset values. NULLs compare equal for set
membership. INTERSECT and EXCEPT retain payloads from the left branch; no specific
representative offset is promised when equivalent rows are deduplicated.

Native tests check single evaluation over 6,000 rows for DISTINCT and both set
branches, exact 100ns UTC payloads and retained offsets. Client tests cover typed
empty results, prepared rebinding, aliases, paging, CTEs, mixed scales and tuples.
The new local audit case captures DISTINCT, INTERSECT and EXCEPT results.

Set-operation verification: 168 Rust tests and 247 client tests pass; formatting
and Clippy are clean. All 143 local audit captures completed. The new capture
has one NULL and one UTC instant after DISTINCT, one matching INTERSECT row,
and zero EXCEPT rows, with scale-7 metadata for all three result sets.
The upstream mssqlite `packages/engine/src/datetimeoffset.ts` UTC key helper was
reviewed again: it retains 100ns precision in its normalized lexical key. Rust
uses exact integer UTC ticks instead. Live SQL Server comparison remains open.

Conditional results now normalize DATETIMEOFFSET branches before DuckDB combines
them. CASE, COALESCE, IIF and CHOOSE use the highest DATETIMEOFFSET branch scale;
ISNULL keeps the first argument's scale unless that argument is literal NULL.
NULLIF retains the first result type and compares UTC instants. Conversions stay
inside the selected branches, preserving lazy evaluation of scalar alternatives.
Native tests verify exact UTC ticks, source offsets, NULL structure validity and
6,000-row vector processing; ISNULL evaluates each needed operand once. Client
coverage includes metadata for empty/NULL results, views, prepared rebinding,
comparisons and assignments. Complete mixed temporal type coercion still needs
reference verification, and conversion from offset values to other temporal
types remains unfinished.

Conditional verification: 169 Rust tests and 248 client tests pass; formatting
and Clippy are clean. All 144 local audit captures completed. The new conditional
capture verifies scales 7/3/7, exact COALESCE fractional ticks, rounded ISNULL and
a typed NULL CASE result. This remains local evidence, not a live SQL Server
comparison. Return-type rules were checked against Microsoft's
[ISNULL reference](https://learn.microsoft.com/en-us/sql/t-sql/functions/isnull-transact-sql?view=sql-server-ver17)
and [CASE reference](https://learn.microsoft.com/en-us/sql/t-sql/language-elements/case-transact-sql?view=sql-server-ver17).

DATE and TIME extraction now reads local components from DATETIMEOFFSET, including
values whose UTC date differs. The native extractor validates both structure
fields, adds the retained offset, and preserves exact clock ticks before TIME
scale rounding. Shared calendar extraction also gives YEAR/MONTH/DAY the local
date. DATE and TIME_NS assignments use the same conversion macros.
DATETIME2 conversion is still pending: its converter is also used internally by
date functions, so accepting offset payloads there requires auditing each caller's
offset semantics. Legacy temporal and textual conversions also remain open.

Extraction verification: 170 Rust tests and 249 client tests pass; formatting and
Clippy are clean. All 145 local audit captures completed. The new capture verifies
the local date, exact TIME(7) fraction, rounded TIME(3) value and local YEAR.
It also exposes a remaining mismatch: TIME(3) is advertised as scale 7. The current
TDS Type::Time descriptor is fixed at 7, so correct value rounding does not imply
correct declared-scale metadata. This affects TIME generally and remains open.

The TIME descriptor gap above is now addressed for the audit's explicit casts.
TDS TIME carries its declared scale and a 3/4/5-byte payload as specified by the
copied TDS skill. Result inference preserves explicit cast/convert scales,
known conditional and set result scales, bound TIME parameter declarations and
direct catalog column provenance. Tests include NULL/empty results and midnight
rounding. Broader expression provenance and complete assignment rounding remain
open rather than being inferred from this wire metadata fix.

TIME scale verification: 171 Rust tests and 250 client tests pass, with formatting
and Clippy clean. All 146 audit captures completed. The original offset-to-TIME(3)
case now advertises scale 3; the new TIME descriptor case retains scales 0/2/4/6
and scale 3 for an empty result, with correctly rounded decoded values.

TIME target assignments now apply the catalog's declared scale before storage,
including DATE/TIME extraction from offset values. INSERT, INSERT SELECT,
omitted/explicit defaults and UPDATE share the target conversion. ALTER COLUMN
retains its original declaration for conversion of existing rows. ADD population
rounds to the declared TIME scale while preserving the original default expression
for later assignments, avoiding double rounding after a scale change.

Storage-scale verification: 172 Rust tests and 251 client tests pass; formatting
and Clippy are clean. The native assignment test inspects backing TIME_NS ticks
at all eight scales over 6,000 rows. All 147 local audit captures completed; the
new case matches all three stored values in equality predicates before and after
ALTER COLUMN changes the scale. Live SQL Server comparison remains open.

Persistence and transaction verification: a native test closes and reopens a
file-backed database twice, checking TIME scale metadata, default expressions
and raw stored nanoseconds after rollback and a committed scale change. Existing
rows round from their stored value, while future default assignments use the
original default expression. A client regression checks rollback of values and
descriptors, failed multi-row UPDATE atomicity, and rollback of ADD WITH VALUES.
All 173 Rust tests and 252 client tests pass; formatting and Clippy are clean.
This increment changes tests only; the prior 147-case semantic audit is unchanged.

TIME ISNULL replacement conversion now occurs inside the expression, before
comparison or assignment to another target type. Previously result encoding could
make a value look rounded while predicates still saw the finer-scale replacement.
The first argument's scale is recovered from explicit conversions or catalog
annotation; the replacement is converted lazily through the TIME assignment path.
Tests verify internal equality, assignment into TIME(7), midnight carry and one
evaluation per needed operand over 6,000 rows. Mixed temporal conditional
precedence and broader TIME result provenance remain unfinished.

ISNULL verification: 174 Rust tests and 253 client tests pass, with formatting
and Clippy clean. All 148 audit captures completed. The new case returns scale 2,
12:00:00.12 and a true equality result against the rounded value. This confirms
conversion inside the expression rather than rounding solely at the wire boundary.

TIME conditional conversion now shares the exact temporal result rewrite.
CASE/COALESCE and lowered IIF/CHOOSE normalize their result branches to the common
TIME scale, including text branches; ISNULL keeps first-argument scale semantics.
Conversions remain inside branches to preserve lazy scalar evaluation. Supported
TIME/text column combinations retain result scale metadata through query catalog
inference. Internal predicates and SELECT INTO use the converted values, rather
than relying on output encoding to round them. Complete mixed temporal precedence
and arbitrary expression provenance remain open.

Conditional TIME/text verification: 175 Rust tests and 254 client tests pass;
formatting and Clippy are clean. All 149 audit captures completed. The new case
verifies scales 2 and 3, rounded text conversion, midnight carry and equality
against the converted value. Live SQL Server comparison remains open.

COUNT(DISTINCT DATETIMEOFFSET) and COUNT_BIG(DISTINCT DATETIMEOFFSET) now count
UTC keys, ignoring NULLs and merging equal instants across offsets while keeping
100ns differences distinct. The prior implementation counted the backing structs
and returned 4 rather than 2 in the new regression. Tests cover grouped counts,
empty and NULL-only inputs, mixed-scale inline sources, prepared reuse and native
single evaluation over 6,000 rows. Approximate-count semantics remain separate.

Distinct-count verification: 176 Rust tests and 255 client tests pass; formatting
and Clippy are clean. All 150 audit captures completed. The new capture returns
COUNT DISTINCT 2, COUNT_BIG DISTINCT 2 and ordinary COUNT 3, with 4/8/4-byte integer
metadata. The two unique instants differ by 100ns; NULL is excluded.

Window PARTITION BY and ORDER BY keys now use UTC equality for DATETIMEOFFSET.
The regression previously split three offset representations of one instant into
three single-row partitions. Tests now verify one shared partition, RANK and
DENSE_RANK peers, RANGE CURRENT ROW frames, NULL partitions, tuple partitions and
prepared rebinding. Adjacent 100ns instants remain separate. Native tests inspect
6,000-row partition/frame results. A standalone DuckDB 1.5.5 numeric-key probe
shows partition keys evaluated twice per row, while ORDER BY keys evaluate once.
The native regression compares rewrite evaluation counts with this backend
baseline. Single evaluation of volatile partition keys is not established.

Window verification: 177 Rust tests and 256 client tests pass; formatting and
Clippy are clean. All 151 audit captures completed. The new case returns shared
partition counts and ranks for equal UTC instants and the expected RANGE peer
sums. Volatile partition-key evaluation remains a documented backend concern;
these results do not establish single evaluation or full SQL Server equivalence.

Query ORDER BY output aliases are now protected from UTC key annotation against
same-named source columns. A regression previously returned numeric aliases in
source-offset order. Tests verify alias precedence, paging and DISTINCT, while
window ORDER BY still binds the source DATETIMEOFFSET and retains UTC peers.
The shared fix also covers SQL_VARIANT source columns shadowed by text aliases.

Alias-binding verification: 177 Rust tests and 257 client tests pass; formatting
and Clippy are clean. All 152 audit captures completed. The new capture orders
the projected numeric alias as -2, -1 while both source instants retain window
rank 1, verifying the two binding scopes independently.

DATEDIFF and DATEDIFF_BIG now normalize typed DATETIMEOFFSET inputs to exact UTC
DATETIME2 ticks through a dedicated native adapter. This preserves NULL structure
validity and avoids changing local-clock cast semantics. Existing boundary counting
and checked INT/BIGINT overflow then apply. Tests cover equivalent offset instants,
100ns differences, scale combinations, NULLs, mixed DATETIME2 inputs, column and
prepared execution, overflow and single evaluation over 6,000 rows at every scale.
Microsoft's DATEDIFF documentation specifies use of the offset component; this
UTC-boundary implementation still awaits live SQL Server reference comparison,
including mixed temporal types and calendar boundary edge cases.

Offset DATEDIFF verification: 178 Rust tests and 258 client tests pass; formatting
and Clippy are clean. All 153 audit captures completed. The new case returns
0, 100 and NULL, with INT/BIGINT/INT widths. Offset handling follows the
[documented DATEDIFF offset rule](https://learn.microsoft.com/en-us/sql/t-sql/functions/datediff-transact-sql?view=sql-server-ver17);
live SQL Server comparison remains pending.
