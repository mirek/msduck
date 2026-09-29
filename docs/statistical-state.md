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

The root native adapter in `src/decimal_aggregate/statistical.rs` now uses this
state for STDEV, STDEVP, VAR and VARP. Registration runs through the existing
numeric aggregate entry point. SQL lowering collects a typed LIST, retaining
DISTINCT and window membership/order on that call, then converts the completed
list to DOUBLE for the scalar accumulator. DISTINCT therefore runs before
binary64 conversion. The source operand appears once; NULL observation runs
before ordinary deduplication or on the consumed window frame. Empty frames do
not warn about NULLs excluded from the frame. Native overflow maps to the
captured 8115/state 2/class 16 float error and leaves the connection reusable.

The native callback copies child values into a 1,024-element flat scratch
vector before reading them. This handles constant/dictionary child vectors and
checks list bounds and selection-index capacity. Its accumulator and scratch
storage are bounded, but DuckDB-owned input LISTs materialize a whole group or
frame; ordinary aggregate memory is therefore proportional to its input and
large window frames can be expensive. This is an explicit cost of evaluating
the observed sequential arithmetic without inventing a parallel combine rule.

Independent tedious replay matches all 156 precision cells and 812 transition
cells, including exact FLOAT(53) descriptors, NULLs, warnings and captured
overflow diagnostics. Full observations retain separate token/completion
differences. The SQL Server captures do not establish a universal physical
order for grouped or parallel plans, and DuckDB LIST aggregate transition
order is not a SQL Server plan guarantee. Further plan/type/scale evidence and
performance work remain necessary before claiming full statistical parity.
