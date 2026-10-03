# CONCAT_WS and TRANSLATE UTF-16 output boundaries

This fixture records SQL Server behavior, not a runtime implementation or compatibility completion claim. The generator captures 44 programs in two fresh databases per independently owned SQL Server 2025 container, four identical complete sequences. Containers share explicit hostname `msduck-text-reference`; their names, ports and databases are independently generated. The image is the repository's pinned digest `86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a`.

Fixture SHA-256: `a9aacc82bdfd34f6fb854cf95d2a8fda9a2a3b9eb20b46e55fb6d78c13219ecf`. Each complete sequence hashes to `d73f4c7a1c4c3db6e4ced38cdb99a0698ed94c379e21ba4f45b6e74d7788344c`.

## Observed boundaries

Every successful scalar probe retains the value, DATALENGTH, the final UTF-16 code unit through `UNICODE(RIGHT(value COLLATE Latin1_General_100_BIN2,1))`, and `CONVERT(VARBINARY(MAX),value)` as exact UTF-16LE bytes. Validation compares those redundant observations while leaving raw BIGINT length strings and all original fields unchanged. The tables below summarize both `SQL_Latin1_General_CP1_CI_AS` and `Latin1_General_100_CI_AS_SC` observations; each variant remains separately present in the fixture.

The bounded CONCAT_WS declarations are NVARCHAR with wire capacity 8000 bytes. The prefix is 3999 `a` units unless stated otherwise. Empty separator probes use `N''`. Emoji is U+1F600, represented by high U+D83D and low U+DE00; isolated units come from separately typed NCHAR/CAST expressions. A later `Z` exposes an incorrect capacity refill after an overflowing value.

| CONCAT_WS components | Retained bytes, ordinary / SC | Final unit, ordinary / SC |
| --- | --- | --- |
| split leaves | 7998 / 7998 | 97 / 97 |
| lone high fits | 8000 / 8000 | 55357 / 55357 |
| lone high overflow tail | 8000 / 8000 | 55357 / 55357 |
| split pair leaves | 8000 / 8000 | 55357 / 55357 |
| split pair overflow tail | 8000 / 8000 | 55357 / 55357 |
| pair leaf overflow tail | 7998 / 7998 | 97 / 97 |
| pair crossing joined leaf | 8000 / 8000 | 55357 / 55357 |
| pair exactly fits | 8000 / 8000 | 56832 / 56832 |
| max preserves pair tail | 8004 / 8004 | 90 / 90 |
| low surrogate fits | 8000 / 8000 | 56832 / 56832 |
| pair separator overflow | 8000 / 8000 | 55357 / 55357 |
| high separator low leaf | 8000 / 8000 | 55357 / 55357 |
| high leaf low separator | 8000 / 8000 | 55357 / 55357 |
| pair separator exactly fits | 8000 / 8000 | 56832 / 56832 |

A single emoji value argument crossing capacity loses the entire pair: 7998 bytes, final `a` (97). A subsequent `Z` is not appended to fill the resulting free unit. In contrast, splitting high and low units across value arguments retains the high unit at capacity: 8000 bytes, final 55357, raw suffix `3dd8`. This also holds when prefix plus high unit forms one already-full first leaf and low plus tail forms the next leaf. An exactly fitting pair remains intact; MAX retains the pair and tail.

The separator is different. An emoji separator crossing capacity retains the high unit (`3dd8`), unlike an emoji value argument. High-separator/low-value and high-value/low-separator also retain the high unit. A global flattened-output pair trim would discard captured data in those cases, and using value-argument pair trimming for separators would also contradict the observations. These results ground only the listed append boundaries; they do not establish a universal implementation algorithm or every source/encoding/collation rule.

| Other expression | Ordinary | SC |
| --- | --- | --- |
| Native bounded CAST of prefix plus emoji | 8000 bytes, lone high 55357 | 8000 bytes, lone high 55357 |
| Bounded TRANSLATE, 3999 `a` plus `x`, `x` mapped to emoji | Error 9828, state 3, class 16 | 7998 bytes, final `a` |
| MAX TRANSLATE with the same mapping | Error 9828, state 3, class 16 | 8002 bytes, final low 56832 |
| Native lone-high control (NVARCHAR(1)) | 2 bytes, high 55357 | 2 bytes, high 55357 |
| TRANSLATE lone-high replacement | 8000 bytes, high 55357 | 8000 bytes, high 55357 |
| TRANSLATE two expanded replacements near capacity | Error 9828, state 3, class 16 | 8000 bytes, final low 56832 |

The non-SC mapping mismatch follows the retained one-unit versus two-unit mapping operands. The captured CAST and function results are different; no generic CAST width or truncation rule is inferred from CONCAT_WS. Unknown domains include arbitrary codepages, other collations, separator multiplicity at other boundaries, conversion formatting, every malformed-surrogate pattern and runtime integration.

Two prepared programs repeat short/NULL/surrogate/overflow bindings and a repeated initial binding on one handle per collation, preserving descriptors independently of current parameter values. Their diagnostics include the same value/length/final-unit/raw-byte projection. Full prepare/execution/unprepare tokens and RETURNVALUE bytes remain retained.

## Evidence and reproduction

```sh
node scripts/capture-concat-boundary.mjs .tmp/reproduction.json
node scripts/capture-concat-boundary.mjs --check-fixture
node scripts/capture-concat-boundary.mjs --self-test
```

The generator retains complete ordered handler tokens and independent public events, including ORDER when present, raw DONE status/command/64-bit count, bounded fragment-safe RETURNVALUE bytes, full parsed scalar descriptors (userType/schema/udtInfo/tableName), and ERROR/INFO serverName/procName. Descriptor/message/completion events reconcile against ordered tokens. The complete sequence is pinned independently of agreement between copies. `--one-database` is diagnostic only; a new retained fixture requires all four captures and exclusive creation. CLI collisions, fixture aliases through symlinks/hardlinks, existing outputs and conflicting/unknown flags fail before Docker.

The offline self-test exercises every RETURNVALUE split, bytewise fragmentation, truncation rejection, and twelve coherent four-copy corruptions: rows, descriptors, missing/changed userType, omitted tokens, DONE fields, handle payload, diagnostic state/identity, missing tableName, raw UTF-16 and final-unit disagreement. Native build/cache/source work is unnecessary for this reference-only change; all capture work uses an isolated Linux reference directory and read-only cached Node dependencies.

The preceding private 10-program scratch capture and 36-program expansion are preserved separately in the worker's ignored evidence directory. All 34 nonprepared observations from the 36-program expansion are identical in every final copy. Eight separator controls were added; the two prepared queries intentionally add length/final-unit/raw-byte projections, changing their descriptor shape (including the outer-projection value flags) while preserving their bound values and underlying function results. No raw fields are normalized to conceal that difference. Final exact-head reproduction, CI and reviews are recorded in the PR.
