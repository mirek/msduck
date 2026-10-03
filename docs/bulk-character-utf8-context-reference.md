# UTF8 BulkLoad with adjacent ASCII bytes

This finite reference capture extends the isolated UTF8 controls in
[conversion](bulk-character-conversion-reference.md),
[boundary](bulk-character-utf8-boundary-reference.md) and
[bounded-target](bulk-character-utf8-bounded-target-reference.md) captures.
SQL Server admission changes when ordinary bytes surround a malformed anchor.
These observations do not establish a general decoder or implement runtime
conversion, arbitrary collation, capacity overflow or SQL assignment semantics.

The frozen matrix has 16 anchors, three contexts and six declaration profiles,
for 288 probes per database. Each load has NULL followed by exactly one challenge.
All payloads fit the declared widths, with at most six bytes.

| Anchor group | Native anchor bytes |
| --- | --- |
| Illegal lead | `80`, `bf`, `c0af`, `c1bf`, `f5808080` |
| Malformed | `c228`, `e228a1`, `e08080`, `eda080`, `f02880a1`, `f0808080`, `f4908080` |
| Truncated | `c2`, `e0a0`, `f09080`, `f48fbf` |

These categories describe the original anchors, not the transformed input or
SQL Server's decoder. Contexts are `41 + anchor` (prefix), `anchor + 42` (suffix)
and `41 + anchor + 42` (sandwich). The six profiles are bounded VARCHAR(64)
source to native UTF8 VARCHAR(64) or Unicode NVARCHAR(64), and MAX/PLP source
to either of those bounded targets or their corresponding MAX targets.
Source declarations and TYPE_INFO match. Bounded-source to MAX-target probes
are excluded because the earlier source-form capture rejected those with 4816
before a useful decoder comparison.

## Actual observations

Every database has 222 successful loads, 40 failures7339/state1/class16,
8 failures4896/state7/class17 and 18 failures9833/state2/class16.
All suffix and sandwich inputs succeed across all six profiles. Prefix outcomes
are:

| Prefix anchors | Outcome |
| --- | --- |
| `80`, `bf`, `c0af`, `c1bf`, `c2`, `e0a0`, `f09080`, `f48fbf` | 7339 except MAX source to native UTF8 MAX, which reports 4896 |
| `f5808080`, `e228a1`, `f02880a1` | 9833 with info3621/state0/class0 in all six profiles |
| `c228`, `e08080`, `eda080`, `f0808080`, `f4908080` | Success in all six profiles |

For example, isolated `80` failed9833 in #895. `4180` instead fails7339
into bounded targets, while `8042` and `418042` succeed. Isolated and prefixed
`f5808080` fail9833; `f580808042` succeeds. These are actual admission
observations, not proof of a universal tail-check algorithm.

Successful native UTF8 targets preserve every supplied byte, including malformed
bytes. Their client display and SQL UTF16 projection remain separately retained.
For example, `e0808042` displays three replacement characters then `B` through
native UTF8 readback, but its SQL units are `fdfffdff4200`: two replacement
characters then `B`. `f580808042` produces four SQL replacement units then `B`.
Truncated anchors followed by `42`, such as `e0a042`, produce one replacement
unit then `B`. Unicode target native bytes equal the observed SQL UTF16 units.
The fixture and tests preserve every complete row rather than deriving these
results with a platform codec.

Successful loads retain the NULL and challenge rows, callback count2 and
original-session counters `[0,2,0,0]`. Failures have callback count0 and empty
readback, including the earlier NULL row; counters are `[error,0,0,0]` in every
retained run. Earlier captures' counter differences remain valid evidence;
this matrix does not establish a universal token-to-counter rule.

## Verification evidence

Each acquisition uses four fresh databases on two pinned SQL Server17.0.4065.4
containers, tedious20.0.0, Node24.13.0 and packet size512. Original SQL,
declarations, TYPE_INFO/ROW/PLP, typed descriptors, native bytes, SQL units,
client display, errors/info/DONE/callback and counters are retained. Post-login
framing is preserved; credentials are excluded. Reviewed bounded helper hashes
and all ten ancestor source hashes are checked before starting Docker.

An initial attempt stopped on a two-second connection timeout after 577 complete
loads. Its partial raw capture and sidecar were saved privately, and container
cleanup completed before retry. No request timeout was relaxed. The unchanged
collector's retry completed all 1,152 loads, saved all raw records and comparisons,
then intentionally rejected validation because independent gold pins were not
established yet. That acquisition is not a final passing verification.

The retained fixture has 12,270,016 bytes and SHA256
`cd27cbeb9549b1109eaa85fc31417b234dad1140a6f830d65ba9ba9daafbf0d0`.
Its initial collector SHA256 is
`dc841b40e53774e5ed6353a8b2cc638c3714d7b067eaa5031d923b260a15d2f9`.
Independent pinning checks all four runs' complete semantics, reconstructed
original requests, response payloads and packet framing before freezing gold.

The frozen collector SHA256 is
`f7790addf553959fa72f514934b54521ccb788c3d2f27b75705b1e77e93fbb7e`.
Its complete fresh reproduction has 11,598,346 bytes and SHA256
`8b8160295953128d682071383aef666500854b87c310d6f3903b3d4d7297249d`.
Both acquisitions agree on 2,304 original requests and 36,864 fixed
semantic/request/response/framing pins across eight runs. The full fresh
comparison sidecar has 4,683 differences and was independently recomputed
without normalizing original records or omitting any difference.

All 16 focused tests pass on macOS (38,210.414417 ms, Node26.5.0) and Linux
(32,913.911522 ms, Node24.13.0), with zero failures/skips/TODO/cancellations.
Both SQL acquisitions ran under the pinned Linux Node24.13.0 provenance.

```sh
node scripts/capture-bulk-character-utf8-context.mjs --replay-fixture
node scripts/capture-bulk-character-utf8-context.mjs .tmp/utf8-context-fresh.json
node --test tests/bulk_character_utf8_context_capture.test.mjs
```

The 16 focused tests cover every original/native/unit/display/error/counter
outcome and source/context profile; fixed provenance, omitted probes and uniform
corruption; packet repartition despite identical payload and recomputed
comparisons; bounded reads, aliases and raw retention through asynchronous
trace, aggregate and comparison failures. Historical isolated anchors are
checked against the immutable #895 fixture without erasing their different row
shapes or imposing their errors on changed inputs. Operational UTF8 collation
gates and storage/BulkLoad/output integration remain unfinished.
