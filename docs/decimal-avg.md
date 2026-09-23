# Exact DECIMAL AVG work

Typed DECIMAL AVG now uses an exact native aggregate instead of DuckDB's
floating-point AVG. The deterministic arithmetic lives in
`crates/msduck-core/src/decimal_aggregate.rs`; native vector and lifecycle
callbacks live in `src/decimal_aggregate.rs`. SQL lowering preserves DISTINCT
and window clauses, widens the input to precision 38 without changing its
scale, and evaluates the original argument once. Catalog-bound columns and
DECIMAL columns inferred from VALUES inputs use the same path.

`reference/decimal-avg.json` preserves 29 live SQL Server queries and their
canonical result metadata, values, diagnostics and completion events. It also
records the reference image and server version. Exact VARCHAR renderings avoid
relying on JavaScript Number for high-precision results.

The captures establish:

- AVG returns DECIMAL(38, max(input scale, 6)), including empty inputs.
- Division truncates toward zero. Two divided by three yields 0.666666 at
  scale 6, not 0.666667. Positive and negative probes cover scales 0, 2, 6,
  7 and 38.
- Intermediate sums are bounded at precision 38 and the input scale.
  Overflow remains an error even if a later row would cancel it.
- Increasing the output scale can overflow independently of accumulation.
- Ten copies of the largest 32-digit integer have a representable average
  even though multiplying their sum by one million would overflow i128.
  The core divides into quotient and remainder before scaling.
- Runtime overflow emits result metadata before error 8115, state 2,
  severity 16, with the message
  `Arithmetic overflow error converting expression to data type numeric.`
- All-NULL input produces NULL and warning 8153. An empty input produces
  NULL without that warning.

The result declaration agrees with Microsoft's
[AVG documentation](https://learn.microsoft.com/en-us/sql/t-sql/functions/avg-transact-sql?view=sql-server-ver17).

Current verification covers native scales 0 through 38, grouped and window
states, parallel combination, NULL and empty inputs, DISTINCT, exact values
beyond floating-point integer precision, and single evaluation of a sequence
argument. The independent tedious test checks table columns, derived VALUES,
variables, window peer groups, and empty result declarations.

The comparison in `artifacts/compatibility/decimal-avg-after.json` preserves
all differences from the 29 reference captures. Decimal AVG values and their
own result declarations now agree for all successful reference queries, and
all seven reference overflow queries now fail with error 8115. Only two whole
captures match exactly. Remaining differences are:

- CONVERT-to-VARCHAR metadata flags are 1 rather than 33.
- Native decimal-to-text conversion omits the leading zero at scale 38.
- Overflow lacks metadata before the error, retains a DuckDB message prefix,
  and reports state 1 instead of 2.
- Aggregate NULL elimination does not emit warning 8153.

More expression typing remains necessary: aggregate arguments whose DECIMAL
type cannot yet be inferred by the logical binder can still fall through to
DuckDB AVG. This work does not establish complete decimal expression or
aggregate compatibility.

The earlier arithmetic-only step passed 351 workspace Rust tests, strict
Clippy and formatting. Native adapter and query integration verification is
recorded separately as it completes. The preceding DECIMAL wire snapshot
completed remote verification with 348 Rust tests, 362 client tests and 296
audit captures; those counts do not include this AVG implementation.

The integrated implementation passed all 353 workspace Rust tests, strict
workspace/all-target Clippy, formatting, and the focused tedious AVG test on
macOS. The full Linux client/audit verification remains running in the frozen
AVG snapshot.

## Aggregate declaration propagation

The shared storage-type inference now retains DECIMAL AVG, SUM, MIN and MAX
result declarations. Projection inference uses the same rule against explicit
catalog/parameter declarations. This lets outer aggregates choose the exact
adapter after an inner grouped aggregate, CTE or derived table, including
parameter-only queries. No runtime parameter values are used for typing.

`reference/decimal-avg-expressions.json` records eight additional live SQL
Server probes. Their before/after comparisons are preserved in
`artifacts/compatibility/decimal-avg-expressions-{before,after}.json`. Five now
match completely, including result flags and completion events: AVG over
AVG/SUM/MIN window inputs and AVG through CTE/derived sources. Before this
change those five returned FLOAT metadata. Three probes still differ in type
and value: AVG over decimal arithmetic, COALESCE and CASE. Those are retained
as unresolved expression-typing gaps.

Both focused tedious tests pass, including an empty outer aggregate and a
parameter-only CTE. Full workspace/client/audit verification of this follow-up
is running locally. The remote run still covers the preceding native AVG
snapshot, so it cannot establish verification of these later inference changes.

## Conditional and arithmetic input typing

Shared storage inference now merges decimal conditional branches and reuses
existing SQL arithmetic precision/scale formulas for addition, subtraction,
multiplication and modulo. This supplies declared input types to AVG rather
than allowing its result to inherit DuckDB's floating-point AVG type. Decimal
division remains excluded from this inference path pending an exact execution
adapter; knowing its SQL declaration alone cannot repair floating-point division.

A native session test now executes the three previously failing expression
probes plus ISNULL, stores their results through SELECT INTO, and checks exact
text and DECIMAL(38,6) storage. It passes. A separate tedious test and audit case
cover the original three probes. These client checks have not yet run against
a rebuilt executable because the prior full client suite still owns the local
binary. The existing expression after-capture therefore describes the earlier
snapshot and must be refreshed before claiming all eight wire probes match.

The running local verification chain began before these edits. Its Rust,
Clippy and client stages cover the earlier aggregate-propagation snapshot;
its later `audit:local` stage builds current sources and will cover this new
conditional/arithmetic snapshot. A separate Rust/Clippy run verifies this step.

The conditional/arithmetic snapshot passed all 354 workspace Rust tests,
strict workspace/all-target Clippy and formatting. Wire comparison and full
client verification for this snapshot remain pending.

The integrated decimal-division Linux build subsequently passed all three
focused client tests for nested AVG, conditional/arithmetic AVG and division.
Its comparison in `artifacts/compatibility/decimal-integrated-comparison.json`
now matches all eight aggregate-expression reference captures exactly. This
supersedes the earlier expression after-capture's three recorded failures.
Decimal division now has a native adapter; see `docs/decimal-division.md`.
The prior native AVG snapshot completed remote verification with 353 Rust
tests, 363 client tests and 297 audit captures.

The numeric diagnostic correction now makes all seven captured AVG overflow
errors match SQL Server's diagnostic fields, including state 2 and canonical
message. Their missing pre-error result metadata remains unresolved. The newer
capture is `artifacts/compatibility/decimal-diagnostic-comparison.json`; see
`docs/decimal-division.md` for verification details and snapshot boundaries.
