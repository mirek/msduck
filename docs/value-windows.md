# FIRST_VALUE and LAST_VALUE

These functions require one scalar argument and OVER with ordering. Named
windows resolve before validation. SQL Server's external IGNORE NULLS or
RESPECT NULLS modifier moves inside the DuckDB argument list. Omitted treatment
retains the backend's default RESPECT NULLS behavior; the argument is not copied
or evaluated by a separate emulation expression.

Known integer and BIT input types propagate through the shared inference pass,
including catalog columns, prepared values, supported derived/CTE projections
and VALUES sources. Integer arithmetic and outer SUM/AVG can therefore preserve
widths and exact values. Direct native results retain their backend type, with
the existing limitations of general character/collation descriptors.

Tests cover leading/intermediate/trailing NULLs, default running frames,
whole-partition and suffix frames, exact BIGINT values, integer widths, BIT,
prepared NULL inputs, scalar subqueries, outer aggregates and nesting errors.
Broader character metadata, collations, compatibility-level gating and other
analytic functions still require implementation and reference validation.

References: Microsoft [FIRST_VALUE](https://learn.microsoft.com/en-us/sql/t-sql/functions/first-value-transact-sql)
and [LAST_VALUE](https://learn.microsoft.com/en-us/sql/t-sql/functions/last-value-transact-sql).

## LAG and LEAD

LAG/LEAD share argument-type inference and NULL-treatment translation. They
accept one to three scalar arguments and require ordering; frames are rejected.
Explicit offsets convert to BIGINT using integer conversion and a native helper
rejects negative values with 8730. Zero retains the current row. The helper
preserves NULLs, traverses vector chunks safely and evaluates its argument once.
Known integer/BIT defaults explicitly convert to the first argument's type,
including truncation and bounded overflow, instead of widening the result.

Tests cover default and explicit offsets, IGNORE/RESPECT NULLS, zero, negative
prepared offsets and recovery, exact BIGINT, narrow integer defaults, BIT,
outer aggregates, and native vector/single-evaluation behavior. General
character/decimal/date default conversion and NULL-offset reference behavior
remain unverified. References: Microsoft
[LAG](https://learn.microsoft.com/en-us/sql/t-sql/functions/lag-transact-sql),
[LEAD](https://learn.microsoft.com/en-us/sql/t-sql/functions/lead-transact-sql),
and [error 8730](https://learn.microsoft.com/en-us/sql/relational-databases/errors-events/database-engine-events-and-errors-8000-to-8999).
