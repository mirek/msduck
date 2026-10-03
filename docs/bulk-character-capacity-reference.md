# Bulk character capacity reference

This finite SQL Server BulkLoad reference distinguishes the INSERT BULK declaration, the transmitted TYPE_INFO byte capacity, converted ANSI target byte capacity, and Unicode target code-unit capacity. It does not establish general CAST, assignment, arbitrary collation best-fit, transaction or staging semantics, and does not add server runtime support.

The retained fixture contains 96 probes and 294 supplied rows per run: four independent databases in two SQL Server 2025 containers pinned to 17.0.4065.4, with tedious 20.0.0 and packet size 512. All original post-login requests and replies, full descriptors, errors, INFO/completion callbacks, native byte projections, SQL UTF-16 projections, display values and original-session counters remain in the fixture. Login credentials are excluded. Cross-run comparisons retain ephemeral server/database/SPID differences without normalization. Fixed verification digests project only checked database/server identities; they do not modify observations.

Fixture SHA256: `cd82a8853bcac9f3fee98c1fb6c6f17564443918868ece1f1d9f66b3abb0ec92` (9,550,933 bytes). Its collector source SHA256 is `0a62a38a082ab493620b5c26d074126f97cbf733cfa078fbcc725ded60dce227`; the subsequent collector embeds independent complete semantic/request/response/framing pins from those observations. The reviewed task895 helper is pinned to `9dd773a8170ea2c255d51bf40574fa75474563902c9211a2bceae7f5382e8868`, alongside all three shared helper source hashes. Original evidence is retained before pin validation, including collection failures and comparison-overflow diagnostics.

The matrix includes CP1251/CP1252/UTF8 VARCHAR and CHAR sources, CP1252/UTF8 VARCHAR and CHAR targets, Unicode NVARCHAR and NCHAR targets, paired widths 1/2/3/4 where applicable (two redundant higher-width Unicode cases are replaced by incoming CHAR ROW-length controls), separate declaration-versus-wire capacity conflicts, NULL/empty/fixed padding and four MAX conversions. Source widths are native byte counts, not JavaScript string lengths. CHAR challenge rows exclude empty/ASCII distractors; dedicated CHAR controls record NULL, empty and ASCII padding separately.

| Actual input and target | Retained outcome |
| --- | --- |
| CP1251 `cff0` → UTF8 VARCHAR(1)/(2) | 2628 / success `d09f` (only П) |
| CP1252 `a9e9` → UTF8 VARCHAR(3)/(4) | success `c2a9` (©) / `c2a9c3a9` (©é) |
| CP1252 `41a9e942` → UTF8 CHAR(3)/(4) | 2628 / success `41c2a920` (A© plus space) |
| UTF8 Ω `cea9` → CP1252 VARCHAR(1)/(2) | 2628 / success `4f` (O), despite one converted byte |
| UTF8 € `e282ac` → CP1252 VARCHAR(1)/(3) | 2628 / success `80`, despite one converted byte |
| UTF8 € → NVARCHAR(1) | success `ac20` |
| UTF8 🦆 → NCHAR(1)/(2) | 2628 message retains lone high surrogate U+D83E / success `3ed886dd` |
| INSERT BULK width 1/wire width 2, or declaration 2/wire 1 | 4816, state 2, class 16, all 12 controls |
| Equal declared/wire CHAR(1), two-byte ROW, bounded CHAR(4) target | 4815, state 1, class 17, both source controls |
| CHAR(1) empty → CHAR(4)/NCHAR(4) | four spaces; NULL remains NULL |

There are 27 error-2628, 12 error-4816 and two error-4815 probes per run. Every failed probe has an empty ordered readback and counters `[error, 0, 0, 0]` for `@@ERROR`, `@@ROWCOUNT`, `XACT_STATE()` and `@@TRANCOUNT`. Successful counts match the supplied rowset. Declaration/TYPE_INFO mismatches report 4816; the equal-width oversized CHAR ROW controls report 4815. Initial exploratory VARCHAR/CHAR-to-MAX controls instead reported 4816; their full raw captures remain private and are not silently relabelled as the final bounded controls.

MAX conversions preserve 12,401-byte UTF8 outputs from CP1251 and CP1252, 4,401-byte CP1252 output from the UTF8 control, and 8,802-byte Unicode output. Their original request fragmentation, PLP framing and native values are retained. These observations cannot be replaced with a rule that merely checks the converted target byte count: source/preconversion capacity participates in the measured bounded failures.

Run `node scripts/capture-bulk-character-capacity.mjs --replay-fixture` for offline validation, or pass a fresh output path for a new four-run capture. The collector refuses canonical output, existing files, symlink/hardlink aliases and non-directory ancestors before Docker starts. Each exchange is bounded to 2 MiB and 1024 frames with phase/type/SPID/status/packet-ID/EOM validation, reserved failure capacity and awaited trace errors. Both the raw artifact and complete comparison envelope are bounded to 48 MiB. Overflow preserves bounded raw observations with an explicit failed sidecar; it never labels omitted differences complete.

Focused tests exercise uniform semantic/payload corruption with recomputed comparisons, omitted failures, exact capacity and partial-surrogate diagnostics, helper/source provenance, invalid framing, asynchronous trace failure, resource exhaustion, output aliases and raw retention before validation. Final reproduction and both-host validation are recorded in the PR; this document does not claim the root server implements these outcomes.

The frozen collector SHA256 is `2cbb4172e902e2a4817812de570d09e375533da8a3efaf539cd08f9408975e87`. A fresh full four-run reproduction completed with exit 0, SHA256 `a3a2c9e7639330512da8addbf114d65226f6d389e8d8359060f72c89f95af25b` (9,550,933 bytes), passing every independently pinned semantic/request/response/framing check. Its complete raw comparison sidecar is preserved; raw observations differ in actual database/server/SPID identities. Each of the eight final runs contains 96 probes and 294 supplied rows. All 94 unchanged probes also match the prior matrix's semantic/request/response/framing pins across all four runs (376 complete comparisons); the old full captures remain separately preserved.

All 15 focused tests pass with zero failures, skips, cancellations or TODOs on macOS Node 26.5.0 and Linux Node 24.13.0. In addition to the 4815/4816/capacity controls, they prove rejection of uniform MAX packet repartition with unchanged payloads, and preservation of complete bounded raw runs when derived comparisons overflow. These are reference-collector proofs, not native msduck compatibility or a whole client-suite run.
