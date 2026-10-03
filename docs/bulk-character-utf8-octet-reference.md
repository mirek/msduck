# UTF8 octet and lead/next-byte controls

This finite reference capture extends the [context matrix](bulk-character-utf8-context-reference.md)
with every possible single octet and explicit lead/next-byte boundary controls.
It records native UTF8 bytes and direct SQL UTF16 projection together, preserving
client display separately. It does not establish a total malformed decoder,
all third/fourth-byte transitions, declaration profiles, codepage mappings,
capacity/assignment semantics or runtime integration.

The matrix has 416 isolated NULL/challenge loads per database:

- All 256 octets as `byte + 42`.
- Sixteen leads `c0`, `c1`, `c2`, `df`, `e0`, `e1`, `ec`, `ed`, `ee`, `ef`,
  `f0`, `f1`, `f3`, `f4`, `f5`, `ff`, each followed by ten next-byte controls
  `00`, `7f`, `80`, `8f`, `90`, `9f`, `a0`, `bf`, `c0`, `ff`, then `42`.

Every source declaration and TYPE_INFO is UTF8 VARCHAR(64); every target is
native UTF8 VARCHAR(64). Inputs fit the widths and have at most three bytes.
The ASCII suffix follows prior context observations; it does not predeclare
new inputs successful or classify their decoding. Categories label the frozen
byte grid rather than presumed valid/malformed outcomes. No platform strict or
lossy decoder supplies the expected SQL projection.

## Actual outcomes

All 416 loads succeed in each retained database. Every load preserves the NULL
and challenge rows, callback count2 and original counters `[0,2,0,0]`, with no
errors or info messages. Native UTF8 readback preserves all supplied bytes.
SQL UTF16 projection and the native client's display remain independent facts.

For the 256 single-octet controls, `00` through `7f` project to their original
ASCII unit followed by `4200`; `80` through `ff` project to `fdff4200`.
The complete original byte remains in native storage, including NUL and invalid
UTF8 leads. This statement applies to these suffixed inputs, not isolated octets
which earlier captures sometimes rejected before projection.

Among the pair controls, `c2` or `df` followed by the six sampled continuation
boundaries (`80`, `8f`, `90`, `9f`, `a0`, `bf`) produces the observed valid scalar
unit followed by `4200`: for example `c28042` gives `80004200`, and `dfbf42`
gives `ff074200`. The sampled `e0`..`ef` and `f0`..`f4` leads followed by those
same continuation controls produce `fdff4200`, even when the second byte is
forbidden for a complete scalar. For example `e08042` has native client display
two replacements then `B`, but SQL units one replacement then `B`. A platform
lossy decoder does not supply that SQL grouping.

The sampled illegal leads `c0`, `c1`, `f5`, `ff` followed by a continuation control
instead produce `fdfffdff4200`. Every sampled lead followed by `00` or `7f`
retains that ASCII unit after a replacement; followed by `c0` or `ff`, it gives
two replacements then `B`. Tests retain every actual complete row and descriptor
rather than deriving these outputs from these summaries. Third/fourth-byte
transitions and a total malformed decoder still require further implementation
and grounding.

## Evidence and verification

Each acquisition uses four fresh databases on two pinned SQLServer17.0.4065.4
containers, tedious20.0.0, Node24.13.0 and packet size512. Exact source/target
facts, declarations, TYPE_INFO/ROW/requestSQL, descriptors, native bytes,
SQL units, display, errors/info/DONE/callback, original-session counters and
post-login framing are retained. Credentials are excluded. The eleven reviewed
ancestor/helper source hashes are verified before Docker starts.

The inherited guards remain 48MiB per capture, 500,000 JSON nodes, depth64,
2MiB/1024 frames per exchange, callback15s and connection2s. Exclusive raw
outputs and complete comparison sidecars precede gold validation; partial
failures remain separate and cleanup is awaited. No guard is relaxed to
obtain an acquisition. Independent pins cover full semantics, reconstructed
original requests, response payloads and complete packet framing.

The retained acquisition has 15,755,701 bytes and SHA256
`41d1e69bc51a5624b46f5970c82dd985a1a631089a5a8041e4994435d4a46d8d`.
Its initial collector SHA256 is
`6154180160506c05dadf5096b0de9c3c1d514c4f069b431bce32152d4e3d4d26`.
This initial acquisition completed all 1,664 loads, saved raw data/comparisons,
then intentionally rejected validation because independent gold pins were not
yet established. It is not a passing final verification.

The frozen collector SHA256 is
`64070f28616c759978e91efbb1f98a7cb0482f49f60f093d1dc44e1b72f2be69`.
Its fresh four-database reproduction is running; it is not yet counted as a
successful verification. Final raw hashes and complete comparison evidence will
be recorded after that process terminates and its outputs are independently
checked.

All 16 focused tests pass on macOS (45,314.456875 ms) and Linux (43,332.38962 ms),
with zero failures/skips/TODO/cancellations.

```sh
node scripts/capture-bulk-character-utf8-octet.mjs --replay-fixture
node scripts/capture-bulk-character-utf8-octet.mjs .tmp/utf8-octet-fresh.json
node --test tests/bulk_character_utf8_octet_capture.test.mjs
```

Tests cover every original byte-grid/native/unit/display/error/counter outcome,
fixed provenance and source/target shapes; uniform corruption and omission;
packet repartition despite unchanged payload/recomputed comparisons; aliases,
bounded reads and raw evidence retained through asynchronous trace, aggregate
and comparison failure. Prior overlapping context observations are kept exact;
changed inputs are not normalized into isolated-anchor equality. General UTF8
conversion and operational storage/BulkLoad/output support remain unfinished.
