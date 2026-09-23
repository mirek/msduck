# Distribution windows

PERCENT_RANK and CUME_DIST share ranking signature checks: zero arguments,
mandatory OVER and ORDER BY, and no ROWS/RANGE frame. Named window inheritance
is expanded before validation. Errors use 174, 10753, 4112 and 4106 respectively.
They remain outside BIGINT ranking inference: DuckDB computes DOUBLE results,
which the existing wire encoder exposes as eight-byte FLOATN (SQL FLOAT(53)).

Tedious tests cover tied values, ascending/descending NULL ordering, partitions,
singleton and all-NULL partitions, empty-result metadata, outer aggregates,
prepared execution and invalid prepared windows. A diagnostic audit probe
captures values, metadata and missing-order diagnostics for future reference
comparison. No live SQL Server comparison has been run; broader floating-point
expression coercion and complete diagnostic fidelity remain unverified.
WINDOW clauses without FROM now reach named-window resolution and inherited
frame validation; both source-free and VALUES queries are covered.

The upstream mssqlite implementation has been inspected for related mappings;
its behavior is not used as SQL Server ground truth.

References: Microsoft [PERCENT_RANK](https://learn.microsoft.com/en-us/sql/t-sql/functions/percent-rank-transact-sql)
and [CUME_DIST](https://learn.microsoft.com/en-us/sql/t-sql/functions/cume-dist-transact-sql).
