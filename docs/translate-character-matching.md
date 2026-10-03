# TRANSLATE character matching reference

Task #841 captures actual SQL Server TRANSLATE behavior under eight explicit
collations and VARCHAR(512)/NVARCHAR(512) declarations. It does not derive
function matching from ordinary equality, a collation name or generic Unicode
folding. There are 1,682 programs per run, below the fixed 2,500-program guard.

The grid includes all 26 ASCII uppercase/lowercase pairs and 44 enumerated
accent, combining, ligature, ignorable, space, punctuation, control and surrogate
pairs. This is not every ASCII pair or an exhaustive Unicode comparison table.
Each pair retains native operand bytes, independently executed TRANSLATE
controls in both directions, and a separately labelled ordinary equality
control. Separate statements preserve a successful direction when the other
direction raises a length error.

Four blocks admit every ANSI byte from 0 through 255 as original binary SQL inputs, including
controls. Their conversion first uses the database character domain and then
explicit COLLATE; native operand bytes retain any subsequent recoding, including
UTF8. This does not establish directly stored malformed UTF8 behavior. Unicode blocks instead contain UTF16 units
with the same numeric values; they are not asserted to be CP1252 decodings.
Other ANSI programs explicitly convert the retained Unicode source to the
declared domain under the selected collation and retain the actual resulting
bytes, including fallback characters. Thirty mapping controls cover first
duplicates/equivalents, reversed order, no chaining, NULL/empty/mismatch cases,
valid supplementary pairs, split pairs, isolated units, replacement units and
lone-unit prefixes/suffixes. No admitted source or function failure is filtered.

The collations are SQL_Latin1_General_CP1_CI_AS and Latin1_General_100 CI_AS,
CS_AS, CI_AI, CS_AI, BIN2, CI_AS_SC and CI_AS_SC_UTF8. Their properties are
observed through the individual function controls rather than assumed from
their names. The default server and database collation/version are retained.

Sixteen prepared programs use original VARCHAR/NVARCHAR(512) parameter
declarations. Five bindings cover case/accent distinctions, no chaining, NULL
and repeat; Unicode programs add three supplementary/lone/mismatch bindings.
Actual emitted TYPE_INFO, length and payload bytes are recorded once by the
driver encoder hook and checked against independently generated evidence using
the explicit captured connection collation. Prepare descriptors are compared
with every execution's descriptors, including errors. Prepared queries use a
single SELECT; the earlier multi-SELECT pilot with absent prepare descriptors
is retained as diagnostic evidence and is not a final capture pass.

The generator reuses the approved full observer and lifecycle machinery from
#835: complete rows and raw binary carriers, all descriptor fields, full errors
including server/procedure names, ordered tokens, prepare RETURNVALUE bytes and
raw DONE fields. A task-local fragment-safe ROW/NBCROW hook also retains bytes
from the actual text-result evaluation and reconciles them with descriptors and
decoded cells. The separately projected CONVERT binary is a second server
evaluation; it is retained independently and is never assumed equal to the
text-result bytes. UTF16 source/output bytes distinguish server behavior from
JavaScript string decoding. The fixed hostname is an explicit input; container
names, ports and databases remain independently owned. Containers use the
pinned reference image, 4 GiB memory, two CPUs and 512 PIDs. Node uses a 1 GiB
heap in the recorded runs. Request/container/capture watchdogs and retained JSON
bounds apply. Phase bounds are checked after decoding fixed small results;
this does not prove general hostile stream bounds.

The retained 67,246,848-byte fixture has SHA256
`be7056a2fbe88e681fcec688131f3b58c3d7bc5786112709419c1dc22e4b35ee`.
It contains all four complete 1,682-program runs, exact per-run digests and every
differing path/run identity. Three run digests are
`2c024ae0752f86b2a6db4059b7df5b96db044831bf4344a2e192504c4144828d`;
container0/database1 instead has
`2b9c1f9680ea17c9bfb63bf296f20125b7d9c3c69a33b14de7ea5f53602e044f`.
Two paths differ in the SC_UTF8 lone-low suffix program: the separately evaluated
binary projection and its raw row carrier. All original source, declaration,
metadata and error fields remain equal. Malformed UTF16 outputs are measured
variable evidence and remain unknown; this is not a global equality pass.

Earlier diagnostic pilots remain on Linux, including rejected multi-SELECT and
pre-ROW-observer captures. Their source snapshots and raw files are not final
passes. The raw-row bootstrap observer captured the retained fixture; the
validator then added fixed INT decoding and passed an offline check of every
raw field before exclusive fixture creation. The observer/query code is
unchanged by that validation correction or the immutable per-run digest guard.
Final frozen-source fresh-four and second-four reproductions, corruption/CLI/
lifecycle proofs and exact-head review/CI are pending at this checkpoint. The
ROW/NBCROW all-split, bytewise, NULL, unpaired-unit and truncation proof passed.
No runtime implementation or complete collation support is claimed. Matching
weights and malformed-unit generalization remain future integration work.
