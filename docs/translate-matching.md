# Finite TRANSLATE matching keys

Task #858 adds a registered deterministic matching provider. It does not execute
SQL, decode TDS, acquire catalog state, format source values, or integrate the
server runtime. `Context::new` validates an explicit original VARCHAR/NVARCHAR
domain and exact catalog/wire properties before a caller takes NULL shortcuts.
`validate_plan` checks the independently compiled result domain, collation,
encoding and SC property. Neither call changes the plan or compile metadata.

The fixed evidence profiles are the eight collations in #854 under each original
VARCHAR(512)/NVARCHAR(512) domain. LCID 1033, flags, version, sort ID, CP1252/UTF8,
SC and case properties are checked individually. Names select retained evidence;
no name parsing, generic case folding, ordinary equality or padded BIN2 helper
supplies matching relationships. Other profiles or contradictory properties
return explicit errors, including for an eventual NULL result.

ASCII keys replay the complete 128×128 actual TRANSLATE relationship matrix in
all 16 profiles. Two literal observed partitions retain control/NUL/space identity
and the measured 26 upper/lower pairs where present. Every pair is independently
checked through both actual replacement markers in all four raw #854 runs.
The source fixture SHA256 is
`d284c6a7c4d0ff6062492db2de91f6b8bedcd8f9620395ed10c02f0f2184c579`.

The additional finite table contains 109 stable ordered single-scalar relationships
from all four #841 runs, selected from original native operands and the actual
same-evaluation raw ROW text cell. Original malformed UTF16, multi-character
maps, variable results and unrecognized transformations supply no relationship.
The source fixture SHA256 is
`be7056a2fbe88e681fcec688131f3b58c3d7bc5786112709419c1dc22e4b35ee`.
Each entry records separate known/equal bitsets for the 16 original profiles;
a missing negative is unknown, never a distinct key by assumption.

`Context::certificate` streams borrowed, already SQL-converted UTF16 input and
mapping values, deduplicates their observed characters, and requires every
ordered pair in that finite alphabet to be known and consistent with an
identity partition. This is deliberately stricter than assuming transitivity
across separately measured pairs. For example, a captured closed e/é pair can
be admitted while its union with an unobserved character remains unknown.
Unpaired surrogates in the matching alphabet are unknown in every profile;
valid supplementary pairs
are considered single characters only in the explicitly captured SC profiles.
The default/nonSC surrogate behavior and malformed variable suffixes remain
unknown. Replacement payloads do not extend the matching alphabet.

Each operand is limited to 8,388,608 UTF16 units (16 MiB); a certificate retains
at most 256 distinct characters and fixed u16 operation keys. Construction is
O((input+mapping) log 256 +256²) with bounded retained memory. Lookup performs
no allocation, costs O(log 256), rejects characters outside the exact finite
alphabet and does not extend certificates. Repeated MAX values do not create
per-character records. Keys are meaningful only within one certificate.

The adapter must preserve original source declarations and perform admitted
SQL conversion/native decoding before this boundary. It must use a TRANSLATE
plan, validate context/plan before NULL, and preserve the existing core's
payload validation, character-length 9828 diagnostic and empty-input precedence.
The validation checks the stored plan operation and exact TRANSLATE descriptor
flags (readonly, nullable and case sensitivity) as well as domain, encoding and SC properties. Collation record identity
uses ASCII case-insensitive names; every supplied catalog property still must
match its retained profile. Public result flags cannot change plan operation.
When evaluation requires matching, the certificate's `key` can be passed to
`concat_ws::evaluate_with_keys`. That evaluator retains first mapping wins and
one-pass replacement without chaining. Certificates are value-dependent
matching facts and must never be used to infer result widths or flags.

Prepared #841 ANSI UTF8 e/é bindings retain native CP1252 bytes 65e965e9,
while the result descriptor is UTF8. These invalid-native-UTF8 operands remain
an explicit unknown conversion/context frontier; no fallback encoding is
inferred. Prepared #854 marker projections separately retain recoded UTF8
bytes c3a9/e282ac. These are independently retained observations, not a
general conversion rule.

The current core explicitly rejects ANSI UTF8 evaluation as UnknownEncoding;
matching facts for its measured ASCII/finite sets remain known independently.
Native undefined CP1252 bytes such as 81 retain their SQL byte/unit identity,
while tedious decodes the text cell to U+FFFD. The native unit 0081 round-trips
through the core CP1252 codec; mistakenly reusing the client-decoded U+FFFD as
the SQL operand fails CP1252 encoding (InvalidPayload). Tests check each actual
raw SQL result and client-decoded result separately. They neither substitute a
separately evaluated binary projection for text nor normalize either result to
obtain a pass. These encoding integration barriers remain future adapter work.

The focused replay checks all 8,192 raw ASCII grid observations, 64 prepared
ASCII programs with 384 actual bindings, and stable/unknown #841 finite controls.
It independently decodes complete same-evaluation ROW/NBCROW payloads, retains
original native bytes/declarations, checks full ordered descriptors and actual
prepared TYPE_INFO/length/payload, and preserves NULL, 9828 and repeated bindings.
JSON strings with isolated UTF16 units use a reversible typed unit carrier in
the test reader; the immutable fixtures are untouched. The #841 finite replay
checks 9,284 admitted matching results, 1,036 explicit unknowns, 21,648 complete raw
rows and 624 retained diagnostics. The additional 64 #841 prepared programs
retain all 416 bindings: 260 admitted matching results, 12 explicit unknowns and
80 exact SQL errors; NULL outcomes and descriptor-stable repeats remain intact.
Six complete replay/context tests, the native 0081/client U+FFFD regression and
four plan/context validation regressions cover the public boundary. Focused
test evidence and full workspace/client/audit results are recorded on the
implementation PR with their exact revisions. No general linguistic matching or runtime
compatibility completion is claimed.
