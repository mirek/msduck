# Expanded SQL Server ORDER evidence

`reference/order-token-expanded.json` contains two matching runs from fresh SQL
Server 17.0.4065.4 containers using the pinned image in
`scripts/lib/reference-container.mjs`. Each run has 98 request records: version,
setup, and 48 queries executed both as a SQL batch and through sp_executesql RPC.
The first 18 profiles retain the supplemental optimizer probes previously kept
locally by the logical planner. Raw ORDER bytes, decoded ordinals, full descriptor
flags, rows, errors, completion statuses/commands and token event positions remain
in the committed fixture. DONE row counts and rows are decoded by Tedious; the
capture does not retain whole wire messages.

The generator reuses `captureBatch` and `captureRpc` from the original trusted
ORDER capture module. Those are now exported without changing their behavior.
The original 32-profile and prepared API baseline runs first and is validated;
only its version/setup and the expanded requests are retained here. Raw token
capture remains bounded by the USHORT payload and restores parser hooks in
`finally`. Capture calls are sequential within one isolated connection.

| Captured profile | ORDER ordinals |
| --- | --- |
| `SELECT *` / qualified `h.*`, order b,a | 2,1 |
| mixed b,* projection, order ordinal 2 | 2 |
| joined h.*,c.b projection, order c.b,h.a | 4,1 |
| derived/CTE b,a expansion, order a | 2 |
| UNION ALL wildcard, order b,a | 2,1 |
| projected a+2, a*2, CAST(a AS BIGINT) | 1 |
| grouped COUNT/SUM result alias | 2 |
| hidden COUNT aggregate sort | 0 |
| projected collation expression | 1 |
| hidden collation expression | 0 |
| ROW_NUMBER over a scalar NULL subquery | 1 |
| projected typed NULL, alias or ordinal | no token |
| projected literal 1 / literal arithmetic 1+2 | no token |
| typed NULL key followed/preceded by column a | 1 only |
| two typed NULL keys | no token |
| CASE with equal literal branches over predicate a>0 | 1 |

These observations concern the exact queries and catalog in the generator. They
are not a general constant-folding algorithm: even the CASE profile with equal
branches preserves ORDER. Identical constant UNION branches retain ordinal 1;
equivalent joined columns keep their own projection identity. General expression
and optimizer behavior must not be inferred merely from syntactic ORDER BY or
from known rows in this tiny dataset.

Two profiles exercise explicit sp_prepare/sp_execute/sp_unprepare lifecycles
inside a batch, in both transport modes. Parameterized arithmetic is projected
in one and hidden in the other. Preparation and executions at @b=0,9,1 have
respectively 0,4,0,3 rows and four ORDER tokens: ordinal 1 for projected arithmetic
or zero for hidden arithmetic. Preparation metadata precedes executed rows;
unpreparation adds no extra ORDER. This supplements rather than replaces the
original fixture's Tedious prepare/execute API captures.

Errors are retained unchanged: duplicate bare projected names produce 209,
a literal arithmetic ORDER BY expression produces 408, and a parameter alone
as a sort key produces 1008. No ORDER is fabricated for these failures.

Run `node scripts/capture-order-token-expanded.mjs --check` and
`node --test tests/order_token_reference.test.mjs tests/order_token_expanded_reference.test.mjs`
to verify integrity and exact expectations. A new capture uses two fresh owned
containers; `--write-fixture` refuses to overwrite the retained fixture. Use the
repository's locked Linux remote runner/cache for capture. The SHA-256 constant
in the generator binds the retained bytes; tests bind every request's SQL/mode,
ORDER payload/ordinals, prepared phases and error codes.

This is reference acquisition. The merged logical planner does not yet support
all these profiles, and root ORDER emission remains unfinished. The fixture does
not prove msduck produces these rows, descriptors or tokens.
