# NTILE

NTILE participates in ranking signature checks and BIGINT result inference.
It requires one scalar argument, ordering, and no frame. A native ANY-input
validator preserves integer layouts, rejects noninteger inputs with 4110 and
nonpositive counts with 4116, and returns BIGINT without rounding or precision
loss. Named windows resolve before validation. Resolved same-query integer/BIT
column references in the bucket argument report 4195; scalar subqueries retain
their own scope.

Tests cover uneven distributions, more buckets than rows, partitions, BIGINT
counts up to the signed maximum, prepared invalid-count recovery, empty result
metadata, outer SUM/AVG and mixed character arithmetic. Native tests exercise
all integer layouts and NULLs across chunks, plus invalid types and counts.

Remaining gaps include complete correlated/source-column restrictions,
NULL-bucket SQL Server reference behavior, exact type-error message fidelity,
and live reference verification. Native NULL inputs currently remain NULL.

Reference: Microsoft [NTILE](https://learn.microsoft.com/en-us/sql/t-sql/functions/ntile-transact-sql).
