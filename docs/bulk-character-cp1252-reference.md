# Native CP1252 BulkLoad conversion reference

This reference measures SQL Server's conversion of native CP1252 bytes into CP1252, CP1251, UTF8 and SQL Unicode MAX columns. It separates original source bytes, converted native bytes, SQL UTF-16 units and Tedious display strings. It does not implement a converter or establish padding, bounded capacity, assignment, arbitrary collation or runtime support.

The fixture retains four fresh databases in two independently owned containers pinned to SQL Server 17.0.4065.4, tedious 20.0.0, Node 24.13.0 and packet size 512. Each run has 12 cases and 1,068 supplied rows: four complete individual-byte tables (256 bytes plus NULL/empty), four mixed controls, and four large fragmented controls. Every original post-login request and reply, descriptor, native/UTF16 projection, error/info/completion callback and original-session diagnostic is preserved. Login credentials are excluded.

Fixture SHA256: `d9d8baa3ce4530f1af077feda8c80c9394b110557748b1fb84df0db9201189c2` (10,842,443 bytes). Its original collector SHA256 is `1f5efb789cc7b3332a906de50e2c44fc0dc38b1ec2600ef3dd2b3a3a62cb5960`. Independent complete semantic, request, response and packet-framing pins agree across all four runs and are embedded in the subsequent collector. No client or platform codec supplies conversion expectations.

All 12 cases succeeded in each run. NULL stays NULL and empty stays empty. The same-CP1252 tables preserve every source byte exactly, including NUL and undefined bytes. SQL Unicode and UTF8 targets preserve the SQL Unicode projection of each original byte. CP1251 conversion remains separately captured, including losses; the original inputs are never replaced with converted expectations.

| Original CP1252 byte | Same-CP1252 native | Same-CP1252 public display | SQL UTF-16 units | UTF8 native | CP1251 native / SQL units |
| --- | --- | --- | --- | --- | --- |
| `81` | `81` | U+FFFD | `8100` | `c281` | `3f` / `3f00` |
| `8d` | `8d` | U+FFFD | `8d00` | `c28d` | `3f` / `3f00` |
| `8f` | `8f` | U+FFFD | `8f00` | `c28f` | `3f` / `3f00` |
| `90` | `90` | U+FFFD | `9000` | `c290` | `3f` / `3f00` |
| `9d` | `9d` | U+FFFD | `9d00` | `c29d` | `3f` / `3f00` |

The mixed controls contain all 256 bytes in one value and explicit undefined-byte/NUL sequences. The large source is 9,256 native bytes: ASCII prefixes/suffixes, 33 repeats of all 256 bytes and an undefined-byte suffix. Captured target native lengths are 9,256 bytes for CP1252 and CP1251, 14,046 for UTF8, and 18,512 for SQL Unicode. The full source, PLP values and original packet splits are preserved. Focused tests check coherence against the actual individual-byte observations rather than deriving a code-page table from a platform codec; this finite agreement does not establish arbitrary string conversion semantics.

The collector reuses owner-reviewed task895 bounded trace primitives and task899 retention envelopes (owner PR902 checkpoint `edbdd67a`). It pins both ancestor scripts and all three shared helper sources before Docker starts. It validates exact phase message types, SPID consistency, packet status/IDs/EOM and fragmentation, with at most 2 MiB and 1,024 frames per exchange, reserved failure diagnostics and awaited trace errors. Raw and complete comparison envelopes are separately bounded to 48 MiB. Original observations survive failed validation or derived comparison overflow, with an explicit failed sidecar rather than truncated completeness claims.

Run `node scripts/capture-bulk-character-cp1252.mjs --replay-fixture` for offline validation, or pass a fresh output path for a new four-run capture. Existing files, canonical output, hardlink/symlink aliases and regular-file ancestors are rejected before Docker. Complete raw differences, including ephemeral database/server/SPID fields, remain unnormalized. Fixed verification projections remove only checked ephemeral identities from their digests; they do not change the fixture or sidecars.

Fresh full reproduction, focused tests on both hosts, independent review and exact-head CI/Codex evidence are recorded in the PR. This task adds no Rust, engine, catalog, output or BulkLoad runtime changes.

The frozen collector SHA256 is `7a1e2540ff6d25eae530b26ab96ea114f26f13a28b81de46d0e3c58e7d956d5f`. A fresh complete four-run reproduction exited 0 and passed every fixed semantic/request/response/framing pin: SHA256 `1f391f578a0a99024e873455396cc31246495b13df85919b74917499668b2635`, 9,378,995 bytes. Its complete sidecar preserves all 6,013 raw differences from the retained fixture and independently matches a full recomputation. Raw artifacts are not required to have equal byte sizes: their complete, unnormalized cross-run differences remain part of each artifact. All eight runs have 12 cases and 1,068 supplied rows, with at most 258 rows per case.

All 16 focused tests pass with zero failures, skips, cancellations or TODOs on macOS Node 26.5.0 and Linux Node 24.13.0. They check all byte projections and mixed/fragmented coherence against captured tables, exact undefined-byte distinctions, uniform corruption with recomputed comparisons, omissions/provenance, payload-preserving packet repartition, phase/type/SPID/status/EOM, awaited trace failure, resource/comparison exhaustion, output aliases and raw-first failure retention. The trace persistence test retains the reviewed bounded 60-second deadline for slower workers; production collection limits are unchanged. These are reference-collector proofs, not a native msduck client-suite or compatibility pass.
