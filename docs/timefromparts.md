# TIMEFROMPARTS

`TIMEFROMPARTS(hour, minute, seconds, fractions, precision)` returns TIME with
precision 0–7. Precision accepts integer constants and supported constant expressions. Fractions count units of
10^-precision seconds, so precision zero requires a zero fraction. Hours are
0–23; minutes and seconds are 0–59. Invalid components report error 289.
NULL components return NULL; NULL or unsupported precision expressions report 10760.

The AST lowering converts the four components through the shared SQL integer
conversion path and selects a native function with a fixed scale. That function
reads bounded INTEGER vectors and writes exact TIME_NS values. No text or
floating-point intermediate is used to construct the time. Result inference
preserves the scale through source projections, views, aggregates, set queries
and DATEADD. Stored assignments still apply the target column's scale.

Verification covers all eight scales across 6000 rows, NULLs, component bounds,
single input evaluation, fractions down to 100 ns, integer conversion, prepared
calls, TRY/CATCH, default expressions, storage rounding and empty result metadata.
The local audit captures exact fractions separately from JavaScript milliseconds.

Remaining work includes SQL Server reference captures, exact diagnostics for
invalid precision/arity/modifiers, and broader conversion/error precedence.
A shared checked evaluator supports parentheses, unary signs, arithmetic and
bitwise integer precision expressions before runtime expression lowering.
Parameter-dependent and volatile expressions are rejected without evaluation.
Additional forms such as casts, out-of-range/overflow diagnostic precedence and
live SQL Server comparison remain open.

References:

- [Microsoft TIMEFROMPARTS reference](https://learn.microsoft.com/en-us/sql/t-sql/functions/timefromparts-transact-sql)
- [Microsoft error 10760 catalog](https://learn.microsoft.com/en-us/sql/relational-databases/errors-events/database-engine-events-and-errors-10000-to-10999)
- [Microsoft error 289 catalog](https://learn.microsoft.com/en-us/sql/relational-databases/errors-events/database-engine-events-and-errors-0-to-999)
- Inspected upstream `packages/engine/src/implicit.ts` and `engine.test.ts`
  DATEFROMPARTS/DATETIMEFROMPARTS validation and NULL coverage. No TIMEFROMPARTS
  implementation was found in the copied upstream packages.
