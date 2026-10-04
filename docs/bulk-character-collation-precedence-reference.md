# BulkLoad character collation precedence reference

This finite SQL Server 17.0.4065.4 capture separates the `INSERT BULK` source
collation, original five-byte TDS collation, destination column collation and
actual owned database default. It does not implement operational encoding support.

The 126 controls comprise 36 explicit VARCHAR(64), 36 explicit VARCHAR(MAX)/PLP,
36 VARCHAR(64) declarations without COLLATE under three independently set/read
database defaults, and 18 special descriptor controls. Normal controls send
original buffers NULL, empty, `41`, `c3a9`, `f09fa686`; special controls isolate
NULL and `c3a9`. Targets are bounded VARCHAR(64) in CP1251, CP1252 or UTF8, or
NVARCHAR(64). No decoder derives input text before transmission. Each case
records the actual ALTER DATABASE request and DB_NAME/DATABASEPROPERTYEX result;
only the freshly generated owned database is changed, with no simultaneous case
connections or existing user tables.

Four databases in two pinned containers retain 504 complete observations. Every
run has 114 successful loads, six 4002/state2/class16 omitted-descriptor failures,
and six 4012/state1/class16 unknown-descriptor failures. NULL does not bypass
these descriptor errors. The raw `ffff0f0000` descriptor is separate from the
five-zero-byte descriptor, which accepts the sampled NULL and non-ASCII values.

For these original operands, explicit source COLLATE determines conversion;
changing among the three valid supplied wire descriptors does not change the
result. Without source COLLATE, the independently observed database default
determines conversion. Both bounded and PLP source forms exhibit this finite
relation into these bounded targets. Destination conversion remains visible in
native target bytes and SQL UTF16 units; client display is separately retained.
For example `c3a9` reaches Unicode as `1304a900` (CP1251 source), `c300a900`
(CP1252 source), or `e900` (UTF8 source), including the zero-descriptor controls.
The supplementary UTF8 original `f09fa686` reaches Unicode as `3ed886dd` only
under UTF8 source interpretation. These samples do not establish arbitrary
collation acceptance, malformed UTF8 grammar, fixed CHAR padding or legacy TDS.

The fixture preserves original requests, typed descriptors, packets, ROW payloads,
errors/messages/state/class, INFO, DONE, callbacks, native bytes, SQL units,
original session counters and database/server identities. Verification digests
replace checked ephemeral identities only in separate comparison projections;
complete raw comparisons retain all differences. Exact request validation
reconstructs SQL declarations, supplied descriptor bytes, ROW/PLP and DONE
independently. Context SQL and its original packet payload are validated against
the generated database name and declared profile.

Acquisition source SHA256 is
`b3b89ab1edd26fa4c8dd1bc45c49b6072bfb03e0da37cde5051383e63eaf4580`;
the immutable original artifact is 7,962,502 bytes, SHA256
`53c33fed21e40b89ed824c1b077ee7acbea5107d91aca34e555675d90f97ef80`.
The final collector SHA256 is
`a18291b128cbf3aec21362014959b2f82e653b782dd4c465626ea01c1a70d6af`.
Its independent four-database reproduction exited zero: 8,565,694 raw bytes,
SHA256 `6144fcabd12b37721159921fae1a034e6ab8417ea7dac258d7ca8cac381e40fc`.
The complete 9,453-difference sidecar is 6,889,683 bytes, SHA256
`0f3175559ee020296bdb9e5b0ab687169e622d785676039d10f9e364d02cac98`.
Private proof `.tmp/collation-precedence-final2-reproduction-proof.json` verifies all
eight runs, 1,008 observations, exact recursive sidecar and 576 within-run
valid-wire result comparisons. No identities/counters/errors are normalized
in either original artifact. All 17 focused tests pass on macOS and Linux,
with zero failures/cancellations/skips/TODO; final-source reproduction and
all owned resource cleanup are terminal. The unpinned acquisition intentionally
failed validation only after retaining its complete original raw observations.

The collector pins reviewed
helpers and limits each exchange to 2 MiB/1024 packets, the aggregate capture to
48 MiB, and JSON traversal to 500,000 nodes/depth64. It guards output aliases
before Docker, retains bounded raw evidence before semantic validation, records
complete comparison sidecars or an explicit omission failure, and awaits owned
container/database cleanup. Focused tests reject coherent request/response/default
corruption and test trace/budget/retention/CLI failures using bounded assertion
messages. No native server or runtime adapter change is included.

The final validator also pins the complete context request payload, including
all 22 ALL_HEADERS bytes, after replacing only its checked owned database
identity in a separate digest. Independent inspection found all 504 original
context headers exactly `16000000120000000200000000000000000001000000`.
Four coherent mutations of internal length, header type, transaction descriptor
and outstanding count leave SQL, packet lengths and decoded context unchanged:
predecessor `571750b` accepted all four, corrected validation rejects each.
Private proof `.tmp/collation-precedence-context-header-before-after.json`
retains this result. Earlier four-database reproduction evidence remains private
as predecessor evidence; no original raw observation or fixture byte changed.
