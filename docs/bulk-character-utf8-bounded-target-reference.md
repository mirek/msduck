# UTF8 BulkLoad into bounded targets

This finite capture isolates target declaration form after the earlier
[source-form capture](bulk-character-utf8-source-form-reference.md) rejected
bounded sources into MAX targets before decoding. It preserves supplied/native
UTF8 bytes, SQL UTF16 units and client display separately. It does not implement
or establish a general decoder, arbitrary collation, capacity overflow policy,
SQL assignment rule or runtime integration.

The same 24 explicit vectors from the [boundary capture](bulk-character-utf8-boundary-reference.md)
are loaded under matching source VARCHAR(64)/bounded TYPE_INFO or
VARCHAR(MAX)/PLP TYPE_INFO, into native UTF8 VARCHAR(64) or Unicode NVARCHAR(64).
This gives 96 probes. Each contains NULL plus exactly one isolated challenge,
so an earlier malformed value cannot hide the intended result. Every vector
fits the target width. Target form is a declaration fact, not something to
infer from the current row length.

| Category | Original native bytes |
| --- | --- |
| Valid | `00`, `7f`, `c280`, `c2a9`, `dfbf`, `e0a080`, `ed9fbf`, `ee8080`, `efbfbf`, `f0908080`, `f48fbfbf`, `c2bf` |
| Truncated | `c2`, `df`, `e0a0`, `ed9f`, `f09080`, `f48fbf` |
| Malformed | `c228`, `e08080`, `eda080`, `f0808080`, `f4908080`, `f5808080` |

These names classify test inputs, not SQL Server's decoding behavior. The
collector retains exact declaration/TYPE_INFO/collation, post-login frames,
original SQL, typed descriptors, native readback, SQL units, public display,
errors/info/DONE/callback and original-session counters. Login/credentials
are excluded. Reviewed bounded exchange/retention helpers and all ancestor
source hashes are pinned before Docker starts. Exclusive outputs, aliases,
asynchronous failure, bounded replay and raw-first failure retention are tested.

## Actual outcomes

Every run has 68 successes, 24 failures7339/state1/class16 and 4
failures9833/state2/class16 with info3621/state0/class0. Both source forms
reach these outcomes with bounded targets; none rejects 4816 or 4896. Successful
loads contain NULL plus the challenge and counters `[0,2,0,0]`. Failed loads
have callback row count 0 and completely empty readback, including the earlier
NULL row. These are whole-load observations, not an implemented transaction rule.

All twelve valid vectors retain original bytes in native UTF8 targets. Unicode
native bytes match SQL UTF16 units, including supplementary pairs `00d800dc`
and `ffdbffdf` at U+10000/U+10FFFF. Five malformed vectors also succeed:

| Input/native UTF8 | Client display | SQL units / Unicode native bytes |
| --- | --- | --- |
| `c228` | U+FFFD then `(` | `fdff2800` |
| `e08080` | three U+FFFD | `fdfffdff` |
| `eda080` | three U+FFFD | `fdfffdff` |
| `f0808080` | four U+FFFD | `fdfffdfffdff` |
| `f4908080` | four U+FFFD | `fdfffdfffdff` |

Native UTF8 retains the input bytes in each row above. Unicode public display
follows the captured SQL units, rather than the original UTF8 client's display.
The remaining malformed vector `f5808080` rejects9833 for both source forms
and targets. All six truncated vectors reject 7339 for both source forms and
bounded targets. Native MAX targets in #913/#918 instead rejected 4896 for those
MAX-source vectors. A future decoder/admission plan needs explicit source and
target declaration forms; a strict/lossy platform codec is not the oracle.

Original counters are retained independently from error tokens. Here
`e0a0`, MAX source to Unicode NVARCHAR(64), reports error 7339 but `@@ERROR=0`
in all eight original/fresh runs. The same case in #913/#918 had 7339.
`f48fbf`, MAX source to Unicode, has 7339 here and in #913, while #918's MAX
Unicode target had 0. No universal error-to-counter rule is inferred.

The original #895 fixture has additional empty/ASCII rows before its challenge.
Its shared `eda080`, `f4908080` and `f5808080` controls agree on native bytes,
SQL units/display and diagnostic categories. Their different row IDs, shapes
and counts remain exact: successful two-row loads here versus four-row loads
there are not normalized into whole-capture equality.

## Retained evidence

Each acquisition uses four fresh databases on two pinned SQL Server 17.0.4065.4
containers, tedious 20.0.0, Node 24.13.0 and packet size 512. Fresh databases/server
identities are checked before verification digests contextualize those identities;
original observations and every raw difference remain untouched.

The retained fixture has 3,808,606 bytes and SHA256
`6c951a06d19c60c2a71e4656b570976baf726982cffcdef207d8e96bb5b92455`.
Its initial collector SHA256 is
`c7ce01b70e8ceaf69b2ab566d75fb9f89ab79f2367d87fb6c249b7cbcd7bb34c`.
This complete initial acquisition saved all 384 original loads and comparisons,
then deliberately rejected validation because its independent pins were not yet
established. It is not counted as a successful final verification.

The final collector SHA256 is
`7040f1ade58bc18083618184de28d3351cc282675631727c47e5576b618bd53b`.
Its complete fresh reproduction has 3,909,580 bytes and SHA256
`2a91c0e49a951cda945543e318b5fc5d6a61e9c8b4c84b1802cf8a37f6d7adfe`.
All 96 cases/192 supplied rows per run, 768 original requests and 12,288 fixed
semantic/request/response/framing pins across eight runs agree. The full fresh
sidecar contains 3,483 differences and was recomputed in full before validation.

Complete historical selected-record comparisons remain privately retained:
384 corresponding observations against #918 yield 7,007 differences;
192 MAX-source observations against #913 yield 3,330 differences;
24 shared bounded-source controls against #895 yield 756 differences.
These comparisons retain full original selected records, including names,
SQL, frames, descriptors, source/target facts, row shapes and counters.
Different targets and earlier distractor rows are documented, not erased.

```sh
node scripts/capture-bulk-character-utf8-bounded-target.mjs --replay-fixture
node scripts/capture-bulk-character-utf8-bounded-target.mjs .tmp/utf8-bounded-target-fresh.json
node --test tests/bulk_character_utf8_bounded_target_capture.test.mjs
```

All 18 focused tests passed on macOS (19,354.239 ms) and Linux (21,869.076 ms),
with zero failures/skips/TODO/cancellations. Tests cover all original/native/unit/
display/error outcomes and fixed pins; exact ancestor hashes and historical
counter/row-shape distinctions; uniform corruption and omitted probes; packet
repartition despite unchanged payload/recomputed comparisons; raw retention
through trace/aggregate/comparison failure; bounded files and output aliases.
The collector and fixture are reference evidence. Operational collation gates,
storage/BulkLoad consumers and general UTF8 conversion remain unfinished.
