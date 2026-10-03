# BulkLoad character conversion evidence

Task #895 extends the [raw encoding capture](bulk-character-encoding-reference.md)
with 69 probes in four fresh databases across two pinned SQL Server containers.
It supplies original bytes through tedious BulkLoad and retains SQL Server's
native bytes, SQL UTF16 units, client text, column descriptors, diagnostics,
completion callbacks, transaction counters and every post-login request/reply
frame. It neither changes runtime conversion nor proves arbitrary codepage
support. The [adapter plan](bulk-character-adapter-plan.md) describes the
remaining carrier, conversion, storage and output work.

The retained [fixture](../reference/bulk-character-conversion.json) is
9,092,471 bytes with SHA256
`f55527a2b0969d7104510d9b76a3303c82b28eac05d4ad11bc2acaf472caab27`.
Its actual capture source SHA256 is
`8422ef5d221fcad16e611fc7ce744b135b946359e94ff8a360075096c760377c`.
Subsequent source changes add validation without changing the probe matrix.
The reference reports SQL Server 17.0.4065.4, using image
`mcr.microsoft.com/mssql/server:2025-latest@sha256:86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a`,
tedious 20.0.0 and Node v24.13.0. The requested packet size is 512.

## Matrix and capture method

| Group | Probes | Scope |
| --- | ---: | --- |
| Every CP1251 byte | 3 | Each of 256 one-byte cells, NULL and empty, to CP1251/CP1252/Unicode |
| Malformed UTF8 controls | 48 | 16 sequences, each to UTF8/CP1252/Unicode, following NULL/empty/ASCII |
| Capacity, padding and MAX | 13 | Converted widths, CHAR/NCHAR, NULL/empty and fragmented MAX |
| Declaration versus wire descriptor | 5 | Three conflicts, all-zero descriptor, omitted descriptor |

Each CP1251 probe is one load of 258 rows, with one byte per cell; these are
not 256 independent transactions. Malformed controls are separate loads so
one failure cannot hide another sequence's behavior. Each probe uses a fresh
connection in its run's fresh database. All retained readbacks used the original
connection. The source records a replacement readback separately if reconnection
is necessary; replacement counters cannot establish the failed session's state.

A narrowly scoped tedious type override accepts explicit Buffer values rather
than encoding JavaScript strings. The source obtains actual SQL collation
metadata and supplies five bytes to TYPE_INFO, while INSERT BULK explicitly
declares its source collation. CP1252, CP1251 and UTF8 descriptors are respectively
`0904d00034`, `1904002200`, `0904002600`. The zero case supplies five zero bytes;
the omitted case removes those bytes from TYPE_INFO. These are distinct probes.

Replay reconstructs complete BulkLoad metadata, ROW values, PLP and DONE from
the fixed matrix and original input. Fixed digests also cover all original
outgoing payloads, complete incoming payloads and decoded semantic fields.
Packet lengths, types, IDs and EOM are checked independently. No login packet,
password or credential is retained. The copied
[tedious skill](../.agents/skills/tedious/SKILL.md) and
[TDS skill](../.agents/skills/tds-protocol/SKILL.md) inform capture structure;
their upstream implementation notes are not msduck support claims.

## Measured outcomes

Every CP1251 source byte is accepted in the three target probes. Same-encoding
native bytes remain identical. In particular byte `98` stays native `98` and
SQL Server converts it to UTF16 `9800` (U+0098), while tedious's public VARCHAR
decoder displays U+FFFD. A CP1252 target stores `3f` for that byte; a Unicode
target stores `9800`. Byte `00` converts to `0000`, and `ff` to `4f04` (я).
Client display must not replace SQL Server's native or Unicode evidence.

| UTF8 source sequence | Observed outcome in all three targets |
| --- | --- |
| `80`, `bf`, `c0af`, `c1bf`, `e228a1`, `f5808080` | 9833/state2/class16, INFO3621; no inserted rows |
| `e282`, `f09f92` | 7339/state1/class16, no INFO3621; no inserted rows |
| `c328`, `e080af`, `eda080`, `eda0bf`, `f08080af`, `f4908080`, `41c32842`, `41eda08042` | Accepted; exact target bytes and Unicode units retained |

Failed loads leave no earlier NULL/empty/ASCII row. Original-session counters
are `[error_number, 0, 0, 0]` for @@ERROR, @@ROWCOUNT, XACT_STATE and @@TRANCOUNT.
Accepted malformed values retain original bytes in the same UTF8 target.
For `e080af` and `eda080`, public text contains three U+FFFD characters but SQL
Unicode conversion contains two; `f08080af` and `f4908080` produce four versus
three. Neither a universal strict UTF8 decoder nor a universal replacement
decoder follows from these observations.

In the declaration/descriptor conflicts, the observed INSERT BULK source
declaration determines conversion: CP1251-declared bytes `cff0e8e2e5f2` read
as Привет even with a CP1252 wire descriptor; a CP1252 declaration with a CP1251
descriptor reads as Ïðèâåò. UTF8-declared `c328` with a CP1252 descriptor preserves
native `c328`. The zero descriptor accepts the ASCII controls. Omission fails
with 4002/state2/class16, including the exact unexpected-end-of-stream message.
This finite result does not establish acceptance of arbitrary descriptors.

Capacity behavior is also specific to the measured conversion path:

- CP1251 copyright byte `a9` to UTF8 VARCHAR(1) succeeds with empty bytes;
  VARCHAR(2) stores `c2a9`.
- UTF8 é to UTF8 VARCHAR(1) fails with 2628; VARCHAR(2) succeeds.
- UTF8 Ω or 🦆 to CP1252 VARCHAR(1) fails with 2628, even though diagnostic
  truncated text is respectively `O` or `?`.
- UTF8 🦆 to NCHAR(1) fails with 2628 and retains the diagnostic's lone high
  surrogate `\ud83e`; NCHAR(2) succeeds.
- An oversized UTF8 CHAR(1) source fails with 4815/state1/class17, before any
  earlier row becomes visible. CHAR(4) targets pad é to native `e9202020`
  (CP1252) or `c3a92020` (UTF8); empty becomes four spaces and NULL stays NULL.

These bulk observations are not generalized to ordinary CAST or assignment.
The fixture retains full diagnostic text, state/class, descriptors and results.

The UTF8 MAX load uses 401 ASCII bytes followed by 3,000 duck characters. In
every run, its second outgoing BulkLoad packet ends in `f0`, and the next
payload begins `9fa686`: the request splits inside a UTF8 character. The
second readback packet also ends in `f0`; the next starts with PLP chunk length
`f4010000`, then `9fa686`. CP1251 Привет converted to UTF8 similarly splits
`d0` from `b5` across response PLP chunks. Focused tests check these actual
boundaries and complete unchanged native values, rather than assuming a split
from value length alone.

## Reproduction, differences and bounds

Run offline:

```sh
node scripts/capture-bulk-character-conversion.mjs --replay-fixture
node --test tests/bulk_character_conversion_capture.test.mjs
```

For a new container-backed capture, supply a new output path:

```sh
node scripts/capture-bulk-character-conversion.mjs artifacts/compatibility/bulk-character-conversion/new.json
```

The source refuses existing paths, hardlinks to existing outputs, symlink
ancestors and regular-file ancestors before Docker. It cannot overwrite the
retained fixture. Capture files are bounded before reading their payload;
growth during the bounded read is rejected. Each exchange is capped at 2 MiB
and 1,024 frames; each frame at 32,767 bytes; each case at 260 rows; the matrix
at 96 cases; capture and complete difference output at 48 MiB. JSON preflight
bounds depth/nodes and computes escaped UTF8 size before serialization. Raw
packets are charged before copying, and complete differences before retention.
The retained capture's largest exchange is 151,215 bytes and 296 frames.

Raw output and its full unnormalized comparison sidecar are written before
fixed-gold validation. Failed or changed observations remain available for
review. Uniform corruption is rejected even if all four runs and their
comparison summaries are changed together. Tests also cover packet corruption,
session labels, resource bounds and output aliasing.

Raw cross-run comparison counts are 14, 997 and 997; the differences retain
original database/container names, diagnostics, response bytes and packet
headers. Verification digests use a separate, documented projection: version
identity fields are checked against their recorded database/server; serverName
diagnostic fields must match that server; 2628's database-qualified table prefix
must match the actual database. Only those ephemeral identities are omitted
or replaced in digest inputs. Incoming payload digests substitute their exact
UTF16 database/server byte sequences. Original capture bytes and comparison
values are never normalized, rewritten or discarded. Packet header SPIDs are
retained and structurally checked but excluded from payload digests.

The initial complete captures exited after saving evidence because fixed gold
had intentionally not yet been pinned. The retained second capture was inspected
across all four runs before semantic and payload expectations were installed;
its subsequent offline replay passes. The earlier exploratory source SHA256
`c13f3608516d40bd3ec53bd9d3c129436e4fedae503e323c48b5921f1e3c0d7e`
recorded only two version columns and is preserved separately, not substituted
for retained evidence.

A fresh capture with the final validation source
`6e63054a9948f52b315249ddb59503fb390ebbd54a94c74f3dc09f5c41bfa585`
completed with exit zero. Its 10,357,418-byte raw artifact has SHA256
`ba4c8c4320608c94587e4afd7c266250bd1254149d2da7462c3071bbd8ac0373`;
its complete 6,659,908-byte comparison sidecar contains 5,331 differences
against the retained fixture. All semantic and payload projections match the
independent pins; original ephemeral identities and packet headers remain
different in raw evidence. Both local and Linux focused suites pass ten tests.

Legacy TEXT, arbitrary collation/codepage profiles, a general best-fit map,
Unicode-source isolated surrogates and combinations of large values with all
staging/explicit-transaction scenarios remain outside this finite matrix.
Root BulkLoad still needs native ANSI storage and correct conversion/output
integration. These reference tests do not establish complete BulkLoad or
SQL Server compatibility.
