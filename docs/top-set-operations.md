# TOP in set operations

TOP is now translated independently in each SELECT branch of UNION, UNION ALL,
INTERSECT and EXCEPT. Each affected branch receives its own query scope before
TOP lowers to a backend limit. The outer ORDER BY stays on the combined result;
existing subqueries retain their own ordering and limits.

This follows Microsoft's [TOP processing-order explanation](https://learn.microsoft.com/en-us/sql/t-sql/queries/top-transact-sql).
A final set-operation ORDER BY does not determine which unordered rows a branch's
TOP selects. Tests therefore use identical rows within unordered sources and
verify counts and branch membership rather than assuming an insertion order.

Tedious coverage includes zero and nonzero limits, multiple/nested set-operation
branches, duplicate removal, INTERSECT/EXCEPT, prepared limit parameters, SELECT
INTO counts and stored views. TOP PERCENT and WITH TIES remain explicit unsupported
forms, including when they occur in set-operation branches. Complete TOP argument
validation/conversion, exact diagnostics and all DML TOP forms remain unfinished.

Integer TOP counts now pass through a shared native validator. NULL and negative
counts raise 1014; zero returns no rows. NULL is replaced by an invalid sentinel
before validation so DuckDB's NULL propagation cannot turn it into an unlimited
query. Prepared tests verify recovery after invalid counts, TRY/CATCH, ordinary
positive limits and stored views. TOP combined with OFFSET/FETCH in the same
query scope is rejected instead of replacing the existing limit clause.

Noninteger count binding/conversion and error 1060 remain unfinished; backend
coercion still applies before native count validation. Full evaluation timing
and live SQL Server differential validation remain unverified. The invalid-count
number follows Microsoft's [error catalog](https://learn.microsoft.com/en-us/sql/relational-databases/errors-events/database-engine-events-and-errors-1000-to-1999).
