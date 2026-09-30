# ORDER token ground truth

SQL Server ordered results emit TDS ORDER (0xA9) between COLMETADATA and the first
row, or before completion for empty results. The token carries a little-endian
USHORT payload length followed by little-endian USHORT ordinals. It carries no
ascending/descending flags. This is reference evidence; msduck does not yet emit
ORDER tokens.

The retained fixture contains two identical runs from fresh isolated SQL Server
2025 containers, pinned to the repository reference image (17.0.4065.4). Each run
has 68 request records and 76 phases: 32 query shapes in SQL batch and RPC modes,
version/setup, and two prepared requests with three executions and unprepare.
Full rows, column descriptors, errors, informational messages, return values,
raw DONE words and exact ORDER bytes/event positions remain in the fixture.
Its SHA-256 is `d35d174dabf26749937e6ca9a36bae3499551a4cdf5cc176dd9b0bb050ea4fac`.

The capture extends the owner upstream mssqlite investigation in
`todo/order-token-fidelity.md`. Its missing-token observation also appears in
msduck's complete runtime percentile and RAND replays. The copied protocol skill
and [MS-TDS specification](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-tds/b46a581a-39de-4745-b076-ec4dbb7d13ec)
provide the format; emission rules below come from retained live captures.

| Captured query shape | ORDER ordinals |
|---|---|
| SELECT a,b ORDER BY a, ascending or descending | 1 |
| SELECT b,a ORDER BY 2 or its alias | 2 |
| SELECT a,b ORDER BY b DESC,a ASC | 2,1 |
| SELECT b ORDER BY a | 0 |
| SELECT b ORDER BY a,b | 0,1 |
| SELECT a+1 AS k,b ORDER BY a+1 | 1 |
| SELECT a,b ORDER BY a+1 | 0 |
| Constant projection ordered by a source column | 0 |
| ORDER BY (SELECT NULL) | 0 |
| Empty SELECT a,b ORDER BY b,a | 2,1 |
| TOP(0) ordered by a | 1 |
| All-NULL projected constant ordered by its alias | no ORDER |
| Window ordering without outer ORDER BY | no ORDER |
| Derived TOP query's internal ordering only | no ORDER |
| Clustered table scan without explicit ordering | no ORDER |

Zero is an observed hidden/nonprojected key, not an invalid ordinal to discard.
Aliases and duplicate projected expressions require resolution against the output
projection; the duplicate-expression probe chooses the first projected ordinal.
TOP/OFFSET, DISTINCT, GROUP BY, UNION/UNION ALL and outer derived/window ordering
also retain their captured projected ordinals. ORDER BY syntax alone cannot
justify universal emission: the all-NULL constant probe omits the token while a
scalar-subquery constant key emits ordinal zero. Unknown shapes need further
captures rather than guessed metadata.

Both captured prepared statements emit COLMETADATA and ORDER during preparation
with no rows. Repeated executions retain the same ORDER payload, including empty
bindings; unprepare emits no ORDER. SQL batch and RPC captures agree on ORDER
payloads while preserving their different completion envelopes.

Run `node scripts/capture-order-token.mjs --check` and
`node --test tests/order_token_reference.test.mjs` for retained checks. The harness
captures bounded exact token payloads through Tedious before decoding and checks
raw/decoded ordinal agreement. Tests cover length rejection, the maximum even
USHORT payload, every fragmentation boundary and important retained emission
cases. `--write-fixture` refuses to overwrite retained evidence. Default capture
records two new fresh-container runs in an ignored artifact without replacing the
fixture. Use the shared Linux builder lock for live capture.

The next implementation needs a bounded deterministic ORDER codec and resolved
logical ordering metadata, followed by root emission in preparation and execution.
Neither this reference nor ordered rows alone establishes server compatibility.
No engine adapter files changed in this task.
