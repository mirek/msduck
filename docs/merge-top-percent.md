# MERGE TOP percentage boundaries

`reference/merge-top-percent.json` retains 110 raw observations from each of two
fresh databases on the pinned SQL Server 2025 image documented in
`docs/merge-top-reference.md`. A second independent container and two more fresh
databases reproduced every count and diagnostic invariant. The fixture SHA-256
is `4272bd06042205d8f83650d2d4e3cdf1bf4a5c92ffe2331a1b60a75a67753988`.
Each record includes the original SQL, ordered TDS descriptors and rows, errors,
information and DONE events. The capture checks OUTPUT IDs against final target
rows but never asserts which eligible IDs TOP chooses or their OUTPUT order.

For seven eligible INSERT candidates, observed affected counts were:

| PERCENT expression | Count or diagnostic |
| --- | --- |
| `0`, `1`, `25`, `33.333`, `50`, `75`, `100` | `0`, `1`, `2`, `3`, `4`, `6`, `7` |
| `1.5`, `50.0`, `99.999` | `1`, `4`, `7` |
| `101`, `100.001`, `-1` | `1031/state 1/class 15`: `Percent values must be between 0 and 100.` |
| `NULL` | `1014/state 1/class 15`: `A TOP or FETCH clause contains an invalid value.` |
| declared `@p INT=25`, `@p DECIMAL(6,3)=33.333` | `2`, `3`; each DECLARE has its own DONE before MERGE DONE |

At 25%, candidate counts `0,1,2,3,4,7` yielded `0,1,1,1,1,2`.
At 33.333%, three and four candidates yielded one and two. At 50%, three
and four yielded two and two; at 75%, three and four yielded three and three.
These agree with percentage rounding upward. [Microsoft's TOP documentation](https://learn.microsoft.com/en-us/sql/t-sql/queries/top-transact-sql?view=sql-server-ver17)
states that PERCENT converts its expression to `float` and rounds a fractional
row count up. The retained evidence establishes these bounded values and
diagnostics, but not every float boundary or type conversion. The pure binder
accepts captured INT and fixed decimals of at most three fractional digits and
uses a checked 0.001% ratio; wider precision and unknown expressions remain
unsupported. Very large candidate counts and float edge cases still need
differential testing against SQL Server before claiming exact general parity.

The separate count-mode `TOP (-1)`, `TOP (NULL)`, and `TOP (1.5)` diagnostics
remain 127, 1060 and 1060 respectively in `reference/merge-top.json`.
The binder retains source AST and evaluated type/value, resolves variables once,
returns only an unordered candidate-selection limit, and performs no writes.
[Microsoft's MERGE documentation](https://learn.microsoft.com/en-us/sql/t-sql/statements/merge-transact-sql?view=sql-server-ver17)
places TOP after join and action qualification; it does not supply an action
order. Parser integration, statement-atomic execution, OUTPUT and DONE behavior
remain separate work.

Run `node scripts/capture-merge-top-percent.mjs artifacts/compatibility/merge-top-percent/recheck.json`
on a machine with Docker
and Node.js 24+ to recapture against two fresh databases in a new pinned
container. The script refuses to replace the committed fixture unless it is
absent and `--write-fixture` is explicitly given; otherwise it compares stable
invariants with the retained fixture and preserves the raw recheck artifact.
