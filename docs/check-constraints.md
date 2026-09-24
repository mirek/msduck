# CHECK constraints

CREATE TABLE column and table CHECK expressions use the shared search-condition
validator. Bare BIT/numeric values and invalid logical operands are rejected
with 4145 before any statement in the batch executes.

DuckDB stores and enforces accepted CHECK expressions. A false result rejects
the write; UNKNOWN from NULL is allowed. Native CHECK violations map to SQL
Server error 547. Multi-row INSERT and UPDATE failures preserve the original
rows, prepared statements can execute again after a failed write, and TRY/CATCH
receives the mapped error number. Explicit transaction rollback restores valid
updates. Driver tests cover both named column and named table constraints.

This follows Microsoft's documented [CHECK semantics](https://learn.microsoft.com/en-us/sql/relational-databases/tables/unique-constraints-and-check-constraints).
Remaining work includes ALTER ADD/DROP constraints, CHECK/NOCHECK and trust
state, constraint catalogs and exact names/messages, column-reference rules,
full type/collation behavior, and live differential validation. This coverage
does not establish SQL Server UNIQUE-constraint NULL semantics.
