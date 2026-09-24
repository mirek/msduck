# Result declaration properties

`msduck-core::result::Properties` carries optional declared nullability and an
origin category independently of backend and wire types. Unknown nullability
is distinct from proof of NOT NULL. SQL planning attaches these properties to
ordered `binding_scope::Field` values beside type information and JSON provenance.

The root acquires table nullability and identity from the catalog snapshot.
`msduck-sql` propagates them through aliases, stars, CTEs and derived sources,
adds outer-join NULL extension, removes identity for set-operation outputs,
and marks recursive CTE fields nullable. ROLLUP/CUBE/grouping sets extend
grouping-key nullability, including computed keys. Literal, cast, scalar-subquery, conditional and aggregate
rules inspect syntax and explicit declarations, never actual row or parameter
values. Expression fields exposed through row sources become derived columns;
stored/identity provenance survives that boundary. Temporal and SQL_VARIANT
outputs follow their reference metadata conventions rather than acquiring a
computed flag solely because their syntax is an expression. Unresolved names retain unknown properties rather than inheriting an
unrelated outer field.

Before backend lowering, execution obtains the ordered result properties. The
root attaches them to the corresponding wire columns after binding the backend
schema. `msduck-tds` owns their flag encoding:

| Origin | NOT NULL | Nullable |
| --- | ---: | ---: |
| Computed scalar expression | 32 | 33 |
| Ordinary stored column | 8 | 9 |
| Identity column | 16 | 17 |
| Derived aggregate/set result | 0 | 1 |

Unknown properties retain the prior conservative nullable flag. Unclassified
functions and persisted view origins remain unknown. Flags are never
inferred from whether a particular execution happened to return NULL or no rows.
The wire representation remains independent. Proven non-null integers, BIT,
REAL/FLOAT and MONEY/SMALLMONEY now use fixed scalar descriptors and payloads;
unknown or nullable outputs retain the nullable families. Complete result widths,
legacy datetime fidelity and collation flags still need implementation.

The live SQL Server 2025 RTM-CU7 matrix in
`artifacts/compatibility/sql-server-result-properties.json` covers literals,
casts, stored fields, empty results, aliases, CTEs, joins, grouping, ranking,
conditional expressions, scalar subqueries and set outputs. A combined audit
probe is preserved in `sql-server-result-properties-probe.json` in that directory.
Standalone SQL tests verify immutable snapshots and NULL extension; protocol
vectors verify the flag bytes, and tedious tests exercise the complete server.

This is an incremental result contract, not a complete compiler plan. Remaining
work includes full nullability inference for every expression/table function,
view-origin fidelity, precise grouping-set and set-operator distinctions,
collation case-sensitivity and updatability modes, and remaining wire types. Type and property inference must eventually produce one complete
result declaration rather than parallel adapter outputs.


## Verification against preserved live captures

The final local audit completed all 276 cases. Of 275 preceding executions,
198 were unchanged and 77 changed. Changes were confined to column flags,
except the unordered `derived table apply` query returning its existing rows in
reverse order. Types, lengths, errors and completion events were unchanged.
The new declaration/join probe's entire flag matrix agrees with its live SQL
Server capture. Raw differences are retained in
`artifacts/compatibility/result-properties-local-diff.json`.

For corresponding result-set/column positions with matching names in the
preserved full SQL Server capture, 164 flag values changed from mismatching to
matching; none changed from matching to mismatching. This comparison concerns
flags only and does not establish whole-case equivalence. Reuse probes still
have fixed-versus-nullable integer type and XACT_STATE width differences.

The initial audit exposed overbroad computed flags on derived and temporal
results. Further checks caught missing type information and the batch parser's
internal integer-conversion marker. Inference now borrows the original source
through that marker, and pure tests use the production batch parser. Unknown
function/view/type provenance stays unknown; it is not inferred from current
values or guessed solely from expression syntax.


Final verification (2026-09-22): formatting, strict Clippy, all 328 workspace
Rust tests and all 340 client/harness tests passed, with no failures,
cancellations or skips. Reference containers were removed, the dedicated VM
was stopped, and the original Docker context was retained.

Property acquisition and result-type binding currently obtain separate catalog
snapshots. Consolidating them into the unified result contract should avoid
those repeated catalog reads as well as parallel descriptor vectors. No runtime
performance improvement is claimed by this extraction.


## Grouping keys and expressions evaluated after grouping

`msduck-sql::grouping_properties::Plan` collects grouping keys from an explicit
AST and row-source snapshot. It reuses grouping validation and bounds without
expanding the Cartesian product of grouping sets. Name resolution canonicalizes
qualified and unqualified references to the same source field; parentheses and
internal integer-cast markers do not change key identity. The caller's AST and
catalog remain unchanged.

A computed key becomes a derived result. In advanced grouping, every key is
nullable, including keys present in every grouping set. Property inference checks
for complete key matches before recursively inspecting expressions. Thus a CASE
used as a ROLLUP key is nullable for the grand total, whereas `ISNULL(n,0)`
evaluated after `ROLLUP(n)` remains a non-null computed expression. Wildcard
projections apply the same key properties to their fields.

VALUES columns now infer known literal/member declarations and merge supported
numeric or character declarations across rows. Bare NULL entries do not override
other declarations; unknown or unsupported combinations remain unresolved.
This supplies the static input type needed by post-group conditional inference.

Eleven SQL Server 2025 probes are preserved in
`artifacts/compatibility/grouping-properties-probes.json` and
`artifacts/compatibility/grouping-properties-extra-reference.json`. They cover
computed keys, always-present keys, post-group expressions, qualified references,
stars, real NULL detail rows and empty output. The focused tedious regression
matches all eleven row/flag results. The original computed CASE ROLLUP probe
returned a NULL grand total while msduck incorrectly advertised NOT NULL.
Fixed versus nullable wire type differences remain separate unfinished work.


Grouping audit verification (2026-09-22): all 277 captures completed. Of the
276 preceding executions, 274 were unchanged; two changed only flags, both to
match the preserved SQL Server reference. No previously matching flags regressed,
and no captured NULL was marked NOT NULL. The new six-query audit flag matrix
matches its live reference. Raw differences are retained in
`artifacts/compatibility/grouping-properties-local-diff.json`. Formatting, strict
Clippy, 329 Rust tests and all 341 client/harness tests passed, with no
failures, cancellations or skips.


The next wire-encoding boundary is established by
`artifacts/compatibility/sql-server-fixed-scalars.json` and the paired
`fixed-scalar-before.json`. Five live SQL Server probes cover TINYINT, SMALLINT,
INT, BIGINT, BIT, REAL, FLOAT, MONEY and SMALLMONEY: populated and empty NOT NULL
tables, outer joins, ISNULL repairs and typed NULLs. Current rows and flags match.
The three proven-non-null probes each differ only in nine wire types and nine
length metadata fields; the two nullable probes match completely. Fixed scalar
payloads must omit the nullable length prefix as well as select the correct
TYPE_INFO token. `Column::fixed_scalar_type` now supplies the shared decision to
metadata and row encoding. A fixed scalar cannot encode NULL; the adapter rejects
it before appending payload bytes. Integer payloads also validate width and range
instead of silently truncating inconsistent backend values. MONEY retains its
high-word-first order. The five updated local raw captures in
`fixed-scalar-after.json` match the reference completely. Formatting, strict Clippy
and all 332 Rust tests passed on macOS and Linux after the conditional
correction. Final remote verification also passed all 343 client/harness tests
and completed all 279 audit captures.


Fixed encoding exposed two pre-existing COALESCE property errors in the broader
audit. The preserved reference retains nullable SMALLMONEY metadata when the
selected non-NULL value requires conversion. The SQL planner now compares known
primitive precedence and decimal declarations for that candidate. ISNULL uses a
separate NULL-propagation rule for ordinary casts; TRY_CAST remains nullable.
Grouping-key context takes precedence over scalar cast reasoning. Five additional
live probes in `sql-server-coalesce-conversion-properties.json` and
`sql-server-coalesce-shape-properties.json` establish those distinctions. Their
row/flag matrices pass the focused client test; complete type/length equivalence
for those additional conditional probes is not claimed.

The corrected fixed-scalar audit completed all 279 cases. Of 277 preceding
executions, 228 were unchanged and 49 changed. Changes were confined to wire
types, lengths and flags except for two existing rows swapping order in the
unordered `derived table apply` query. Across corresponding execution and reuse
columns in the preserved SQL Server capture, 611 type values and three flag
values improved to matches; no previously matching type or flag regressed.
No captured NULL was advertised NOT NULL. Raw differences remain in
`artifacts/compatibility/fixed-scalars-local-diff.json`.

Eight pre-change tests expected nullable descriptors for proven non-null results:
ALTER COLUMN, ISNULL, numeric literals, grouped source columns, and source columns
beside window functions. Their corrected metadata assertions pass in the complete
343-test client suite, with no failures, cancellations or skips. The live reference
matrix in `sql-server-fixed-existing-expectations.json` confirms fixed Int/Float
literals, fixed SmallInt ISNULL results, and the distinction between fixed source
columns and nullable aggregate/window outputs. The final remote 279-case audit
matches the preserved macOS capture exactly, including row order in this run;
`artifacts/remote/linux.local/fixed-scalars-baseline-comparison.json` retains the
comparison. These results precede the XACT_STATE declaration correction.


### Original login and conditional operand declarations

`ORIGINAL_LOGIN()` carries a declared NVARCHAR(4000) result from SQL planning to
wire metadata, rather than deriving capacity from the actual login name. Eleven
live reference captures match, including empty results, derived sources, ISNULL,
COALESCE and compilation errors. The shared conditional planner now consults the
existing literal declaration rules for runtime operands. Literal ISNULL results
retain their first argument capacity, with the SQL Server minimum one-unit
capacity for an empty literal; Unicode lengths count UTF-16 units.

Additional reference probes are retained in
`artifacts/compatibility/sql-server-conditional-literals.json`, paired with
`conditional-literals-after.json`. These expose remaining constant-folding
metadata gaps: SQL Server may choose the selected literal's width instead of the
widest branch, while MAX conversions retain separate nullability behavior.
Those captures predate the CHOOSE/MAX corrections below. Literal-only conditional inference
remains conservative until the compiler models that folding; raw mismatches are
preserved, not normalized. All result rows in these five captures match.


### CHOOSE, MAX conversions and parameter declarations

CHOOSE result metadata now considers every character arm, including constant,
NULL and out-of-range indexes, and retains nullable computed flags. COALESCE
preserves MAX capacity and marks bounded-to-MAX conversion nullable; ISNULL keeps
its separate first-argument and nullability rules. Both Unicode and non-Unicode
families retain their wire identity.

Projection scope now receives explicit parameter type metadata. These declarations
survive CTE and scalar-subquery boundaries without reading parameter values or
adding database/session dependencies to the SQL crate. Root result-type adaptation
also retains character parameter declarations in its metadata-only AST copy.

Evidence: `artifacts/compatibility/sql-server-conditional-flags.json` and
`conditional-flags-after.json` retain eight raw reference/local pairs. Five pairs
match completely; the two DECLARE pairs differ only in completion counts. The
bounded typed-NULL COALESCE pair still differs in character family/width. The two additional pairs in
`sql-server-choose-width.json` and `choose-width-after.json` match completely.
Focused client tests pass for CHOOSE, MAX conversions and parameters changing to
NULL. Pure tests cover immutable ASTs and declarations crossing CTE/subquery
boundaries. Formatting, strict Clippy and all 341 workspace Rust tests passed locally.
Full client/audit verification is running for this snapshot. Constant-folded bounded COALESCE widths and DECLARE completion counts
remain open; captures preserve those differences.


The initial 288-case local audit completed: 281 of the 285 previous cases were
unchanged, and four had metadata-only differences. Comparison against retained
SQL Server captures confirmed the character changes, but identified a DATETIME2
CHOOSE regression (flags 33 instead of 1). The conditional provenance fallback
now includes CHOOSE, with an explicit temporal client regression assertion.
`conditional-parameters-audit-diff.json` preserves the initial differences;
the corrected snapshot requires a new audit. The in-flight Linux verification
still covers the earlier snapshot and cannot validate this correction.


### Session counter declarations

SQL planning now recognizes @@ROWCOUNT, @@TRANCOUNT and @@ERROR as non-null INT
counters. Their runtime values remain in the session adapter. The deterministic
counter declaration feeds both projection inference and result properties, so
empty results, COALESCE/ISNULL/IIF and CTE forwarding retain their integer family
and nullability. CHOOSE remains nullable because its index can select no arm.

Five new live reference captures are retained in
`artifacts/compatibility/sql-server-rowcount.json`. Direct counters have fixed INT
metadata and computed flags 32; CTE forwarding removes the computed flag; CHOOSE
has nullable IntN metadata and flags 33. Local full verification is in progress.
The previous Linux snapshot passed all 354 client tests, but predates DECLARE,
temporal CHOOSE correction and these counter declarations.


The session-counter local audit completed all 290 cases. Of 289 previous
captures, 287 were unchanged. One changed two @@ROWCOUNT descriptors to fixed INT
with flags 32; one unordered APPLY query reversed two rows. Raw differences are
retained in `session-counters-audit-diff.json`. This snapshot predates logical
result labels and unary-plus lowering.


The session-counter snapshot subsequently passed all 357 macOS client/harness
tests, with zero failures, cancellations or skips. Together with formatting,
strict Clippy, 341 Rust tests and the completed 290-case audit, this closes its
local verification. Logical result names and unary-plus lowering are later
snapshots with separate pending gates.

### Constant CASE selection

`projection::constant_case` now evaluates bounded integer constant predicates
when inferring CASE result properties. Searched and simple CASE handle SQL
UNKNOWN, checked integer arithmetic and three-valued AND/OR/NOT. Variables,
columns, volatile functions, unsupported expressions and arithmetic failures
remain unknown. This changes metadata inference only; it does not remove or
execute expressions in the AST. Grouping properties take precedence.

The selected arm retains its own provenance only when its catalog declaration
matches the complete CASE declaration. Unreachable arms still participate in
type resolution: selecting an INT column with a BIGINT alternative requires a
conversion and retains nullable computed metadata. Selecting the same column
with an untyped NULL alternative preserves stored-column flags. An ordinary
cast still retains its own nullable metadata.

`reference/case-constant-properties.json` records 20 live SQL Server programs.
Two pure regression tests cover their flags, immutable ASTs, bounded evaluation,
and exclusion of runtime values. The public replay in
`artifacts/compatibility/case-constant-properties-replay.json` matches all raw
captures and DONE tokens for 19 programs. Integer overflow retains three
capture differences (missing descriptor, error state and message) and two DONE
field differences. Those backend execution gaps are explicitly preserved by the
client test; they are not a passing compatibility claim. Constant character
comparisons, including the previously recorded BIN2 CASE metadata differences,
still require collation-aware constant evaluation. Full workspace and client
verification is recorded separately in the verification artifact.

The constant-integer-CASE revision passed local formatting, strict Clippy and
465 workspace Rust tests. The focused client test passed with the exact overflow
gaps above. The frozen Linux full-client/audit run remains in progress; see
`artifacts/compatibility/case-constant-properties-verification.json` for its
process handle and scoped results.

### Native integer overflow before result description

The root adapter now recognizes native INT32/INT64 addition, subtraction and
multiplication overflow and emits SQL Server error 8115, state 2. Recognition
requires the exact backend envelope, supported operand types, and a checked
operation that actually overflows. Typed application errors retain their identity.
Conversion overflow remains a separate diagnostic category.

DuckDB may raise constant arithmetic overflow during preparation. For an ordinary
SELECT, the adapter can now emit a complete descriptor from the already-bound
logical fields before that error; unknown or incomplete shapes remain unavailable.
It does not evaluate expressions to obtain metadata. Query arithmetic-error DONE
tokens retain the SELECT command code, and the existing continuation/TRY-CATCH
paths retain their result prefixes.

`reference/integer-overflow.json` contains seven live SQL Server probes covering
INT arithmetic, BIGINT addition, CASE conditions, continuation and TRY/CATCH.
All seven public replays match their captures and decoded DONE tokens exactly.
All 20 constant-CASE probes now match as well, superseding the overflow gap in
the previous subsection. Both focused client tests pass. Broader verification
is running separately; these comparisons do not establish full compatibility.

The reference generators now canonicalize DONE objects before writing JSON,
preserving undefined-field markers that JSON previously omitted. The previous
CASE replay is archived as
`artifacts/compatibility/case-constant-properties-before-overflow-fix.json`.
One of its two DONE differences was the serialization mismatch; the other was
the SELECT command code. Fresh reference captures prove both corrections.
The upstream engine's `udf.ts` also distinguishes bounded arithmetic errors from
conversion errors; the exact states here are established by the live probes.

### Alignment of logical facts with physical results

`result_metadata::Aligned` supplies one alignment decision for normal Arrow
results, prepared error descriptors and logical-only preparation failures.
Logical fields and declared overrides each describe a whole result. When either
list has a different number of columns from the physical schema, none of that
list is applied by position. The other, aligned list can still supply its facts.
The existing physical-type fallback and unknown properties remain available;
this does not infer SQL declarations from row values.

Previously, names and collations checked column counts, but nullability/origin
and type overrides could still be borrowed from an unrelated prefix. Native
adapter regression tests reproduced a NULL failing fixed-scalar encoding and an
error descriptor advertising fixed, non-null INT after receiving a one-field
logical list for a two-column physical result. These are direct adapter
reproductions, not a newly established public-SQL trigger.

The regressions exercise shorter and longer field/override lists, both column
orders, actual NULL rows, and complete descriptors with duplicate SQL labels,
Unicode capacity and collation. Complete descriptors retain the same metadata
for populated, empty and prepared-error paths. Logical-only error description
continues to require a complete known shape; it does not emit partial metadata.
This fixes positional alignment, not the broader duplicated declaration binding
or all physical/wire type differences.
