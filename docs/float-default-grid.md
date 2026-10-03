# REAL/FLOAT default-format grid reference

This is retained SQL Server ground truth for a bounded finite IEEE grid, not an msduck runtime or exhaustive formatting pass. `scripts/capture-float-default-grid.mjs` retains 2,180 programs per run: 2,170 ordinary typed RPC grid inputs, eight single-control prepared programs and two version/session controls. Two fresh databases in each of two independently owned containers produce four complete matching observations. A second complete four-database reproduction matches the fixture without field projection, sorting or normalization.

The pinned image is `mcr.microsoft.com/mssql/server:2025-latest@sha256:86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a`, explicitly selected despite environment image overrides. The observed SQL Server version is `17.0.4065.4` and the server/database collation is `SQL_Latin1_General_CP1_CI_AS`. Container names, loopback ports and fresh database names are independently generated; explicit hostname `msduck-float-grid-reference` keeps raw diagnostic server identity comparable without changing captured fields.

Fixture SHA-256: `1a86ed8f35876029c94b317c3520917cfce4d3f180c286e5612494cbeaeddba9` (59,925,647 bytes). Every complete `JSON.stringify(run)` independently hashes to `62652effb096086238363d024e4cb63783760b30ba29367d3844b6be643fb87e`. The pinned complete-observation digest detects even consistent corruption of all four copies. It is an integrity contract, not an independent SQL semantics oracle.

## Deterministic admitted inputs

Original IEEE little-endian bytes are the inputs. Decimal intent records identify the grid construction, not an assumed exact decimal value or a rule inferred from returned text. JavaScript parses the retained decimal intent to the nearest binary value; REAL admission then rounds once to IEEE binary32. The resulting binary32/binary64 bytes, declaration, outgoing TYPE_INFO, length and actual generated payload are recorded before execution. Native SQL VARBINARY storage is separately captured and verified against those exact bytes (SQL float storage is big-endian in these observations).

| Grid family | Admitted ordinary RPC programs |
| --- | ---: |
| Nearest representable decimal powers, both signs | 1,432 |
| Selected adjacent powers, offsets -1/0/+1, both signs | 108 |
| Six-digit boundary centers and adjacent bits, both signs | 600 |
| Explicit binary boundaries, both signs | 28 |
| Typed NULL, one per REAL/FLOAT declaration | 2 |

REAL powers cover every decimal exponent from -45 through 38; FLOAT powers cover every exponent from -323 through 308. They are nearest finite IEEE representations of decimal powers, which usually are not exactly representable. Selected adjacent REAL powers use exponents -45, -44, -38, -4, -3, 5, 6, 38; FLOAT uses -323, -322, -308, -307, -4, -3, 5, 6, 307, 308. The adjacent lower REAL bit at the smallest power is zero, whose sign remains explicit.

Six-digit boundary mantissas are `1.234565`, `1.234575`, `9.999985`, `9.999995`. Each decimal center and its immediately preceding/following positive IEEE bit pattern is admitted, then each sign is observed independently. REAL exponent selections are -40, -20, -5, -4, -3, 0, 5, 6, 10, 20, 35; FLOAT selections are -320, -310, -200, -100, -5, -4, -3, 0, 5, 6, 50, 100, 200, 300. Binary boundaries include positive/negative zero, minimum/second/maximum subnormal, minimum normal and penultimate/maximum finite values.

The grid deliberately retains repeated IEEE patterns with their distinct construction provenance; observations are not deduplicated. Every admitted non-NULL value must be finite, its actual tedious payload must match the retained bits, and each control must succeed. No admitted failure is filtered away. All exponent-all-ones IEEE patterns (NaNs and infinities) are explicitly excluded. Decimal powers below/above the listed per-type range, and finite bit patterns outside the explicit grid, remain uncaptured. This does not prove behavior for those exclusions.

Tedious20.0.0 normally loses numeric negative zero by calling `parseFloat`. The same process-local admission wrapper as the approved numeric reference preserves only that input in validation/payload generation. The negative-zero payload must round-trip through the actual encoder and native SQL storage. Observed negative zero uses reversible `{kind:"number",value:"-0"}` JSON because JSON numbers alone cannot retain its sign; no output is changed to fit a formula. Existing exact binary/BIGINT/missing/nonfinite carriers remain intact.

## Separate controls and prepared metadata

Each ordinary RPC program has four separate SELECT statements/result sets, with role, SQL and set index retained explicitly:

1. Native typed source plus `CONVERT(VARBINARY(MAX),@p)` source storage.
2. Explicit VARCHAR(MAX)/NVARCHAR(MAX) style-0 conversion.
3. Implicit ANSI/Unicode TRANSLATE with empty mappings.
4. Implicit ANSI/Unicode CONCAT_WS first-source forms with empty companions.

They are independent labelled controls, not a same-text assertion. The raw request retains all rows, descriptors, diagnostics, completions and tokens. Combining four fixed controls into one bounded program permits every finite decimal exponent within the 2,500-program budget. Any control error invalidates admission rather than causing the source or another control to be silently omitted. Native source echoes establish the actual bits independently of current formatted strings.

Preparing the four-statement RPC did not expose result descriptors in initial private captures. Those raw captures remain retained; the final suite instead prepares each control individually for each REAL/FLOAT declaration. Eight prepared programs retain prepare, six executions (positive zero, negative zero, minimum subnormal, maximum finite, NULL, repeated positive zero), and unprepare. All 48 executions compare full descriptors against actual prepare metadata; no descriptor is synthesized. Native prepared storage also proves the original bits. Every RPC success status is zero and the raw RETURNSTATUS agrees with the public event.

All retained finite controls succeeded. NULL remains NULL for native/style-0/TRANSLATE, and CONCAT_WS returns empty strings with its non-null declaration. Native REAL/FLOAT descriptors retain FloatN length4/8; explicit conversions retain MAX length65535; numeric TRANSLATE retains VARCHAR8000 or NVARCHAR4000 (wire byte capacity8000); first-source CONCAT_WS retains VARCHAR25 or NVARCHAR25 (wire byte capacity50), with computed non-null flags32. These descriptors remain value-independent across each prepared binding sequence. Unicode descriptor lengths are bytes, not character counts.

## Confirmed formula counterexample

The frozen candidate numeric helper at `ec80f6e0055fc2ad1745636f5c348510498bd3ab` used binary multiplication by a decimal power followed by integer rounding. A private comparison of the actual Rust1.95 candidate against the unchanged raw captures found 1,632 differences in 52,032 non-NULL ordinary text comparisons across four runs: 68 inputs per run, each differing in all six explicit/function/domain controls. These counts concern the candidate source only; no msduck runtime adapter was tested or changed by this reference task.

The first retained counterexample is `grid 1867 FLOAT six-digit boundary`, original little-endian IEEE bytes `bee671526d3d6e16`, decimal construction intent `1.234565e-200`. The actual FLOAT parameter has TYPE_INFO `6d08`, length `08` and those exact payload bytes; native SQL storage is `166e3d6d5271e6be`. SQL Server returns `1.23456e-200` independently in explicit style0, TRANSLATE and CONCAT_WS, in ANSI and Unicode domains. The candidate returns `1.23457e-200`. The raw descriptors for each context remain independently retained (MAX versus bounded TRANSLATE versus CONCAT_WS), along with ordered tokens. This counterexample disproves the general scaling inference even though that candidate matched the earlier narrower numeric reference. Future helper revisions need their own exact-source comparisons; expectations must come from this immutable fixture rather than changing captured output to match a formula.

Positive/negative zero remain distinct formatting inputs; the previously captured default `-0` behavior is retained here. The wider grid also retains carries, fixed/scientific boundaries, large/small exponents and subnormal precision. Neither the finite exponent grid nor agreement among controls proves an exhaustive algorithm over every finite IEEE bit pattern. Exact decimal rounding/conversion assumptions remain unknown outside the captured input set.

## Fidelity, bounds and reproduction

```sh
node --max-old-space-size=1024 scripts/capture-float-default-grid.mjs .tmp/reproduction.json
node --max-old-space-size=1024 scripts/capture-float-default-grid.mjs --check-fixture
node --max-old-space-size=1024 scripts/capture-float-default-grid.mjs --self-test
```

The observer reuses the approved numeric-reference implementation: full public descriptors including userType/collation/schema/udtInfo/tableName; complete rows/exact carriers; ordered COLMETADATA with full descriptors, ROW/NBCROW markers, ORDER, ERROR/INFO with serverName/procName, RETURNSTATUS, RETURNVALUE and raw DONE status/command/64-bit count. Ordered descriptors/row counts/messages/completion kind/MORE/count reconcile with separate public events. RETURNVALUE buffering preserves consumed prefixes over every fragmentation point and checks raw prepare bytes against the decoded IntN4 handle. Unexpected errors or incomplete controls remain explicit failures, not normalized gaps.

The self-test checks every RETURNVALUE split and bytewise fragments, rejects truncation, and rejects fifteen coherent four-copy mutations of rows, userType/full metadata, missing controls/tokens, raw DONE/prepare payloads, input/grid/wire provenance, status and prepared width. Nine destination/CLI negative probes refuse overwrite/fixture aliases, existing files, conflicting flags and unknown flags before Docker; fixture bytes remain unchanged. Five fake-operation lifecycle cases verify successful work, work failure, readiness failure, partial start failure and auto-remove races clean up only their fresh owned name and close readiness connections, without invoking Docker. Fixture/output writes use exclusive creation. The full capture retains raw artifacts before validation; `--one-database` is diagnostic only and cannot create a fixture. `--write-fixture` refuses an existing fixture. A second full reproduction compares complete captures/envelopes against the retained fixture.

Each fixed phase is validated at no more than1,024 tokens/1MiB serialized data; the envelope is capped at128MiB and checked before parsing retained data. RETURNVALUE buffering is capped at1MiB. Requests time out at120seconds, Docker commands at60seconds, and a30-minute watchdog is checked between requests/container operations. The demonstrated invocation caps the Node heap at1GiB. One owned SQL Server container runs at a time with4GiB memory, two CPUs and512PIDs. Labels identify its private staging owner; lifecycle cleanup removes only that invocation's random container/databases. These are bounds for the fixed scalar grid, not a general hostile TDS parser proof or process-deadlock watchdog.

Reference staging uses approved helper copies and immutable cached Node dependencies under a private Linux directory. Shared native sources, caches, executables and the root's locked verification job are untouched; no native build or new worktree/agent is required. Redundant intermediate captures remain remote rather than creating duplicate local JSON copies. Required exact-head CI/review evidence belongs to this reference PR. Runtime integration, formatter proof beyond the retained grid, nonfinite values, other styles, arbitrary collation/codepage/session domains and surrounding protocol behavior remain separate work.
