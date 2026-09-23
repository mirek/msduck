# Statistical aggregates

STDEV and VAR map to DuckDB's sample standard deviation and variance;
STDEVP and VARP map to the population variants. Each returns an eight-byte
FLOATN wire value (SQL FLOAT(53)). NULL inputs are ignored. Empty/all-NULL
inputs return NULL; a single non-NULL value produces NULL for sample functions
and zero for population functions.

Calls use shared single-argument validation, ordinary aggregate/subquery nesting
checks, window nesting checks, and DISTINCT-with-OVER rejection. Known BIT
inputs, including catalog columns, CTE projections and prepared parameters,
report 8117. Grouping, ordered windows and explicit frames retain their AST
structure when the function name is translated. Ordinary DISTINCT calls first
collect a typed DISTINCT list, then apply the statistic with DuckDB's list
aggregate functions. This prevents implicit DOUBLE conversion from collapsing
separate BIGINT/DECIMAL inputs before deduplication. No argument is duplicated
by lowering. This implementation retains distinct values per group and needs
memory proportional to their count; a typed streaming aggregate could reduce
additional storage in a future implementation.

Tedious tests cover values and metadata, empty/singleton sets, decimal inputs,
prepared NULLs, grouping, ordered frames, empty frames, stored views, invalid
signatures, nesting, DISTINCT windows and BIT rejection. The differential audit
captures aggregate, singleton and exact-input DISTINCT results; no SQL Server reference endpoint is
configured yet. Floating-point results can differ in low bits across engines
and execution plans. Other invalid operand families, overflow diagnostics,
complete source-type inference and exact reference behavior remain unfinished.
The exact-input regression verifies two original values remain two inputs even
when both round to the same DOUBLE: sample functions return a non-NULL value
instead of incorrectly treating the input as a singleton. It does not establish
numerical accuracy for variance at large magnitudes or SQL Server's precise
floating-point computation order.

The inspected mssqlite source recognizes these names in grouping classification,
but supplies no implementation mapping reused here.

References: Microsoft [STDEV](https://learn.microsoft.com/en-us/sql/t-sql/functions/stdev-transact-sql),
[STDEVP](https://learn.microsoft.com/en-us/sql/t-sql/functions/stdevp-transact-sql),
[VAR](https://learn.microsoft.com/en-us/sql/t-sql/functions/var-transact-sql),
[VARP](https://learn.microsoft.com/en-us/sql/t-sql/functions/varp-transact-sql),
and DuckDB [aggregate functions](https://duckdb.org/docs/stable/sql/functions/aggregates).
