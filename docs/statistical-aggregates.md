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
The aggregate warning observer checks the original value before DISTINCT
deduplication. With ANSI_WARNINGS ON, eliminated NULLs produce one 8153
information token per statement; duplicate non-NULL values and empty inputs
do not. ANSI_WARNINGS OFF suppresses that warning. The four no-argument
errors use their uppercase SQL Server function names.

Tedious tests cover values and metadata, empty/singleton sets, decimal inputs,
prepared NULLs, grouping, ordered frames, empty frames, stored views, invalid
signatures, nesting, DISTINCT windows and BIT rejection. The differential audit
captures aggregate, singleton and exact-input DISTINCT results; it records
local evidence rather than a SQL Server parity verdict.

The pinned SQL Server 2025 capture in `reference/statistical-aggregates.json`
retains 35 cases from two independent containers and fresh databases. Run
`node scripts/capture-statistical-reference.mjs --check` to verify the retained
fixture against the SHA-256 pinned in the script. A fresh run compares full
rows, typed descriptors, errors, token order and raw DONE status words against
that fixture. Every one of the four functions returns an eight-byte nullable
FLOATN descriptor, including empty and all-NULL aggregates. Sample functions
return NULL for a singleton; population functions return zero. BIT inputs
raise 8117/state 1/class 16 before metadata, no-argument calls raise
174/state 1/class 15, and DISTINCT with OVER raises 10759/state 1/class 15.
An empty preceding frame returns NULL for all four, while a one-row preceding
frame returns NULL for sample functions and zero for population functions.

Two distinct DECIMAL(20,0) inputs, 9007199254740992 and 9007199254740993,
produce zero for all four DISTINCT results on this SQL Server build. The sample
results remain non-NULL, proving that input identity survives deduplication
even though floating-point computation collapses the numerical difference.
Floating-point results can differ in low bits across engines and execution
plans. Other invalid operand families, overflow diagnostics, complete
source-type inference and exact reference behavior remain unfinished.
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
