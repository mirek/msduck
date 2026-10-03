# Typed source conversion for CONCAT_WS and TRANSLATE

`msduck_sql::concat_conversion` composes the reviewed function planner/evaluator
with the registered `concat_text_conversion`, `numeric_text` and
`temporal_guid_text` helpers. It accepts original source declarations, explicit
collation catalog/default, language and statement diagnostic context. Planning
receives no values. The private plan snapshots those inputs; later bindings do
not change source kinds, allocations, flags or collation metadata.

Evaluation accepts borrowed, already acquired character units or binary bytes,
exact numeric values (integer, Decimal coefficient, money coefficient and
REAL/FLOAT bits), or exact stored temporal/GUID values. It does not evaluate an
AST operand, construct storage values, query a catalog/session, read an environment
or call a backend. Character/legacy text is already decoded under the original
source encoding; fixed character and binary padding must already be present.
It returns exact optional UTF-16 units and exposes the function's result plan.
Aliases and wire descriptors/tokens remain adapter responsibilities.

Character declarations retain their family/width and collation. Literal NULL is
separate from typed NULL. Numeric and temporal/GUID sources remain their original
logical types; their converted text never creates fabricated character sources.
The function core still decides Unicode promotion, collation diagnostics,
nullability flags, separators, NULL skipping and translation mismatch/first-map
semantics. Callback and indexed matching APIs require established caller-supplied
TRANSLATE equivalence, never guessed weights or ordinary SQL equality.

The old binary conversion helper is unchanged. Its captured BINARY/VARBINARY
2/10/8000/MAX contracts preserve stored bytes, fixed padding, CP1252 decoding,
UTF-16LE reinterpretation and odd-byte zero padding. TRANSLATE now retains MAX
only for an original binary MAX declaration with an explicit MAX conversion
contract, as measured in #815. Other noncharacter MAX contracts remain unknown;
missing/inconsistent binary MAX allocations are rejected. CWS allocation is still
declaration-derived; Unicode binary widths are measured in converted units.

Temporal #835 grounds a separate allocation contract for every modern scale
0 through 7: CWS allocates 40 units and bounded TRANSLATE 8000 ANSI/4000 Unicode
units. This extends the composition API without widening the unchanged older
helper's 0/2/3/7 admission. Formatting still validates stored scale alignment,
legacy ticks/minutes, local offsets and mixed-endian GUID identity. Numeric
allocation remains limited to the old helper's captured decimal declarations;
a formatter's ability to display another Decimal is not evidence for metadata.

Arity is checked before cloning declaration arrays. Aggregate conversion input
is limited conservatively to 8,388,608 units/bytes before any converted text
allocation, bounding intermediate storage by a fixed factor. This is an
implementation barrier, not a SQL Server capacity or SQL error. The reviewed
function core additionally retains its input/output, comparison and distinct-key
limits, per-value versus separator truncation, NULL and exhaustion behavior.
Opaque key sizes remain the caller's responsibility. No unbounded source
conversion, repeated-separator amplification or second operand acquisition is
introduced.

The composition replay reads all four retained observations of #815, #827,
#831, #833 and #835. For each available function result it checks exact strings
and UTF-16 plus family, length, flags, collation identity and all captured
function descriptor properties represented by this API. Full prepared column
arrays remain equal to their actual prepare arrays across bindings. Native
source declarations/aliases, completion tokens and construction belong to the
unchanged reference observer and adapter; this pure layer does not fabricate
those properties or claim a new wire replay.

#827 contributes 1,878 function compositions and 412 independent explicit-style
controls per run; all 77 source-overflow observations per run retain complete
native-source diagnostics and zero-row failure shapes. #831 contributes 8,728
function compositions and 4,364 separately labelled explicit controls per run.
#835 contributes 12,080 function compositions and 8,840 independent style/default
controls across four runs, with all 300 construction failures retained separately.
Source inputs use exact original coefficients/IEEE bits, native SQL stored bytes
or actual prepared payloads; modern lossy JS Date carriers are never inputs.

#815 contributes 770 function compositions and 56 full diagnostics per run,
plus literal-NULL and ordered source rows. Binary source bytes and character
padding come from native observations. Its modern temporal source decoder uses
the independent native/default-CAST observation, which retains exact local
fractional fields and offsets. Legacy native millisecond display is inverted only
on its discrete 1/300-second lattice and verified by a round-trip; SMALLDATETIME
minutes remain distinct. Decimal/money coefficients use original scalar inputs,
not imprecise JS numbers; native REAL/FLOAT JSON numbers preserve their IEEE
identities. #833 adds 936 full diagnostics and 144 legacy text outputs across
four runs, preserving Unicode prescan and first-incompatible-source ordering.
The unchanged core suite retains the ordinary/prepared/mismatch/cap/NULL and
#818 truncation regressions.

Unknown styles, source declarations/decimal allocations, languages, collations,
encodings and matching keys remain explicit. Function implicit conversion rejects
an explicit style instead of silently treating it as default. ANSI UTF8 output
is not admitted by this composition API. Unsupported TRANSLATE legacy source
argument 1 retains captured 8116; uncaptured argument positions remain unknown.
Malformed variable TRANSLATE units from #844 remain unknown and are not imported
as stable expected values. Numeric rounding remains the documented finite-grid
implementation inference. General locale/style/source coercion, complete matching
weights, AST binding/acquisition, DuckDB/native adapters and wire/runtime
integration remain subsequent tasks; no engine or root adapter changes occur here.
