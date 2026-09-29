# NTILE

NTILE participates in ranking signature checks and BIGINT result inference.
It requires one scalar argument, ordering, and no frame. A native ANY-input
validator preserves integer layouts, rejects the tested noninteger inputs and
nonpositive counts with 4116, and returns BIGINT without rounding or precision
loss. Named windows resolve before validation. Resolved same-query integer/BIT
column references in the bucket argument report 4195; scalar subqueries retain
their own scope.

Tests cover uneven distributions, more buckets than rows, partitions, BIGINT
counts up to the signed maximum, prepared invalid-count recovery, empty result
metadata, outer SUM/AVG and mixed character arithmetic. Native tests exercise
all integer layouts and NULLs across chunks, plus invalid types and counts.

The [pinned SQL Server 2025 capture](../reference/ntile-null.json) records 18
ordered requests in each of two fresh databases; a second independent
container replayed both runs exactly. The
[capture script](../scripts/capture-ntile-null.mjs) pins fixture SHA-256
`ff22ae5d5e5b5e8c2fd8fc3abfe2fecafae36c13e7db5dcedea7459ac7c6dfb1`
and checks rows, descriptors, number/state/class/message, event order and raw
DONE words. Literal, cast INT/BIGINT and empty-result NULL bucket inputs all
fail before result metadata with 4116/state 1/class 15. A scalar subquery or
RPC parameter resolving to NULL emits BIGINT metadata before the same error;
prepared RPC calls recover and return buckets 1, 1, 2 when rebound to 2.
Zero and negative literal counts use the same 4116 diagnostic. A same-query
column reference instead fails with 4195 before evaluating its NULL rows.

The [type matrix](ntile-types-reference.md) shows that SQL Server rejects even
integral DECIMAL and numeric-looking character bucket expressions with 4116.
The native validator uses that same 4116 message for noninteger and NULL inputs
instead of returning a different type error or NULL; a 6,000-row native test
covers NULL validity past a chunk boundary. The public client replay now compares the retained rows, column type/width/flags
and complete number/state/severity/message tuples directly. Literal/typed NULL,
zero and negative constants fail before metadata, including empty input;
parameters and scalar subqueries stay runtime inputs with metadata before error.
The engine converts only canonical native NTILE errors to 4116/state 1/class 15,
and source-column rejection to the captured 4195 message/class.

This does not establish full token parity. The retained ORDER/INFO/DONE event
sequences and raw command words remain separate evidence; the client property
comparison above does not claim they all match.

Remaining gaps include complete correlated/source-column restrictions,
exact type-error timing/severity and other unprobed bucket expressions.

Reference: Microsoft [NTILE](https://learn.microsoft.com/en-us/sql/t-sql/functions/ntile-transact-sql).
