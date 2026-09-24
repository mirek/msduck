# OFFSET / FETCH

Ordered paging accepts constant, declared-variable, bound-parameter, arithmetic
and scalar-subquery counts. A token-level parser extension parses FETCH counts
as expressions, then restores the original expression AST before validation and
translation. Quoted identifiers and string literals are left intact.
Normalization runs from inner FETCH clauses outward, so a count's scalar
subquery can itself contain paging with bound or arithmetic counts. Restored
trees retain parameter traversal order across both levels and batch statements.

Integer OFFSET counts must be nonnegative (10742); FETCH counts must be positive
(10744). NULL counts fail explicitly instead of becoming an absent limit/offset.
Prepared queries can be reused after invalid counts. OFFSET/FETCH requires ORDER
BY, FETCH requires OFFSET, and combining TOP with paging in the same query fails
with 10741. TOP and paging share native count-validation infrastructure.

Tests cover page contents, OFFSET alone, empty pages, set-operation paging,
prepared reuse, arithmetic/subquery counts, quoted text, TRY/CATCH and structural
validation, plus nested FETCH counts and recovery after inner count errors.
These behaviors follow Microsoft's [ORDER BY reference](https://learn.microsoft.com/en-us/sql/t-sql/queries/select-order-by-clause-transact-sql)
and [error catalog](https://learn.microsoft.com/en-us/sql/relational-databases/errors-events/database-engine-events-and-errors-10000-to-10999).

Noninteger count typing/conversion, exact NULL error parity, all structural
error diagnostics, correlated count subqueries, optimizer evaluation timing and live differential validation
remain unfinished. FETCH PERCENT/WITH TIES and cursor FETCH are unsupported.
