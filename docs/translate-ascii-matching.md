# Complete ASCII TRANSLATE matching reference

Task #854 records actual TRANSLATE character relationships within the complete
128-character ASCII alphabet under eight explicit collations and original
VARCHAR(512)/NVARCHAR(512) declarations. This extends #841 without changing
its immutable fixture or making nonASCII or malformed-output behavior known.

The inventory is 2,048 single-mapping grid programs, 16 prepared programs and
two server/reuse controls: 2,066 programs per run. Each grid uses one CTE and
SELECT with all original native operand bytes and two TRANSLATE controls whose
nonASCII single-character sentinels are é and Euro. The complete same-evaluation
raw ROW payload is primary evidence for each text cell; the separately projected
CONVERT binary calls remain independent server evaluations. Original ANSI bytes
0..127 first enter the database VARCHAR domain before explicit COLLATE; native
bytes retain recoding. No ordinary SQL equality or generic Unicode folding is
used to derive the measured relationships.

The observer is copied from approved #841 into this scoped script: full ordered
tokens, descriptors including userType/schema/udtInfo/tableName, diagnostics,
raw DONE/RETURNSTATUS, fragment-safe ROW/NBCROW and prepare RETURNVALUE bytes.
Prepared controls retain original declared types and actual emitted payloads,
varied/full/control/trailing-space/NULL/mismatch/replayed bindings, stable descriptors and
connection reuse. Relations are measured independently through both sentinels;
reflexivity, symmetry and transitivity are checked rather than assumed.
Unknown profiles, relationships beyond ASCII and #841 malformed-variable
outputs remain unknown. This fixture does not establish complete linguistic
weights, runtime integration or operational UTF8/SC support.

The diagnostic pilot retained 2,066 programs in 25,035,189 bytes and completed
in 18.02 seconds with peak RSS 276,287,488 bytes. Its raw SHA256 is
`8610e605c281b37ab21ef40ea3a40300d605720aca810997612641044d7bd157`.
The four-run estimate including relation matrices is below the 128 MiB limit.
Containers are owned, randomly named/ported, pinned-image instances with an
explicit hostname, 4 GiB memory, two CPUs and 512 PIDs. Node has a 1 GiB heap;
the private wrapper enforces 2 GiB RSS and a 30-minute wall deadline and cleans
only exact attributable containers. No shared native builder is synchronized.

The canonical 100,454,386-byte fixture has SHA256
`d284c6a7c4d0ff6062492db2de91f6b8bedcd8f9620395ed10c02f0f2184c579`.
All four complete runs have SHA256
`a7bd99b345709a93ab0e8540c70347c5c93cef8949fbde8973e8794db817a7b3`.
The acquired raw-only artifact, written exclusively before semantic relation
validation, has SHA256
`9cdf600f1105b9bd8698f6c6aedf3560f70038d841e96a89b00359baf0a317a4`.
The frozen generator has SHA256
`ef1ae619faa1761c5ccb972aa47fd14908c8fb1bae1e84778b50a5bf803b091e`.
Fresh four observations and a second fresh four reproduce the entire fixture
with zero differing raw fields. Each final frozen capture took 30.01 seconds, with
peak RSS 1,257,222,144 and 1,262,723,072 bytes respectively.
Final fidelity tests took 67.01 seconds with peak RSS 1,476,485,120 bytes. All 16 profiles
satisfy the measured reflexivity, symmetry and transitivity checks within ASCII.

| Captured profile | Measured ASCII classes in each declared domain |
| --- | --- |
| SQL_Latin1_General_CP1_CI_AS | 102: 26 A/a through Z/z pairs and 76 singletons |
| Latin1_General_100_CI_AS | 102, same measured ASCII pairs |
| Latin1_General_100_CI_AI | 102, same measured ASCII pairs |
| Latin1_General_100_CI_AS_SC | 102, same measured ASCII pairs |
| Latin1_General_100_CI_AS_SC_UTF8 | 102, same measured ASCII pairs |
| Latin1_General_100_CS_AS | 128 singletons |
| Latin1_General_100_CS_AI | 128 singletons |
| Latin1_General_100_BIN2 | 128 singletons |

These are observations of TRANSLATE, not deductions from names or ordinary SQL
equality. ASCII NUL, controls and space retain distinct identities. Sentinels
are output markers only: observing é/Euro replacement payloads does not extend
matching coverage beyond ASCII. Their actual native conversion yields CP1252
`e9`/`80`, UTF8 `c3a9`/`e282ac`, or UTF16LE `e900`/`ac20` as captured.
Every control succeeds with a one-character ASCII mapping and each sentinel,
independently establishing the admitted marker's SQL character count.

Each prepared program has six executions (96 per run), including a literal
trailing space and embedded NUL, typed NULL, a genuine 9828 length mismatch and
an original replay after the error. Metadata remains identical to preparation;
actual emitted TYPE_INFO/length/payload and post-collation native source bytes
remain separately retained. NULL and error outcomes are not replaced by empty
strings or passing rows.

Only redundant derived relation matrices are packed. `rowsHex[m]` has 16 bytes:
bit `s & 7` of byte `s >> 3` records whether ASCII source `s` matches mapping `m`.
Rows and bits are explicitly labelled in the fixture, independently decoded
and recomputed against both raw text observations. Every original observation
field remains complete. Axiom failures count every violation but retain at most
16 witnesses per property, keeping unknown reports bounded.

The production raw-first path has a failure regression proving an invalid
relationship leaves the acquired server diagnostic and original raw bytes
intact. Twenty identical four-copy corruption tests refresh integrity hashes
before checking observer/input semantics; five packed-relation corruptions are
also rejected independently of raw digest pins. ROW/NBCROW and RETURNVALUE
fragment tests preserve NULL/isolated units and reject truncation/over-limit
payloads. Nine CLI/destination negatives refuse before Docker, and five owned
lifecycle success/failure cases verify cleanup. Final proof/review completion
is recorded on the PR.

Private bulky evidence, prior source checkpoints and diagnostic captures remain
under `/home/mirek/.cache/msduck/reference/translate-ascii-a59adb9/.tmp`.
The final capture pipeline checks the frozen generator checksum between both
four-run captures and fidelity tests. The shared builder is never synchronized.
Matching providers and runtime adapters are subsequent work; this reference does
not make the nonASCII or malformed-variable outcomes in #841 deterministic.
