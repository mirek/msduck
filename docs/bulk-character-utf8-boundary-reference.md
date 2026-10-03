# Native UTF8 BulkLoad boundary reference

This finite SQL Server capture preserves native UTF8 input bytes, target native bytes, SQL UTF16 units and client display separately. It does not establish a general UTF8 decoder, best-fit converter, SQL assignment rule or runtime implementation.

The frozen matrix has 24 explicit original byte sequences, each sent to native CP1251, CP1252 and UTF8 VARCHAR(MAX), plus Unicode NVARCHAR(MAX). Each of the 96 probes contains NULL followed by exactly one isolated challenge; one malformed row cannot hide a later challenge.

| Input category | Original native bytes |
| --- | --- |
| Valid controls | `00`, `7f`, `c280`, `c2a9`, `dfbf`, `e0a080`, `ed9fbf`, `ee8080`, `efbfbf`, `f0908080`, `f48fbfbf`, `c2bf` |
| Truncated controls | `c2`, `df`, `e0a0`, `ed9f`, `f09080`, `f48fbf` |
| Malformed controls | `c228`, `e08080`, `eda080`, `f0808080`, `f4908080`, `f5808080` |

These category names classify the explicit test inputs; they are not a promise about SQL Server's decoder. Matching UTF8 VARCHAR(MAX) declaration/wire metadata and complete actual bytes are retained in every original BulkLoad request.

The collector reuses reviewed task895 exchange/path/budget guards and task899/906/909 retention envelopes, with immutable ancestor and helper hashes. All post-login frames, descriptor summaries, original SQL errors/info, completion/callback events and original-session counters remain raw. Exclusive output creation, alias refusal, bounded regular-file replay, raw-first partial evidence, asynchronous trace failure and comparison/envelope exhaustion are covered. Container/database identities are checked and contextualized only inside verification digests; complete original observations and differences remain unchanged.

Each full capture uses four fresh databases on two owned random-port SQL Server 17.0.4065.4 containers, tedious 20 and packet size 512. Login frames and credentials are excluded. Fixed full semantic/request/response/framing/provenance pins reject uniform corruption or payload-preserving packet repartition even with recomputed comparisons. The complete observations are independently pinned, and a final-source four-run reproduction passes those pins.

Uncaptured byte sequences, arbitrary stream fragmentation, codepages/collations, bounded column capacity, general assignment and runtime integration remain explicit gaps. The surrounding server must not substitute a strict or lossy platform decoder merely because these finite controls agree with it.

## Actual outcomes

Each complete run has 68 successful probes and 28 failures: six error 4896, eighteen error 7339 and four error 9833. Successful probes retain NULL followed by the exact challenge output and counters `[0,2,0,0]`. Failed probes have callback row count 0, empty readback (including the earlier NULL row), and original-session counters `[error_number,0,0,0]`; none needed a replacement readback session.

All twelve valid controls preserve original native bytes in the UTF8 target. Their Unicode target native bytes equal the SQL UTF16 projection, including supplementary pairs `00d800dc` for U+10000 and `ffdbffdf` for U+10FFFF. Both CP targets project those supplementary controls to two question-mark bytes `3f3f`, rather than one. For `c2bf`, CP1251 yields `3f` while CP1252 yields `bf`; neither mapping is inferred from another codepage.

All six truncated controls reject with error 7339/state 1/class 16 for CP1251, CP1252 and Unicode targets (`STREAM` invalid column data). Native UTF8 targets reject with error 4896/state 7/class 17 (`Invalid column value from bcp client for colid 2.`). Complete original diagnostics, callback errors and completion packets remain retained.

Five malformed controls succeed, with target-native identity and SQL/client decoding kept distinct:

| Original UTF8 bytes | UTF8 target native bytes | Client display | SQL Unicode projection | CP1251/CP1252 native bytes |
| --- | --- | --- | --- | --- |
| `c228` | `c228` | U+FFFD followed by `(` | `fdff2800` | `3f28` |
| `e08080` | `e08080` | three U+FFFD | `fdfffdff` (two U+FFFD) | `3f3f` |
| `eda080` | `eda080` | three U+FFFD | `fdfffdff` (two U+FFFD) | `3f3f` |
| `f0808080` | `f0808080` | four U+FFFD | `fdfffdfffdff` (three U+FFFD) | `3f3f3f` |
| `f4908080` | `f4908080` | four U+FFFD | `fdfffdfffdff` (three U+FFFD) | `3f3f3f` |

The Unicode target native bytes for these controls are exactly the SQL Unicode projection above; their public display follows those SQL units. A platform lossy decoder would hide the grouping difference if used as the oracle. The remaining malformed control `f5808080` rejects with error 9833/state 2/class 16 for all four targets, accompanied by info 3621/state 0/class 0 (`The statement has been terminated.`).

## Evidence and reproduction

The retained four-run fixture has SHA256 `fa1e3ae36794cfc4b197d2ff122c5ab5effb99f3b449d0ad2eafea354c18be6a` (4,014,177 bytes). Its original collector SHA256 is `de7587e0bbab15e2abd491f681958f7735aa859e8c7c36dc958c108757820fef`. This initial complete acquisition deliberately rejected its unpinned validation only after writing every original observation and comparison. Independently computed pins agree across all four runs, and retained offline replay now passes. A separate earlier startup timed out at the existing 2-second connection bound; its partial artifact is retained privately and is not counted as a complete capture.

The final collector SHA256 is `e85213c75f9c4a1cea93a5034e82d4b4d2481dae624d37b87f952dc384932c4a`. Its complete fresh four-run reproduction has SHA256 `b0718a8e05189a9758da6e133659bc5cc0981bdc84a83beebb4267177bf17b62` (4,024,340 bytes), with actual provenance matching that final source. All 96 cases/192 supplied rows per run and the complete semantic, request/response payload and packet-framing pins agree. The full sidecar preserves 5,646 raw differences and was independently recomputed and checked in full. Database/container identities, SPIDs and every raw difference remain unchanged; equal serialization size is not required.

```sh
node scripts/capture-bulk-character-utf8-boundary.mjs --replay-fixture
node scripts/capture-bulk-character-utf8-boundary.mjs .tmp/utf8-boundary-fresh.json
node --test tests/bulk_character_utf8_boundary_capture.test.mjs
```

All 16 focused tests passed on macOS Node 26.5.0 (20,518.235 ms) and Linux Node 24.13.0 (16,997.182 ms), with zero failures/skips/TODO/cancellations. Tests replay all 96 independent captured outcomes and their full native/SQL-unit/display rows, NULL and atomic failures, all fixed provenance/payload/framing pins, uniform malformed-result corruption, payload-preserving packet repartition, TYPE/SPID/status changes, output aliases, bounded replay and raw retention through asynchronous failures and comparison exhaustion. The retention negative uses 47 MiB of bounded raw data and requires complete raw preservation plus an explicit omitted-comparison failure sidecar.
