# Search-condition validation

SQL Server search conditions use predicates combined with AND, OR and NOT.
A BIT value is a scalar value, so it needs a predicate such as `flag = 1`;
`WHERE flag` and `IF @flag` are not valid substitutes.

Batch preflight checks IF/WHILE, SELECT WHERE/HAVING, ordinary join ON clauses,
UPDATE/DELETE filters, and searched CASE conditions before any statement runs.
CREATE TABLE column/table CHECK expressions receive the same validation.
It recognizes comparisons, IS NULL, IS DISTINCT FROM, IN, BETWEEN, LIKE,
quantified comparisons and EXISTS, with parentheses and logical combinations.
Each AND/OR/NOT operand must itself be a search condition. Invalid scalar
conditions report 4145, including inside prepared batches and skipped branches.
Simple CASE operands remain scalar values and are unaffected.

Tests cover BIT variables/columns, numeric constants, nested logical operands,
joins, HAVING, searched/simple CASE, DML unchanged after invalid filters,
batch preflight before INSERT, and prepared nullable BIT comparisons.

This follows Microsoft's [search-condition grammar](https://learn.microsoft.com/en-us/sql/t-sql/queries/search-condition-transact-sql).
It does not complete scalar-versus-predicate validation in every expression
position. ALTER constraint forms, filtered indexes, full-text/graph predicates,
other specialized contexts, operand conversion and collation remain unfinished.
Full live SQL Server differential validation is still required.
