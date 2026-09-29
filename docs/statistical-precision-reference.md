# Statistical accumulation reference

`reference/statistical-precision.json` retains 25 SQL Server 2025 requests
from two independent containers and fresh databases, both using the pinned
image in `scripts/lib/reference-container.mjs`. The complete fixture SHA-256
is `381131fd141da52a49839b17d7a607b9e90c7cb147b5c16e0802f1e9c66ba9e9`.
`node scripts/capture-statistical-precision-reference.mjs --check` verifies
that checksum, both complete captures, their equality and fixed control
vectors. Each request retains SQL text, rows, typed descriptors, diagnostics,
information tokens, event order, raw DONE status and command words, and the
IEEE-754 binary64 bits of every non-NULL FLOAT(53) result. The fixture is SQL
Server evidence; it does not claim that msduck already matches it.

The 23 statistical requests cover INT, BIGINT, DECIMAL, REAL and FLOAT inputs;
nearby large values, mixed signs and exponents, changed input order, NULL,
singleton and empty sets, typed DISTINCT, grouping, and ordered, bounded and
partitioned windows. The server-version and reusable-session requests anchor
the environment and verify recovery. All 25 complete observations agreed
across both containers.
Two further fresh containers reproduced the fixture byte for byte; the
separate replay is retained at
`artifacts/remote/linux.local/statistical-precision-reference/replay.json`.

For these inputs, a candidate binary64 calculation reproduced all **156**
captured statistical cells, including NULLs and zero results. Convert each
non-NULL input to binary64, accumulate `S = sum(x)` and `Q = sum(x*x)`, then
calculate `C = max(0, Q - S*S/n)`. The sample variance is `C/(n-1)`, the
population variance is `C/n`, and the standard deviations are their square
roots. Sample functions return NULL for fewer than two non-NULL inputs;
population functions return NULL for zero and +0 for one. DISTINCT must remove
duplicates in the original input type before binary64 conversion. This is a
fit to the captured observations, not proof of SQL Server's internal operator,
transition order or behavior for other input families.

| Input | Captured sample variance bits | Captured population variance bits | Observation |
| --- | --- | --- | --- |
| INT `[1,2,2]` | `3fd5555555555550` | `3fcc71c71c71c715` | Both input orders agree; uncorrected squared-deviation arithmetic would give different low bits. |
| BIGINT/DECIMAL `[2^53,2^53+1,2^53+2]` | `4350000000000000` | `4345555555555555` | Both source types agree; sample result is `2^54`, far from the mathematical value 1. |
| DECIMAL `[10^12,10^12+1,10^12+2]` | `0000000000000000` | `0000000000000000` | Direct binary64 subtraction is negative here; the captured result is +0, consistent with a zero clamp. |
| DECIMAL `[10^12,-10^12,3,-3]` | `44e1a582513bbe78` | `44da784379d99db4` | Mixed signs retain large finite results. |
| DECIMAL `[0.1,0.2,0.3]` | `3f847ae147ae1474` | `3f7b4e81b4e81b45` | Fractional conversion and arithmetic order affect low bits. |
| DISTINCT DECIMAL `[2^53,2^53+1,2^53+1]` | `0000000000000000` | `0000000000000000` | Sample stays non-NULL although its binary64 result is zero: two exact inputs survive deduplication. |

The same candidate calculation matches the 4-column results of the grouped
case and every captured window row, including ascending and reversed frames.
The null-containing aggregate, all-NULL aggregate, DISTINCT aggregate and
bounded window each emit one 8153 information token; an empty input emits
none. Both independent runs agree on raw DONE words and token order. The
ordinary grouped aggregate's physical transition order is not established by
these queries. The explicit window `ORDER BY id` fixes frame membership and
row order but does not prove how SQL Server combines partial states.

The proposed numerical successor is a bounded root-adapter implementation
task, to publish when the needed engine files are free. Its implementation
scope should include a new root statistical state module, root registration
and SQL lowering, plus focused root wire regressions. Acceptance should
require one evaluation of each source operand, typed DISTINCT deduplication
before binary64 conversion, exact rows and FLOAT(53) descriptors for all
captured cases, one 8153 warning per statement when NULLs are eliminated,
and grouped and ordered-window behavior without tolerance-based comparison.
The state needs bounded `count`, `sum` and `sum of squares` storage; it must
not expand a volatile expression into separate `SUM(x)` and `SUM(x*x)` calls.
Before claiming general numerical parity, add focused SQL Server evidence
for order-sensitive long inputs, negative zero, extreme overflow, and
cancellation near the zero clamp. Coordinate any root engine path with active
reservations before publishing the successor. This capture task changes no
runtime lowering.
