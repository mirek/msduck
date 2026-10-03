# Deterministic CONCAT_WS and TRANSLATE work

The new `crates/msduck-sql/src/concat_ws.rs` module plans character declarations
from explicit argument declarations and catalog properties, then evaluates
already SQL-converted UTF-16 text. It does not acquire catalogs, execute source
expressions or consult a session/backend. Planning takes no parameter values.
The module remains unregistered under this task's three-file scope; its
integration test includes it by path. GREATEST/LEAST has since merged, so a
separately claimed export task can register the module.

The authority is the complete, independently reproduced SQL Server capture in
[concat-ws-translate.md](concat-ws-translate.md), merged through PR #807. The
blocked original reference task 253 and unclaimed core task 290 remain preserved;
this worker owns the fresh successor 812 and its independent receipt.

Current behavior includes CONCAT_WS arity 189, literal NULL width 0, empty literal
width 1 supplied by the binder, separator width per argument gap, character
family/MAX promotion, an INT conversion width 12, bounded width caps, NULL
skipping and empty-result behavior. TRANSLATE preserves first-mapping precedence
without chaining, counts UTF-16 units or supplementary characters from explicit
catalog properties, propagates NULL, distinguishes error 9828 states 1/3 and keeps
MAX only when its first argument is MAX. Compile diagnostics retain number,
state, severity and text; root adapters still own source locations, wire order
and statement/transaction effects.

Collation labels combine through existing core rules. Unknown, absent or duplicate
catalog entries are barriers. An implicit CONCAT_WS collation conflict also needs
an explicit SELECT projection position to render diagnostic 451; absent statement
context returns `UnknownDiagnosticContext`, rather than inventing column 1.
The position is nonzero and passed through `plan_with_context`. Comparison weights are an explicit matcher input
and may return unknown. The tests provide only comparisons established for the
captured cases; they do not claim to implement all linguistic weights. Numeric,
legacy temporal and GUID formatting remains in separate caller conversion code;
uncaptured per-type formatting widths remain unknown. Unsupported legacy types
and ANSI UTF-8 evaluation do not acquire invented behavior. Character payloads
must respect their declared widths and fixed-width padding. Core CP1252 encoding
validates ANSI payloads; Unicode payloads retain isolated surrogate units.

Thirty-one private Rust tests pass using cached, compiler-compatible Linux
dependencies and isolated temporary binaries, with strict Clippy and formatting.
They compare 21 ordinary character cases across all four captures (84 comparisons),
26 declaration/collation cases (104 comparisons), 10 supplementary/UTF-8/mismatch
cases (40 comparisons), 14 character RPC requests (56 comparisons), four column
batches (16 comparisons), four supplementary/explicit integer-text cases (16
comparisons), prepared CONCAT_WS rebindings and 32 prepared TRANSLATE executions.
They also check the 254-argument boundary, predicate inputs, isolated surrogate
fidelity, bounded truncation and MAX behavior, and 13 complete compile diagnostics
across all four captures. Column evaluation retains the three successful rows
before the fourth row's runtime error. Subsequent statement execution and wire
completion ordering remain shell obligations, not claims about this core.

The test reader converts only the four raw `"\ud83d-a"` strings into exact UTF-16
unit carriers because serde_json cannot represent an isolated surrogate in a Rust
String. The fixture remains unchanged; the tests compare original units instead
of replacements. MAX DATALENGTH retains the captured BIGINT string representation.
Unknown catalog entries, duplicate names, unavailable comparison weights, missing
conversion contracts and invalid payloads remain explicit barriers.

Independent review found missing width/encoding validation for converted numeric
payloads. The regression failed on the original checkpoint and passes after every
payload receives those checks. A second review found an implicit-collation error
assumed SELECT column 1 without context; a regression rejects a mutation restoring
that default. Positions 2/17 are rendering tests, not new SQL Server captures.

Noncharacter source formatting is not implemented here. The fixture's aggregate
CONCAT_WS integer-family, decimal/float/money/bit, temporal, GUID and binary cases,
mixed temporal RPC, and TRANSLATE decimal/date/binary source conversions require
separate conversion adapters and independently established declaration widths.
Tests supply already-converted INT text only where its width 12 is established.
This does not certify those other formatters. Server setup, connection reuse,
request execution, comparison/predicate lowering and completion tokens require
root integration. ANSI UTF-8 result evaluation remains explicitly unsupported. A noncharacter
TRANSLATE first operand with an explicit MAX conversion width also returns
`UnknownConversion` until its result shape is established; it must not advertise
8000 and silently truncate. The new VARBINARY(MAX) regression fails on checkpoint
2adc076 and passes with that barrier. Merged reference #815 records this rule alongside individual source-format declarations;
task #819 supplies conversion contracts before a separately claimed integration.

Large TRANSLATE inputs can use `evaluate_with_keys`, with caller-supplied,
established SQL character equivalence keys. A deterministic BTreeMap preserves
the first mapping and gives O((input + mapping characters) * log(distinct mapping
keys)) lookup work without hidden randomness. Key extraction occurs once per
mapping/input character, and input splitting streams instead of allocating an
array for the entire input. No chaining or surrogate normalization is introduced.
The original comparison callback path caches each distinct input unit and stops
with `ComparisonLimit` after one million comparisons; that barrier is not a SQL
Server diagnostic. Adapters must supply stable weights/keys and choose a supported
strategy instead of translating a work limit into an invented SQL error.

Performance regressions verify a million-character repeated input with an
8000-character mapping takes 8000 matcher calls, while the actual old 15ecc3e
implementation fails immediately at call 8001. A distinct-input case reaches the
explicit comparison limit; the indexed path returns the correct value with only
12096 key extractions. Captured ordinary and supplementary/UTF-8/mismatch probes
also check the indexed path using only their established equivalence classes.
Keys remain unknown for unestablished linguistic behavior.

Full final-head checks, CI and review must pass before merge. Registration and
runtime binding/execution need separate claimed successors. This deterministic
module does not establish server compatibility.

The owner-pinned mirek/mssqlite dispatcher at
`7f71f2081602f8e3051998f5c11f058e65fe24ec` maps CONCAT_WS to SQLite and TRANSLATE
to a custom backend function. Its AST/backend separation is useful context;
those mappings are not SQL Server ground truth and are not copied as semantics.

TRANSLATE counts mapping characters without allocating per-character slice vectors,
then streams borrowed UTF-16 character slices in both evaluators. A million-entry
duplicate MAX mapping produces the first replacement with one opaque comparison;
the keyed evaluator keeps one distinct key. Large unequal mappings still return
the captured 9828 diagnostic before consulting unavailable comparison weights.

Evaluation has explicit resource policies: each UTF-16 input and output payload
is limited to 16 MiB. Inputs are checked before temporary encoding validation;
MAX CONCAT_WS checks the complete value/separator size before output allocation.
TRANSLATE also checks incremental replacement growth. `InputLimit` and
`OutputLimit` are resource barriers, not SQL Server diagnostics or claims about
SQL Server MAX capacity. NULL values contribute no separator gaps.

A private four-observation SQL Server boundary capture (SHA-256
`49208568faacbb583ba24ec3ec6fc45841e773625f0ff53ce6b872939d441e29`)
shows bounded CONCAT_WS drops a valid surrogate pair cut inside a value argument at the 4000-unit cap
under both ordinary and SC collations; SC TRANSLATE does the same on replacement
growth. Native CAST retains the high surrogate and therefore is not a substitute
for these function rules. Further four identical 36-record observations (SHA-256
`f0f0e55238001a905f96eb3de8c5607573ef7732f4bc9f2f11ced4bae0cbbdc6`)
establish that argument boundaries must remain visible: CONCAT_WS retains a high
surrogate whose low half arrives in a later argument, and retains a lone high
surrogate at the cap. A pair cut inside one argument is dropped, and a later
argument cannot refill the space. Evaluation therefore truncates each append
independently and latches exhaustion, rather than flattening and trimming the
whole output. The actual earlier whole-output implementation at 3ed52ac fails
the new cross-argument regression (25 pass / 1 fail). Expanded boundary and
lone-surrogate controls have since merged through PR #818 as
`reference/concat-boundary.json`; their native CAST controls retain the distinct
behavior rather than serving as substitute function expectations.

Four identical expanded 44-record captures (SHA-256
`a9aacc82bdfd34f6fb854cf95d2a8fda9a2a3b9eb20b46e55fb6d78c13219ecf`)
show that CONCAT_WS separator truncation differs again: a separator pair cut at
the cap retains its high surrogate under ordinary and SC collations. An exactly
fitting separator pair remains intact, with the following value excluded. The
append operation therefore receives an explicit value/separator role. The prior
per-value-only snapshot fails the added separator regression (26 pass / 1 fail).
Raw UTF-16 carriers preserve these differences instead of replacing isolated
surrogates or borrowing CAST behavior.

Both TRANSLATE lookup paths retain at most 65,536 distinct entries. The indexed
path checks each vacant key before insertion; the callback path checks each new
input-unit cache entry before matching. `LookupLimit` is an explicit resource
barrier. Duplicate mapping keys still keep the first replacement and consume one
entry. Callers remain responsible for bounded allocation in their supplied key
representation. A supplementary mapping with 65,537 distinct pairs is rejected
before growing the index beyond the limit; a short input can instead use the
bounded callback path. Exactly 65,536 keys plus additional duplicates remain
supported. Empty input avoids lookup construction after length-mismatch validation.

Already-converted bounded binary operands use the result text domain during
payload validation. For Unicode output, bytes 0x4142 become UTF-16 U+4241 and must
not receive a CP1252 representability check based on the original binary kind.
The actual 4c62ddb probe rejected that valid converted input with InvalidPayload;
the regression now preserves it. ANSI output still requires CP1252 text, and
original byte conversion/storage bounds remain the adapter/helper responsibility.
The source kind remains binary for declaration planning and unsupported/MAX
barriers; it is not replaced with a fabricated character declaration.

A resolved NoCollation operand retains its source encoding when both conflicting
labels establish the same encoding, even if the database default differs. An
explicit result collation does not recover provenance when source encodings
differ; planning returns UnknownEncoding after preserving unresolved collation
diagnostics. A regression with CP1252 source labels and a UTF-8 default rejects
the actual f1550d3 behavior, which measured the source in the default byte domain.
These are explicit-input domain checks, not additional SQL Server captures.

A read-only runtime probe at server revision
`361fa520c74341767de77ef06e9a83cd27371a04` confirms that backend function
behavior still bypasses this unregistered core. For the exact retained query
`SELECT CONCAT_WS(NULL,'a','b') AS value`, msduck returned NULL with
NVarChar length 65535 and flags 1; all four SQL Server observations return
`ab` with VarChar length 2 and flags 32. For the exact retained query
`SELECT TRANSLATE('abc','ab','x') AS value`, msduck returned `xc` without an
error, with the same NVarChar MAX descriptor; the reference emits VarChar length
8000 and flags 33, no row, and error 9828/state 1. A subsequent SELECT succeeded.
The diagnostic used a separate ephemeral listener and the existing public
`compatibility.mjs` observer, retaining rows, descriptors, errors and public
completion events privately; it is not a full raw-token comparison or a runtime
pass. Registration alone cannot fix these differences: adapters must bind the
declaration plan and route execution through the deterministic rules, preserving
error metadata and statement behavior.

Independent review against merged conversion reference #815 found that Unicode
CONCAT_WS XML/SQL_VARIANT rejections named `varchar` instead of `nvarchar`.
The regression fails on efff2ef and checks all 24 source/NULL/argument-role
observations across all four captures (96 exact diagnostics). Planning now
selects the conversion family from all original character declarations before
rejecting a source, without consulting values. A separate rendering test covers
Unicode following the rejected source; it is not a new SQL Server capture.
