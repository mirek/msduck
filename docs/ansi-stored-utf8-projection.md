# Stored UTF8 projection investigation

Task #933 is in progress. `ansi_conversion::stored_utf8::to_sql_utf16` now implements a deterministic stored-value projector, with explicit source identity, NULL and resource limits. It checks original input limits before EOF fitting, counts exact UTF16 output bytes before fallible allocation, and reports boundary failure separately from declaration/resource errors. The existing strict valid-UTF8 API and all CP1251/CP1252 projections remain unchanged. Full workspace/client/audit verification and final reviews are still required before merge.

## Source and operation

The collector creates each fresh database with `Latin1_General_100_BIN2_UTF8` as its default collation and records that property with SQL Server version/database/server identity. Converting varbinary to varchar in a CP1252-default database and then applying UTF8 collation transcodes CP1252 characters: that discarded setup is preserved separately and is not an oracle for stored UTF8 byte identity.

Each probe declares a VARCHAR(MAX) variable from the original binary input. It then issues **separate** native binary and Unicode-conversion SELECTs. This preserves native identity before a Unicode error instead of hiding both fields behind one failed result row. Original errors, descriptors, done tokens and complete packets are retained.

The frozen 1522 inputs include NULL, empty, all256 octets, all400 previous multibyte anchors without suffix, explicit incomplete/range-forbidden prefixes and every unique admitted variable-storage UTF8 payload in seven previous first-party fixtures. The largest input is12401bytes. Inputs and all reviewed helpers/fixtures are pinned before Docker; guards remain48MiB/500000nodes/depth64/2MiB-phase/1024frames/connect2seconds.

## Original findings

Four fresh databases in two pinned SQL Server17.0.4065.4 containers each produce153 projection failures (error9833,state2,class16) and1369 successful projections, including NULL. Native readback preserves every supplied input, including all failing projections. A failed Unicode result has its typed metadata and no fabricated row.

For these MAX variables, incomplete inputs C2, E0A0, E080, F09080 and F49080 project to an empty non-NULL Unicode byte sequence. Standalone80/BF, C0AF and F5808080 raise9833. Adding suffix42 to80 instead yields `FDFF4200`. NULL and empty retain their distinct meanings and VarBinary(MAX) descriptors.

The earlier repair hypothesis matched4912 admitted non-NULL BulkLoad-derived rows, but the direct variable probes add70 successful disagreements and153 failures per database. Those raw differences invalidate using that hypothesis as a general SQL stored-value decoder. Source-form, tail handling and projection errors must be modeled from evidence; the client decoder is not the oracle.

The retained separated-projection artifact is16032371bytes, SHA256 `04b4b49603116046ac475d31a244207aa61054a672aacaffa976a106800961e9`. Initial source SHA256 `b69e7d39a9f3dd5f56458c5c76da848b66422b4052dfe9b3e07489fb8941fd1a`; frozen source SHA256 `98643cbf943d857ef5e5eb98292b19908da324e68564331128b43833c711dca7`. All four original runs agree on complete result/request/response/framing pins. The final-source reproduction completed all6088queries and awaited cleanup, exited0, and matched the fixed pins. Its raw artifact is17149937bytes, SHA256 `483098e49ec0f2ff063a6a0125c4d65ad9b22b59c359b4b7c17395e4977348e9`; the complete10249-difference sidecar was independently reconstructed without altering raw data. Three Rust reference-spec tests pass in the existing deterministic cache. Those tests preserve the observations; they do not constitute implementation or workspace/client/audit completion.

## Remaining work

### Implemented boundary rules and reproducible controls

The implementation separates a bounded backward nominal-byte-span search from the forward scalar/replacement decoder. The search does not assume that intervening bytes are valid continuations. An incomplete potential variation selector can affect its preceding base; a completed selector protects an existing pair from a second removal. The measured selector ranges are U+180B..U+180D, U+FE00..U+FE0F and U+E0100..U+E01EF. U+180F, U+FE10 and U+E01F0 controls remain outside that behavior. SQL's nominal supplementary-selector recognition masks the final payload bits even for an invalid continuation byte; the repair pass still emits replacements for the original bytes. BMP selector controls do not admit that masking shortcut.

Thirty-three public EOF/selector controls and their complete decoded/request/response/framing pins are frozen in the collector. Reproduce them on the configured SQL Server container host with:

```sh
node scripts/capture-stored-utf8-projection.mjs --eof-controls .tmp/stored-eof.json
node scripts/capture-stored-utf8-projection.mjs --replay-eof-controls .tmp/stored-eof.json
```

This mode uses four fresh databases in two pinned containers, preserves original packets and raw differences, and retains the existing bounds. It does not compare the new cases against the unrelated 1522-input fixture. The original mode and `--replay-fixture` remain available. The first EOF capture is344376bytes, SHA256 `d5ab8d81cc5700482c6e6250161ff221b9e278adadea32ed067a21f7763d0c80`; the frozen-source reproduction is364618bytes, SHA256 `3f0551f6a24a0f3437bf9ce6085de5fc2d873b918c24bbe00ab698e46a81531e`. Both complete132queries, agree on every frozen pin and await cleanup. The complete359 raw differences between those captures are separately preserved.

The current collector source SHA256 is `e9713f72ceb80509228de085644a5a42636bf267ac889656c5deccdf35e538d7`. Its original 1522-input mode also completed all6088queries, awaited cleanup and exited0. That fresh raw artifact is15859531bytes, SHA256 `5026ef455d36cb1fd197fca58dacc56d6a69fe68abcc2eb72271cefe2159da73`; its complete12937-difference sidecar against the original fixture was independently reconstructed without changing either raw capture.

Independent review identified a retention bug in derived inter-run comparisons. Finalization now checks their combined envelope before attaching them; overflow persists the original bounded observations and an explicit omission/failure sidecar, then reports failure. Capture failures likewise live in the sidecar. The regression uses two different 20 MiB values: the raw observations fit the unchanged 48 MiB limit while their derived comparisons do not. It verifies exact raw retention and the labelled sidecar, then removes only its synthetic outputs:

```sh
node scripts/capture-stored-utf8-projection.mjs --test-retention .tmp/stored-retention-test.json
```

Further class, seeded-random, selector-endpoint and malformed/mixed-family captures challenged successive candidates instead of being normalized to fit them. The actual Rust API was independently replayed against20834 original and fresh observations with zero unit/error mismatches. Nine focused Rust tests cover the6088 original outcomes, all33 public EOF controls, malformed repairs versus the strict API, supplementary pairs, NULL/identity and exact resource limits. Focused strict Clippy and formatting checks pass. These checks do not replace the remaining full verification gates.

### Further source-form and EOF controls

Four additional private diagnostic captures completed normally with awaited container cleanup: 40 source-form observations, 68 minimal tail observations, 176 prefix/tail observations and 512 observations covering every terminal octet. Each compares `DECLARE @v varchar(64)` with `varchar(max)`, followed by separate native bytes, `LEN`/`DATALENGTH`, and binary Unicode projection SELECTs. These are diagnostic evidence, not additions to the independently reproduced four-run reference or a completed decoder rule. MAX length values retain their BigInt string representation; bounded values retain their Int representation.

For `1054F55C7B96`, bounded construction retains `1054F55C`; MAX retains every input byte, yet both Unicode projections produce `10005400FDFF5C00`. `C899AC` becomes empty during bounded construction and during MAX Unicode projection, despite `C899` alone being the complete scalar U+0219. `CA9481D4` fails with9833 during bounded construction and during MAX projection. Appending `42` changes these EOF outcomes. A rule that only removes an incomplete final scalar cannot explain these observations.

With four ASCII `41` bytes followed by each possible terminal octet, bounded native readback has the following exact groups. MAX native readback preserves all256 inputs and its Unicode projection agrees with the bounded result. All512 observations succeed without SQL errors.

| Terminal octet | Bounded native result |
| --- | --- |
| `00..7F` | All five supplied bytes |
| `C2..DF`, `E0`, `E2..EE`, `F0..F2`, `F4` | Four `41` bytes |
| `80..C1`, `E1`, `EF`, `F3`, `F5..FF` | Three `41` bytes |

The unusual `E1`, `EF`, and `F3` outcomes are measured, not inferred from a Unicode lead-byte range. They require further context tests before incorporation into a general boundary algorithm. Ordinary repair remains separately necessary after determining the retained prefix; EOF fitting cannot be replaced by the client decoder.

The complete private raw artifacts are retained without normalization: source-form SHA256 `c19b76b992f9c24577d0bb36e5481d31a01f7a38038f2b4d24826f0736edeb0a`, minimal-tail `5ea2287f2dbc9a326d69983ace52eb973f1ca658641336311b9a46922748b8ce`, prefix/tail `8938c5be922548d9671b1f4861a5ec637289ff4210fd518ac7ce57905ee39e00`, and terminal-octet `747d5b5836f0d5094a2bd93bb050e06963b9d3f211970a50ae2cd760c76aceef`. Each includes its exact collector source hash and SQL text. These probes refute additional candidate algorithms; they do not replace the full implementation and verification requirements below.

An additional72-observation capture checks complete sequences beginning with the three unusual terminal bytes. `E18080`, `E1A080`, `EF8080`, `EFBFBF`, `F3808080` and `F3BFBFBF` survive complete EOF and an appended ASCII `41`. Appending `E1`, `EF` or `F3` instead removes the whole sequence in these controls. Appending `80` removes the complete three-byte sequences but raises9833 after the four-byte sequences. Its raw SHA256 is `52b40360a59e49f5da9ca999bfad69940ec32eb505e03e3e7d572441cf1f9792`.

Finally,40 controls assign the binary literal to a `varbinary(max)` variable before converting it to varchar. Their complete result sets, typed rows and errors match the original source-form controls. The additional DECLARE produces an extra completion token, so all40 complete result objects differ and remain preserved; this is not a full-result equality claim. This diagnostic's raw SHA256 is `fb6641fc5ebe1c5dc20cbf36531915550711ed1e6b52eba57e5ffd79f35a4d72`. It does not establish equality for every possible parameter, table or expression source.

Complete full exact-head Rust/client/audit verification, independent/Codex review, required CI, merge and registry/project completion. Preserve any new disagreements as original counterexamples. This checkpoint is not runtime support, admission or capacity completion. Engine/Value/TDS integration remains under the separately coordinated adapter plan.
