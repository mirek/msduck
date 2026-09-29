# FLOAT SUM/AVG execution

Known REAL/FLOAT SUM and AVG collect a typed LIST, retaining DISTINCT identity,
window membership and one source evaluation. Conversion to DOUBLE follows
source rounding and deduplication. The root adapter copies constant/dictionary
child vectors into 1,024-element flat scratch blocks with checked bounds.
`msduck-core::bounded_aggregate::FloatSumState` performs sequential binary64
addition and sum/count division. NULL inputs do not increment count; empty
inputs return typed NULL. Non-finite input/intermediate/final values and count
overflow fail permanently. The native error is the captured FLOAT overflow,
8115/state 2/severity 16, with connection reuse.

The state intentionally has no combine method. Regrouping binary64 additions
can change ordered-window results. The native accumulator and scratch memory
are bounded; DuckDB materializes groups/frames as LISTs, with O(n) memory and
potentially expensive large windows. Physical order for unordered DISTINCT or
parallel aggregation is not guaranteed. Tests retain those raw differences and
accept only exact possible DISTINCT transition permutations for the captured
three-value case, without a floating tolerance or sorted replacement input.

The independent client replay covers all 45 non-version observations in
`reference/float-aggregates.json`, checking every descriptor, stable floating
bit, NULL, warning/error and reuse. It saves full observations and separate bit
and wire-event differences in ignored compatibility artifacts. Additional
native tests cover non-flat children, scratch chunks, bounds, NULL shapes,
non-finite input, and volatile single evaluation. Prepared parameters, catalog
columns and catchable overflow have independent client coverage.

The focused independent replay passed all four client tests and 45 retained
observations, including all three canonical overflow cases. Exact final-head
workspace/client/audit and review evidence is recorded in the linked PR.
Unknown operand declarations remain outside this known-type path and must
remain explicit future compiler work, rather than fabricated type inference.
Full floating aggregation/plan compatibility and streaming execution remain
unfinished.
