# SQL Server sequence reference

[`reference/sequence-reference.json`](../reference/sequence-reference.json) retains 24 ordered raw TDS observations from the pinned SQL Server 2025 RTM-CU7 image (`17.0.4065.4`). The capture ran in two independent containers and fresh databases, with a separate second client connection for cross-session checks. The fixture includes result descriptors, rows, errors, information messages, return status and DONE-family tokens. `node scripts/capture-sequence-reference.mjs --check` verifies the case list, capture shape and observed invariants without starting SQL Server. A fresh capture writes a separate ignored artifact and refuses to overwrite the retained fixture; `--write-fixture` creates it only if absent.

The finite `BIGINT` sequence starts at 10, increments by 2 and ends at 16. Its
first call returns 10. Two references to the same sequence in one row both
return 12 and consume **one** value. An ordered two-row query returns 14, then
16. A further call emits a `BIGINT` result descriptor before error 11728,
state 1, class 16, followed by a final DONE with a null row count; no row is
emitted. The event trace preserves COLMETADATA, ERROR and DONE in that order.
`sys.sequences.current_value` remains 16 and `is_exhausted` becomes true. The
catalog's start, increment, bounds and current value columns have native
`SQL_VARIANT` wire descriptors. Separate `SQL_VARIANT_PROPERTY(..., 'BaseType')`
results confirm their embedded `bigint` or `smallint` type; the decoded outer
values alone do not carry this information. A descending `SMALLINT` sequence
returns 0, -1, -2, then reports the same error with a `SMALLINT` descriptor
and the same event order.

After `ALTER SEQUENCE ... RESTART WITH 10`, a call inside a transaction returns 10. Rolling the transaction back does not restore that allocation: a second connection receives 12 and the first receives 14. `ALTER SEQUENCE ... RESTART WITH 16 CYCLE` returns 16, then wraps to the configured minimum 10. The catalog capture records `start_value` as 16 after that restart, `current_value` as 10 after the wrap, and the `is_cycling`, `is_cached` and `is_exhausted` flags. All probes use `NO CACHE`; they do not establish crash or cache-loss behavior. Dropping both sequences removes their catalog rows, and the first connection remains usable after exhaustion errors.

Microsoft's [CREATE SEQUENCE documentation](https://learn.microsoft.com/en-us/sql/t-sql/statements/create-sequence-transact-sql?view=sql-server-ver17) states that numbers are consumed outside transaction rollback, and its [sequence-number guide](https://learn.microsoft.com/en-us/sql/relational-databases/sequence-numbers/sequence-numbers?view=sql-server-ver17) describes the one-value-per-row behavior for repeated references. The raw capture pins the precise rows, descriptors and completion sequence for this image. It does not prove every supported placement, data type, cache option, concurrency interleaving or diagnostic.

msduck currently uses private DuckDB sequences for identity allocation; it does not expose SQL Server `CREATE SEQUENCE`, `NEXT VALUE FOR`, `ALTER SEQUENCE`, `DROP SEQUENCE` or `sys.sequences` as a compatible public feature. Implementing this fixture requires SQL Server syntax and declaration binding, database-wide non-rollback allocation, statement-row allocation sharing, transactional sequence DDL, typed catalog rows and exact TDS errors. DuckDB `nextval` alone does not establish those contracts.
