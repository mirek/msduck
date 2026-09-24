# Exact decimal division

`reference/decimal-division.json` records 16 live SQL Server probes with image
and server version, exact VARCHAR renderings, metadata, errors and completion
events. They cover repeating positive and negative fractions, scales 0, 2, 6,
20 and 38, full-width coefficients, overflow, division by zero, and NULLs.

The captures establish truncation toward zero at the result scale. For example,
DECIMAL(5,2) division produces DECIMAL(13,8); 2/3 is 0.66666666 and -2/3 is
-0.66666666. The DECIMAL(38,6) examples produce scale 6 and truncate similarly.
The metadata agrees with Microsoft's
[precision and scale rules](https://learn.microsoft.com/en-us/sql/t-sql/data-types/precision-scale-and-length-transact-sql?view=sql-server-ver17).

The deterministic `msduck_core::decimal_arithmetic::divide` function implements
the coefficient operation with explicitly supplied input and output declarations.
It uses unsigned big integers for intermediates and applies the sign afterward.
Validated SQL declarations bound intermediates to at most 114 decimal digits;
the final coefficient must fit the declared precision. `num-bigint` 0.4.8 was
already in Cargo.lock through the backend dependency graph and is now an
explicit core dependency. The pure test loop still requires no DuckDB build.

The core separates division by zero from result overflow. It does not choose
SQL result declarations, evaluate NULLs, apply session arithmetic options or
emit TDS diagnostics. The reference sends metadata before errors, reports
numeric overflow as 8115/state 2 and division by zero as 8134/state 1. NULL
operands yield NULL, including a NULL numerator divided by zero.

`src/decimal_division.rs` registers 39 scale-specific native scalar functions,
with explicit result precision and typed DECIMAL operands. This avoids a
combinatorial family of input-type overloads: callbacks inspect each operand's
precision and scale and read its matching physical coefficient width. NULLs
propagate before testing the divisor. SQL lowering wraps the callback in the
inferred result declaration and coerces supported integer/currency operands
without evaluating an input more than once.

The shared logical inference now includes decimal division, so AVG over a
decimal quotient also selects the exact aggregate. Dynamic operands whose
logical type is still unknown may fall through to the backend; this is not
complete SQL expression typing or arithmetic-session-option support.

Three native tests cover all four physical coefficient widths, multi-vector
inputs, NULL divided by zero, signs, result precision overflow, invalid input
binding, single evaluation of both operands, SQL lowering and SELECT INTO
storage declarations. Three focused Linux client tests pass for division and
the preceding conditional/nested AVG work.

`artifacts/compatibility/decimal-integrated-comparison.json` contains the
integrated Linux reference comparisons. All 13 successful division queries now
match in decimal values, exact rendered text and division-column metadata.
Only two complete captures match: the eleven non-NULL successes still differ
in CONVERT-to-VARCHAR flags (1 rather than 33). The three error cases now fail
with the expected error numbers; they lack metadata before the error. Numeric
overflow still has a DuckDB message prefix and state 1 rather than 2.

All eight aggregate-expression reference probes now match completely, including
the previously failing arithmetic, COALESCE and CASE inputs. The original
29-query AVG matrix retains the documented formatting, warning and diagnostic
differences. The original floating-point division baseline remains preserved
in `artifacts/compatibility/decimal-division-before.json`.

The arithmetic-core snapshot passed all 356 Rust tests, strict Clippy and
formatting. Full verification of the integrated snapshot is pending separately.

The integrated implementation passed all 359 workspace Rust tests, strict
workspace/all-target Clippy and formatting on macOS. The complete Linux
client/audit verification remains running for its frozen integrated snapshot.

## Numeric diagnostic correction

The shared numeric diagnostic classifier now recognizes the canonical numeric
expression-overflow message as 8115/state 2. The root adapter removes only its
known DuckDB envelope; explicit typed application errors retain their original
identity and text. Native session tests compare the emitted error token for
both scalar division and DECIMAL AVG overflow.

`artifacts/compatibility/decimal-diagnostic-comparison.json` is the newer macOS
capture. All diagnostic fields match SQL Server in its three division errors
and seven AVG overflow cases. Those cases still lack result metadata before
the error, so their complete captures remain different. The two successful
NULL division captures and all eight aggregate-expression captures match
completely. The three focused client tests also pass, with assertions for the
canonical numeric message and state 2.

All 359 workspace Rust tests, strict Clippy and formatting passed for this
correction. The earlier aggregate-declaration macOS snapshot completed its
364 client tests; its following audit rebuilt newer sources and is still
running. The remote full verification remains on the preceding division
snapshot, before this diagnostic correction.

The macOS audit completed 301 cases. Its raw comparison against the preceding
297-case Linux AVG baseline preserves 295 unchanged cases, four new decimal
cases, a row-order reversal in unordered derived-table APPLY, and corrected
DECIMAL metadata for a numeric-literal division (previously FLOAT). See
`artifacts/compatibility/decimal-integrated-audit-diff.json`. No ordering
normalization was applied.
