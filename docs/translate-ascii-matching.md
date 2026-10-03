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
varied/full/control/NULL/mismatch/replayed bindings, stable descriptors and
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

Full frozen-source four captures, a second independent four, immutable hashes,
validator/fragment/destination/lifecycle proofs and reviews are pending.
