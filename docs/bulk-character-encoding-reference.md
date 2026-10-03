# BulkLoad character byte evidence

Task #884 records 33 controlled BulkLoad probes in four independent databases
across two containers of pinned SQL Server 17.0.4065.4. The fixture retains
all original supplied bytes, post-login packets, descriptors, client callbacks,
diagnostics, native binary projections and client text decoding. No login
packet or credential is retained. Nothing here changes runtime decoding.

The 16,001,979-byte fixture has SHA256
`0d2cff08bc7f59d271955311ac540f95a37f31b456bc008f739117c1a09d3a83`.
The actual capture source SHA256 is
`0c0b569fc7a2ad20181df8231a9a9ce2d4903eb5a72e17ec42d2582bc2f63e32`.
Later source changes strengthen offline validation only. The container image is
`mcr.microsoft.com/mssql/server:2025-latest@sha256:86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a`.

The source uses tedious 20.0.0 on Node24. It obtains each source collation's
actual descriptor from SQL Server, reconstructs tedious's five-byte Collation,
and supplies it to the VARCHAR BulkLoad column. A narrowly documented type
override accepts explicitly supplied Buffer values, avoiding client text
encoding. Ordinary tedious emits COLMETADATA, ROW, bounded values or PLP and
DONE; a controlled INSERT BULK declaration explicitly names the source
collation. Replay independently reconstructs and checks the complete actual
BulkLoad payload, including original bytes and NULL/empty framing.

The matrix covers CP1252, Cyrillic CP1251 and BIN2 UTF8 source collations,
same-collation VARCHAR, CP1252 VARCHAR and NVARCHAR targets, bounded/MAX,
NULL/empty/ASCII/non-ASCII, and MAX values spanning actual packets. The exact
wire collations are respectively `0904d00034`, `1904002200`, `0904002600`.
Finite captured conversion templates are validated independently of
between-run comparisons; they are not a general best-fit conversion map.

Observed examples include UTF8 éΩ🦆 preserved in a same-collation target,
converted to éO?? in a CP1252 target, and preserved as UTF16 in NVARCHAR.
CP1251 Привет remains native `cff0e8e2e5f2`, converts to six question marks
in CP1252 and to its original characters in NVARCHAR. Native CP1252 C1 bytes
`818d90` retain units 0081/008D/0090 through SQL conversion while tedious's
VARCHAR text decoder reports three U+FFFD characters. Both representations
are retained separately.

Malformed UTF8 probes are separate loads, each following a valid ASCII row:

| Supplied bytes | Observed outcome in all three targets |
| --- | --- |
| `80`, `c0af` | 9833/state2/class16 and 3621; no rows |
| `f09f92` | 7339/state1/class16, no 3621; no rows |
| `c328` | Accepted; same-collation native bytes unchanged, Unicode projection U+FFFD followed by `(` |
| `eda080` | Accepted; same-collation native bytes unchanged, public VARCHAR text three U+FFFD, SQL Unicode conversion two U+FFFD |

The exact errors, rows, completion callbacks and post-load transaction state
are in the fixture. These observations prohibit assuming every invalid UTF8
sequence either fails or takes the same replacement rule. They cover these
inputs only; other malformed sequences and encodings remain unprobed.

An earlier 18-case exploratory capture mixed invalid UTF8 bytes into every
UTF8 load. Those loads failed atomically with9833 and could not prove valid
UTF8 conversion. Its original raw artifact is preserved separately; the final
33-case capture separates valid and malformed controls instead of treating
those failed loads as successful encoding evidence.

Run `node scripts/capture-bulk-character-encoding.mjs --replay-fixture` and
`node --test tests/bulk_character_encoding_capture.test.mjs` offline. A fresh
capture takes one new diagnostic output path. It refuses existing paths,
aliases and regular-file ancestors before Docker. Raw output and its complete
comparison sidecar are saved before fixed-gold validation; a rejected variation
does not overwrite reviewed ground truth. The capture command cannot write
the retained fixture. Replay and retained-side comparison require its fixed
whole-file hash. Every dynamic database/server/header difference remains in
the raw comparisons; there is no byte or result normalization.

The current root BulkLoad adapter still decodes ANSI as CP1252 regardless of
wire collation. A separate implementation must preserve the native-byte/client
text distinction and these exact malformed-input outcomes. This reference does
not establish support for arbitrary collations, general lossy conversion,
all wire encodings or complete BulkLoad compatibility.
