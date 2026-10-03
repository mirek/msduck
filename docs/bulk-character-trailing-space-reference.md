# BulkLoad trailing-space reference

This task captures native trailing U+0020 bytes through SQL Server BulkLoad, over explicit source declarations and bounded target columns. It establishes neither general CAST/assignment rules nor runtime support.

The matrix contains 96 probes, each with NULL followed by one isolated challenge: 72 ASCII `A` plus two spaces, 18 copyright plus two spaces, and six CHAR(8) source-padding controls. Sources use native CP1251, CP1252 or UTF8 bytes with matching declared and wire widths. ASCII sources are VARCHAR(MAX) or CHAR(8); copyright bounded-target sources are VARCHAR(MAX). Targets include CP1252/UTF8 VARCHAR or CHAR and Unicode NVARCHAR or NCHAR at widths 1/2; padding controls target same-native VARCHAR(MAX) or Unicode NVARCHAR(MAX).

All original post-login request/reply packets, decoded descriptors, callbacks, errors, completion events, source bytes, target native bytes, SQL UTF16 units, original-session counters and cross-run differences remain retained. Independently pinned whole semantic/payload/framing/provenance digests reject uniform corruption even when comparisons are recomputed. Only checked container/database identities are contextualized inside verification digests; raw observations remain unchanged.

The collector retains reviewed task895 exchange/output/budget primitives and task899/906 retention guards, with immutable helper hashes. Capture and sidecar creation are exclusive, raw evidence precedes validation, and failures preserve partial evidence. Each complete capture uses four fresh databases on two owned random-port SQL Server 17.0.4065.4 containers and tedious 20 with packet size 512. Credentials and login frames are excluded.

NUL/NBSP, arbitrary widths, other code pages/collations, conversion capacity rules outside the actual matrix and server integration remain unverified. Capture hashes, exact outcomes and reproduction evidence will be added after the full observations are pinned.

## Observed outcomes

All 72 ASCII probes succeed with callback count 2 and original-session counters `[0,2,0,0]`. Width 1 retains `A`; width 2 retains `A `, across VARCHAR(MAX)/CHAR(8) inputs and all six target families. NULL remains NULL. Unicode target native bytes are `4100` or `41002000`; ANSI/UTF8 native bytes are `41` or `4120`.

For native CP1251/CP1252 copyright `a92020`, CP1252 VARCHAR(1)/(2) retains `a9`/`a920`. UTF8 VARCHAR(1) succeeds with empty native bytes; UTF8 CHAR(1) succeeds with native `20` and SQL U+0020. Both UTF8 width-2 targets retain complete `c2a9`. Thus original source capacity and converted UTF8 capacity have different visible outcomes in this finite matrix; no dangling `c2` byte is retained.

For native UTF8 copyright `c2a92020`, the three width-1 targets each reject with error 2628/state 1/class 16, callback row count 0, counters `[2628,0,0,0]` and empty readback, including the earlier NULL row. Width-2 targets succeed: CP1252 VARCHAR(2) retains `a920`, and UTF8 VARCHAR/CHAR(2) retains `c2a9`. The shorter converted CP1252 payload does not override the captured original-source boundary.

All six CHAR(8)-to-MAX controls reject with error 4816/state 2/class 16 and callback count 0; counters are `[4816,0,0,0]` and readback is empty. These controls **do not establish source padding**. The original supplied bytes are copyright plus exactly two spaces, with matching CHAR(8) declaration/wire width. A separate private four-run pilot supplying exactly eight bytes also returned 4816 for all six MAX combinations; that pilot is supplemental, not substituted into this fixture. Other widths and successful fixed-source padding need separate evidence.

## Evidence and reproduction

Retained fixture SHA256: `d9743a80462871dbed0f2830e984c71e621afb2d704d6a541271ae0f3c305740` (3,979,079 bytes). Its actual collector SHA256 is `aae4b8e80744861dc0ff865af7fd54966104199840b40236d5be9e4292a7ff1a`; the collector with independent pins has SHA256 `805bebbe2a22373e1745dc17786d1763cd295097e24aa2be83a5cfb8ce5211aa`. Each run has 96 cases/192 supplied rows, 87 successful cases, three 2628 failures and six 4816 failures. All whole semantic and five-phase payload/framing pins agree across all four runs. The first acquisition deliberately rejected its unpinned validation only after retaining all original observations; offline replay now validates the pinned fixture.

```sh
node scripts/capture-bulk-character-trailing-space.mjs --replay-fixture
node scripts/capture-bulk-character-trailing-space.mjs .tmp/trailing-space-fresh.json
node --test tests/bulk_character_trailing_space_capture.test.mjs
```

The 17 focused tests cover every matrix outcome, NULL and atomic failure, source/native/SQL-unit differences, complete fixed pins, uniform corruption, packet repartition, TYPE/SPID/status changes, alias/path guards and bounded raw retention. The derived-overflow negative uses 47 MiB of bounded raw input and preserves raw observations even when the complete comparison cannot fit. macOS Node 26.5.0 passed all 17 tests with zero failures/skips/TODO/cancellations; fresh Linux tests and the complete source-pinned four-run reproduction are pending.
