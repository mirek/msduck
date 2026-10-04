# UTF8 BulkLoad capacity reference

Task #940 retains 128 isolated NULL-plus-challenge loads per database. Four fresh databases in two SQL Server 17.0.4065.4 containers supply 512 observations. Matching source declarations and TYPE_INFO are VARCHAR(64)/bounded and VARCHAR(MAX)/PLP, with the captured native UTF8 collation. The frozen 14-vector matrix is explicit original bytes; no client decoder repairs inputs.

Each vector uses UTF8 VARCHAR/CHAR and Unicode NVARCHAR/NCHAR targets. Four selected vectors also use CP1251/CP1252 VARCHAR targets, giving 16 additional controls. Target widths are 1–4 native bytes or 1–2 UTF16 units, as recorded individually. Every load contains NULL followed by one challenge, with separate tables/connections; rejected loads retain their original counters and empty table readback.

The 128 outcomes agree across all four original runs: 62 successes, 42 errors 2628/state1/class16, 16 errors 7339/state1/class16 and eight errors 9833/state2/class16. Original descriptors, display strings, native bytes, SQL UTF16-unit binaries, callback/ERROR/INFO/DONE results and complete packets remain in the fixture. Digests independently pin 512 semantic outcomes and 2,560 request/response/framing phase signatures per category. Identity-aware digests check ephemeral names/SPIDs without rewriting the retained capture or its complete difference arrays.

| Challenge | Actual observation in both source forms |
|---|---|
| Empty → variable/fixed width2 | Variable is empty; CHAR stores `2020`; NCHAR stores `20002000`; NULL stays NULL. |
| `4141` → width2; `414141` → width2 | Exact fits; one extra nonspace raises 2628 and inserts neither row. |
| `C2A9` → UTF8 width1 / Unicode width1 | Native UTF8 raises 2628; Unicode stores `A900`. UTF8 width2 fits. |
| `F0908080` → UTF8 width4 / Unicode width2 | Stores original four bytes / `00D800DC`; width1 targets raise 2628. |
| `412020` → width1 | Accepts `41` (Unicode `4100`); excess ASCII spaces are discarded in these probes. |
| `4100` or `41C2A0` → width1 | Raises 2628; NUL and NBSP are not interchangeable with the preceding ASCII-space observation. |
| `C2` or `41C2` | Raises 7339; no row is stored. |
| `80` | Raises 9833/state2/class16; no row is stored. |
| `E08042` → width2 | UTF8 targets raise 2628; Unicode stores `FDFF4200`. Client replacement display is not a native-byte oracle. |

Selected CP1251/CP1252 outcomes and every original session counter are asserted individually in the replay test, including conversion-sensitive errors. No general mapping, malformed decoder or capacity policy is derived from these examples.

The retained fixture SHA256 is `f31fa57d130c9744a9246b625475bbb75830051ea651d7b250824a4bff7a0fc5` (5,892,974 bytes). The collector SHA256 is `fbf982f8caeaf88b2b20d2d6053836e73050a847a42621f1456ad042d1f01be3`. Reviewed ancestor/helper SHA256 values are embedded in provenance and checked before Docker; the script reuses task895's bounded exchange and the task899/919 observer design without editing them.

The first final-source four-run reproduction SHA256 is `13a9029aaebcb289965b719de598687ec3afd165e636ed5eb741d6a5a6b19dcc`. All semantic, original request/response and framing pins pass; all 8,183 differences against the retained acquisition remain in the full sidecar (SHA256 `d3ce4cd902de1d57e2cdd32d82ef728f74a8036046c92534ac1cccd19890267d`). Differences include actual container/database names, SPIDs and provenance. The independent second final-source four-run reproduction is also terminal0 (SHA256 `6281f69bb62009a7477132289c5140ecf77cb42d35632e17b8b34e595b5b0aaa`), with every fixed pin passing and all 9,209 raw differences against the retained acquisition preserved (sidecar SHA256 `6a31b661a20bdd2d6edffd40a45e3df4f2692bb002585e95ef1612c2574f96da`). Both reproductions use the identical collector SHA and await cleanup. Their artifacts remain separately in the claiming worker’s ignored staging; neither replaces the retained original evidence.

Run `node scripts/capture-bulk-character-utf8-capacity.mjs --replay-fixture` for offline validation and `node --test tests/bulk_character_utf8_capacity_capture.test.mjs` for the 16 focused guard/replay tests. Acquisition needs the cached pinned SQL image and reviewed Tedious20 dependencies; use a fresh private output path. The collector retains raw evidence before semantic validation, checks 48MiB/500,000-node/depth64 capture bounds and 2MiB/1,024-frame phase bounds, guards output aliases before Docker, and awaits container/database cleanup. Derived comparison exhaustion preserves the original bounded raw object and an explicit omission/failure sidecar. Tests exercise trace failure, exact-limit raw retention, uniform semantic/payload/frame corruption, contextual identity and aliases.

Coverage is finite: small target widths and the frozen source forms/collations only. This does not establish arbitrary lengths, source CHAR admission, CAST/assignment behavior, general padding or malformed conversion grammar, UTF8 capacity API support, or an engine/runtime endpoint. No Rust implementation, existing reference, shared helper or canonical build workspace changes accompany this capture.
