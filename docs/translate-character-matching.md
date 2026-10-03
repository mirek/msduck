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
Two paths differ in the SC_UTF8 lone-low suffix program: the translated text
cell at `/1659/result/sets/1/rows/0/0` and its raw row carrier. The text changes
from U+0058 U+DE00 U+0000 to U+0058 U+DE00 U+0002; the actual first-cell raw
payload changes from `580000de0000` to `580000de0200`. The separately evaluated
binary projection stays `580000de0000` in both runs. All original source,
declaration, metadata and error fields remain equal. Malformed UTF16 outputs are measured
variable evidence and remain unknown; this is not a global equality pass.

Earlier diagnostic pilots remain on Linux, including rejected multi-SELECT and
pre-ROW-observer captures. Their source snapshots and raw files are not final
passes. The raw-row bootstrap observer captured the retained fixture; the
validator then added fixed INT decoding and passed an offline check of every
raw field before exclusive fixture creation. The observer/query code is
unchanged by that validation correction or the immutable per-run digest guard.
The final frozen generator has SHA256
`029a15fc260b046292774e8084d6084aaa26a6b0004dfff48551b10768a91f67`.
It completed four fresh observations and a complete independent second four.
Each set reproduced all ordinary controls, native source bytes, declarations,
descriptors, diagnostics and prepared bindings unchanged. Each has eight exact
malformed-output differences against the retained fixture, recorded with full
values, paths and run identities; neither is a global equality pass. There are
156 retained SQL diagnostics per run (9828 states 1 and 3), plus all prepared
execution diagnostics, 16 prepares and 104 executions per run. Nothing is
filtered or adjusted to obtain equality.

The private Linux evidence remains under
`/home/mirek/.cache/msduck/reference/translate-character-matching-3fcef9e/row-observer-v1`.
The final four raw artifact `.tmp/final-four.json` has SHA256
`76644eb423637276ffe6b4f99f2da3fb89312253508d3bb92a41bff8ac7f005c`;
its complete comparison sidecar has
`b28b823716c53fd4667b407439c5ded2ad08456724de796ea1870c4b7ae55a79`.
The second four `.tmp/reproduction-four.json` has
`e3f09f88f414adda695ce0dfa187ed62f82368b312dfca480b245a5b5a9195a9`;
its comparison sidecar has
`fba00406afd161373488a1c50678ecfaf87a701ee483cadf53881e5063e862c4`.
The source and canonical fixture stayed unchanged throughout both captures.

Four exact malformed controls were also repeated 20 times per session in three
fresh databases. Each individual session's outputs were stable; across databases
160 exact malformed-output paths differed. Source/declaration/metadata/error
fields stayed unchanged. This distinguishes observed session repeatability from
an established deterministic rule without attributing a cause. The full
1,254,233-byte `.tmp/malformed-repeat.json` has SHA256
`748de8cc007f620c544a0dc0a83e24b280836f93edbf8b858f4177d2a6dbf6d9`;
its private observer source has
`262822795df01c5e34959ec4fe058d280e7cd28f1a6e56ae071286ad65ca19a7`.

ROW/NBCROW all-split, bytewise, NULL, unpaired-unit and truncation proofs pass;
an actual over-limit consumed ROW payload is rejected. RETURNVALUE fragment
proofs, 15 identical-four-copy corruption cases, nine pre-Docker destination/CLI
negatives and five owned lifecycle success/failure cases pass. Immutable digests
protect the exact retained observations, including variable outputs; they are
integrity checks, not independent semantic proof. The Linux staging footprint
is 409 MiB, with bulky raw evidence kept there and only the canonical fixture
copied locally. Owned containers are terminal and removed. Exact-head CI and
reviews are recorded on the PR. No runtime implementation or complete collation
support is claimed; matching weights and malformed-unit behavior remain future
integration work.
