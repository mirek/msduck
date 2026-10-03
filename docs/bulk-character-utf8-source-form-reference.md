# UTF8 bounded versus MAX BulkLoad source reference

This finite SQL Server capture compares the same 24 explicit native UTF8 vectors under matching VARCHAR(64) declaration/wire TYPE_INFO versus matching VARCHAR(MAX)/PLP declaration/wire TYPE_INFO. Both forms target native UTF8 VARCHAR(MAX) and Unicode NVARCHAR(MAX). Each of the 96 probes contains NULL followed by one isolated challenge, preserving exact source/target metadata and original supplied bytes.

The vectors are the owner-approved task913 inputs: valid controls `00`, `7f`, `c280`, `c2a9`, `dfbf`, `e0a080`, `ed9fbf`, `ee8080`, `efbfbf`, `f0908080`, `f48fbfbf`, `c2bf`; truncated controls `c2`, `df`, `e0a0`, `ed9f`, `f09080`, `f48fbf`; malformed controls `c228`, `e08080`, `eda080`, `f0808080`, `f4908080`, `f5808080`. Category labels describe the explicit bytes, not a universal SQL decoder contract.

Original post-login request/reply packets, supplied/native bytes, SQL UTF16 units, client display, descriptor summaries, errors/info/completions/callbacks and original-session counters remain raw. Independently pinned whole semantics, payloads, framing and source/helper provenance reject uniform corruption and payload-preserving packet repartition even if comparisons are recomputed. Checked database/container identities are contextualized only inside verification digests, without rewriting raw observations or differences.

Reviewed task895 exchange/path/budget guards and task899/906/909/913 retention envelopes are reused with immutable helper pins. Capture output is exclusive and alias-safe, bounded raw data is retained before validation, and trace/comparison failures preserve evidence. Each full acquisition uses four fresh databases on two owned random-port SQL Server 17.0.4065.4 containers, tedious 20 and packet size 512. Credentials/login frames are excluded.

The complete observations and final-source reproduction are independently pinned. Bounded target capacity, arbitrary codepages/byte sequences, general assignment/decoding and runtime integration remain explicit gaps. These tests do not authorize runtime claims or Rust/engine changes.

## Actual admission and value outcomes

All 48 bounded VARCHAR(64) source probes reject with error 4816/state 2/class 16 (`Invalid column type from bcp client for colid 2.`), including valid NUL/ASCII. The declaration and original wire width both remain 64; original ushort-length ROW framing and supplied bytes remain preserved. Callbacks have row count 0, readback is empty (including the earlier NULL row), and original counters are `[4816,0,0,0]`. This is source/target admission evidence, **not a bounded decoder experiment**. The matrix cannot establish how a bounded source decodes when targeting an accepted bounded destination. A separately approved bounded-target companion is needed; this capture does not substitute that experiment.

The 48 MAX/PLP source probes have 34 successes, six errors 4896, six errors 7339 and two errors 9833. All twelve valid vectors succeed for both targets, as do five malformed vectors. Native UTF8 targets preserve original malformed bytes, while client display and SQL Unicode grouping remain distinct: `e08080`/`eda080` display three U+FFFD but SQL units contain two; `f0808080`/`f4908080` display four U+FFFD but SQL units contain three. `c228` retains native `c228` and SQL units `fdff2800`. Unicode target native bytes follow the SQL units, not the client UTF8 display.

All six truncated MAX vectors reject with 4896/state 7/class 17 for UTF8 targets and 7339/state 1/class 16 for Unicode targets. MAX `f5808080` rejects with 9833/state 2/class 16 plus info 3621/state 0/class 0 for both targets. Successful counters are `[0,2,0,0]`; failed callbacks have row count 0 and empty readback. The full original diagnostics/completions and counters are retained without replacement-session substitution.

One counter differs from historical task913: `f48fbf` MAX-to-Unicode still reports 7339/state 1/class 16, but original counters here are `[0,0,0,0]`, whereas task913 had `[7339,0,0,0]`. Both current four-run acquisitions reproduce 0 exactly. Error diagnostics, native/load results and original-session `@@ERROR` are distinct observations; this does not establish a universal counter rule. The focused historical comparison asserts that exact difference and retains all other MAX native/unit/display/error outcomes.

A supplemental private full-field comparison aligns all four historical/current run pairs and their 48 common MAX-source UTF8/Unicode controls by original input/target, without rewriting any field. It retains all 2,465 raw differences, including the four counter leaves above, in the complete sidecar. Source-form case names, table SQL, database/container identities and SPIDs remain original. The historical cross-fixture drift is separate from the fixed within-current-capture pins.

## Evidence and reproduction

Retained four-run fixture SHA256: `7e78c4c663cd2531b205dd667d71e205b77923c36cbcbcc750a7d1d8300e997b` (3,932,929 bytes). Original collector SHA256: `ffebe9164cfd1f84b548f0db71ae706a521e82b0eb069b47f68ac29ebe95b240`. Its initial unpinned validation deliberately rejected only after all original observations and complete comparisons were retained; independently computed full semantic/request/response/framing pins agree across the four runs, and retained offline replay now passes.

Final collector SHA256: `a321c5461e262352a013d3b5a7952932e8f04eb1bc696aa1e5ef96a28067e30d`. A fresh complete four-run reproduction has SHA256 `8e20b9728c09c230329c93179cf7ba15dea6b1c22b379937f676ad0395af5a20` (3,892,055 bytes), with actual provenance matching the final source. All 96 cases/192 supplied rows per run and complete semantic, request/response payload and framing pins match. The full unnormalized sidecar has 2,747 raw differences, independently recomputed and compared in full. Unequal serialization size is retained honestly; no raw values, packet fields or differences are normalized away.

```sh
node scripts/capture-bulk-character-utf8-source-form.mjs --replay-fixture
node scripts/capture-bulk-character-utf8-source-form.mjs .tmp/utf8-source-form-fresh.json
node --test tests/bulk_character_utf8_source_form_capture.test.mjs
```

All 16 focused tests passed on macOS Node 26.5.0 (19,787.703 ms) and Linux Node 24.13.0 (21,355.675 ms), with zero failures/skips/TODO/cancellations. Tests cover every original input and captured source-form outcome, all native/unit/display rows, complete metadata/error/info/counter pins, actual counter drift from the SHA-pinned task913 fixture, uniform corruption, source/type changes, packet repartition, TYPE/SPID/status guards, output aliases, bounded replay and raw retention through asynchronous trace failure and comparison exhaustion. The 47 MiB overflow negative preserves every bounded raw run and requires an explicit failed comparison sidecar.
