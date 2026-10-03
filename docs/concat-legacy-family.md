# CONCAT_WS legacy-family diagnostic reference

This is retained SQL Server evidence, not an implementation or compatibility pass. `scripts/capture-concat-legacy-family.mjs` observes 283 programs in each of four fresh databases: two databases in each of two independently owned SQL Server 2025 containers. A complete second four-database reproduction from the final generator matched the retained fixture. Container names, ports and databases are random; the explicit hostname `msduck-legacy-reference` makes raw diagnostic server identity comparable. The image is pinned to repository `referenceImage`, digest `86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a`.

Fixture `reference/concat-legacy-family.json` has 2,366,900 bytes and SHA-256 `636c51a2516b74ebb3dbb3a16e4478ac58a6d14bae33a832e18d69e31aef1711`. Every complete `JSON.stringify(run)` hashes to `0f1d0ca87a751e734318f1d296eac9ec6b0d07e3cc8998ea870defe516aa4336`. The generator pins that full sequence independently of four-copy agreement. The digest detects changed retained evidence; it is not independent semantic proof.

## Inputs and controls

Five original source declarations remain explicit: XML with `'<a/>'`, SQL_VARIANT with `1`, NTEXT with `N'a'`, TEXT with `'a'`, and IMAGE with `0x41`. Each repeats typed NULL. The script does not infer source kinds or capacities from current values.

There are 40 isolated controls: ten native scalar SELECTs and thirty `CONCAT_WS(separator, source, NULL)` SELECTs. There are 240 mixed controls: ten unordered source pairs, both argument orders, all four value/typed-NULL combinations, and three separators (`''`, `N''`, literal NULL). Pairs include XML or SQL_VARIANT with each legacy family, XML with SQL_VARIANT, and every pair of NTEXT/TEXT/IMAGE. These are two-value-argument expressions; arbitrary arities, legacy-typed separators, triples, custom collations, other sessions and TRANSLATE remain uncaptured here.

Every native, isolated and mixed control is followed by a separate request `SELECT @@TRANCOUNT AS transaction_count,1 AS reusable`. Its full result is retained under `reuse` and is exactly `[0,1]`, including after compile diagnostics. This is actual connection reuse; the probe is not appended to a batch that failed compilation. The remaining three programs are server version/collation, a prepared INT observer control, and a final reuse probe. The prepared control retains prepare, four bindings (1/NULL/2/1), unprepare and unchanged descriptors. It exercises the observer's RETURNVALUE path; it does not establish prepared legacy-family behavior.

## Observations

NTEXT selects the Unicode conversion target even when its current value is NULL and the separator is ANSI or literal NULL. This is unchanged by moving NTEXT before or after XML, SQL_VARIANT or IMAGE. TEXT alone selects VARCHAR with ANSI/NULL separator; an explicit Unicode separator selects NVARCHAR.

| Rejected original family | Number/state/class | ANSI target diagnostic |
| --- | --- | --- |
| XML | 257/3/16 | `Implicit conversion from data type xml to varchar is not allowed. Use the CONVERT function to run this query.` |
| SQL_VARIANT | 257/3/16 | `Implicit conversion from data type sql_variant to varchar is not allowed. Use the CONVERT function to run this query.` |
| IMAGE | 206/2/16 | `Operand type clash: image is incompatible with varchar` |

Unicode targets replace only `varchar` with `nvarchar` in these observed messages; the complete raw messages, serverName, procName, lineNumber and ordered ERROR/DONE tokens are retained. NULL does not remove a declared incompatible family. In the captured mixed matrix, XML produces 40 NVARCHAR-target and 32 VARCHAR-target diagnostics; SQL_VARIANT has the same counts; IMAGE has the same counts. The remaining 24 mixed controls succeed.

When both value arguments are incompatible, the first incompatible argument supplies the diagnostic: XML before IMAGE gives 257 for XML; IMAGE before XML gives 206 for IMAGE. XML/SQL_VARIANT and SQL_VARIANT/IMAGE follow the same order-sensitive rule, including typed NULL. The target domain still comes from the complete declaration context. This is a pairwise observation, not a claim about every wider shape or diagnostic barrier.

TEXT with NTEXT succeeds as NVARCHAR(MAX), userType 0, descriptor length 65535, flags 32, retaining the captured default collation LCID 1033, flags 13, version 0, sort ID 52, CP1252. Values produce `aa`, one typed NULL produces `a`, and both typed NULL produce the empty string. Native TEXT/NTEXT/IMAGE controls retain their own legacy source descriptors, exact values or NULL, and tableName fields. Successful isolated legacy controls and all errors remain separate records; the validator never substitutes a desired rejection for observed support.

## Fidelity, limits and reproduction

The observer is copied locally from approved reference #831/#815. It retains complete public rows (including raw UTF-16 JavaScript strings), every parsed descriptor property including userType/schema/udtInfo/tableName, diagnostics/info and ordered COLMETADATA/ROW/RETURNSTATUS/RETURNVALUE/ORDER/DONE tokens. DONE status, command and unsigned 64-bit count remain raw. Ordered descriptors/messages/row counts/completions reconcile against public events. The prepare handle's raw bytes reconcile with its decoded IntN value. No field sorting, message repair, outcome normalization or candidate implementation expectation enters the full run digest.

The fixed small-input capture limits each observed phase to 1,024 tokens and 1 MiB serialized evidence, retained envelopes to 128 MiB before JSON parsing, and RETURNVALUE fragments to 1 MiB with consumed prefixes preserved across every waitForChunk. Request timeout is 120 seconds, Docker command timeout 60 seconds, and the explicit capture deadline is 30 minutes, checked between requests and phases. Owned containers use 4 GiB memory, two CPUs and PID limit 512; private Node capture uses a 1 GiB heap. Phase bounds check these finite observations after decoding; they do not establish a general hostile-stream parser bound. Existing shared native sources, caches, executables and the locked builder are untouched.

From an isolated reference directory with cached Node dependencies and Docker:

```sh
node --max-old-space-size=1024 scripts/capture-concat-legacy-family.mjs .tmp/reproduction.json
node scripts/capture-concat-legacy-family.mjs --check-fixture
node scripts/capture-concat-legacy-family.mjs --self-test
```

Default capture uses four databases and compares the complete retained envelope, preserving raw output before validation. `--one-database` is diagnostic only. `--write-fixture` exclusively creates a new fixture and refuses an existing one. Output collisions, retained-path symlinks/hardlinks, existing destinations, incompatible flags and unknown options fail before Docker. Labels identify the fresh owned container; finally cleanup targets only that generated name.

All RETURNVALUE splits and bytewise fragmentation pass; truncation fails. Fifteen meaningful identical four-copy corruptions are rejected, including rows, metadata/userType, ordered tokens/raw DONE, raw handle, diagnostic number/message/server identity, source declaration/order, lost or changed reuse evidence and prepared status. Nine CLI/destination negatives refuse before Docker with the fixture unchanged. Five mocked lifecycle cases cover success, work/readiness/partial-start failure and auto-remove races. Exact-source two full four-database captures, syntax/diff/source review and 28 existing agent/shard tests passed. Exact-head CI, independent review and owner-enabled Codex evidence belongs to the PR. Reference intermediates remain in private Linux staging; only the retained fixture was copied locally.
