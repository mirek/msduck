# Deterministic statistical state

`msduck-core::bounded_aggregate::FloatStatsState` implements a sequential
binary64 accumulator for the four SQL Server statistical functions. The caller
converts one non-NULL operand to binary64 and calls `push` once. The state keeps
`count`, `sum` and `squares`, then calculates `max(0, squares - sum*sum/count)`.
Sample variance divides by `count-1`; population variance divides by `count`;
standard deviations take the square root. Empty inputs return NULL for all
four functions. A singleton returns NULL for sample functions and positive zero
for population functions. Non-finite input, intermediate or final arithmetic,
and count overflow mark the state as failed; a later input cannot erase failure.

Tests compare exact binary64 result bits against all **156** non-error cells in
`reference/statistical-precision.json` and all **812** non-error cells in
`reference/statistical-transition.json`, including 96-row ordered windows and
three observed arrangements of the same large decimal multiset. The two
captured SQL Server overflow requests fail with 8115; core tests verify that
their binary64 transitions fail rather than returning infinities. The core
reports a typed overflow result; the root adapter must turn it into the correct
SQL Server diagnostic.

This state is a deterministic arithmetic component, not a complete aggregate
implementation. Typed DISTINCT deduplication belongs upstream, before
binary64 conversion. Native DuckDB registration, NULL warning observation,
metadata, wire encoding and error mapping remain root-side. No parallel
`combine` operation is defined because the captured computation is sensitive
to transition order. The SQL Server captures do not establish a universal
physical order for grouped or parallel plans; a native integration must test
those plans and preserve one evaluation of volatile operands before claiming
full parity.
