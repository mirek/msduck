# Statistical transition order and extreme-value reference

`reference/statistical-transition.json` retains 18 requests from two independent
SQL Server 2025 containers and fresh databases, using the pinned image in
`scripts/lib/reference-container.mjs`. The fixture SHA-256 is
`b8cec3ef18bb56562b6b167c716a1e4e1f4433dd4c4a389510819e1a430d3000`.
Run `node scripts/capture-statistical-transition-reference.mjs --check` to verify
the checksum, independent captures, fixed controls and candidate comparison.
Each observation retains SQL text, typed descriptors, rows, diagnostic and
information tokens, event order, raw DONE status/command words and separate
IEEE-754 binary64 result bits. The separate bit field matters for negative zero:
JSON serializes both +0 and -0 as `0`.
Two further fresh containers reproduced the fixture byte for byte. Their raw
replay is retained at
`artifacts/remote/linux.local/statistical-transition-reference/replay.json`.

The zero-clamped sum-of-squares candidate from
[the earlier precision capture](statistical-precision-reference.md) matches
**812/812** non-error statistical cells here, including every row of two
96-row ordered windows. In that candidate, each typed non-NULL input is converted
to binary64, `S = sum(x)` and `Q = sum(x*x)` accumulate in input order, and
`C = max(0, Q - S*S/n)` feeds sample/population variance and square root.
This fit is evidence for a possible arithmetic path, not proof of SQL Server's
internal implementation, a guaranteed aggregate input order, or all input
families. The two overflow requests returned errors and are excluded from the
812-cell comparison.

| Vector | Captured VAR bits | Captured VARP bits | Observation |
| --- | --- | --- | --- |
| 96 alternating `10^12-1`, `10^12+1` DECIMAL values | `41caf286bca1af28` | `41caaaaaaaaaaaab` | Mathematical variance is near 1; observed result reflects cancellation. |
| Same values, low half then high half | `41d58ed2308158ed` | `41d5555555555555` | Same multiset, different observed bits. |
| Same values, high half then low half | `41a58ed2308158ed` | `41a5555555555555` | Another distinct observed result. |
| `10^8, 10^8+1, 10^8+2` DECIMAL | `0000000000000000` | `0000000000000000` | The candidate clamps a negative cancellation residue to +0. |
| `10^10, 10^10+1, 10^10+2` DECIMAL | `40e0000000000000` | `40d5555555555555` | The cancellation residue is positive and large. |
| `10^11, 10^11+1, 10^11+2` DECIMAL | `0000000000000000` | `0000000000000000` | Cancellation returns +0. |
| `10^13, 10^13+1, 10^13+2` DECIMAL | `4210000000000000` | `4205555555555555` | A different positive cancellation residue. |

The three aggregate requests use different `VALUES` arrangements. SQL Server
SQL semantics do not promise an aggregate transition order without an explicit
order-sensitive operator, so these are observed plans, not a contractual
permutation rule. The window requests specify `ORDER BY id ROWS UNBOUNDED
PRECEDING`; reversing the value sequence changed **108 of 384** statistical
window cells, while all four final-row cells matched. Both independent runs
agreed on every captured bit, descriptor, diagnostic and completion field.

`CAST('-0.0' AS FLOAT)` and unary negation of a FLOAT zero each produced bit
pattern `8000000000000000`. Statistical results for FLOAT/REAL zero inputs and
for clamped cancellation were positive zero (`0000000000000000`). Both the
`1e308` and `1e154` paired-sign FLOAT requests returned error **8115**, state
2, class 16, “Arithmetic overflow error converting expression to data type
float.” Each emitted FLOAT(53) column metadata, no rows and DONE status `2`;
the following request remained usable. The `1e150` paired-sign request returned
finite statistical values and did not error.

A numerical implementation must keep one evaluation of each source operand,
retain typed DISTINCT identity before floating conversion, detect overflow as
a SQL Server error rather than emitting non-finite values, and preserve +0 for
clamped and zero aggregate results. It should not substitute a stable or
mathematically accurate variance algorithm for the observed behavior without
further ground truth. Root engine paths remain reserved by another claim; this
task changes no runtime lowering. Further reference work should test physical
plan variation, grouping, parallel transitions and input families outside these
captures before claiming a general transition-order rule.
