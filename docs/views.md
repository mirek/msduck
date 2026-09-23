# Persisted query views

CREATE VIEW stores a translated query in DuckDB. CREATE OR ALTER VIEW maps to
an atomic CREATE OR REPLACE VIEW; a binding failure preserves the old view.
Explicit column names, bracketed identifiers, TOP, nested views and existing
query translations apply to the stored definition. Queries observe later base
table writes. DROP VIEW uses DuckDB DDL. Creation participates in transactions,
and definitions persist across database reopen.

Validation runs before any batch statements execute. View creation must be the
only top-level statement. Definitions cannot capture bound parameters or session
globals/error functions as constants. Unsupported view options fail explicitly;
SCHEMABINDING, ENCRYPTION, CHECK OPTION, temporary objects, INTO, cross-database
view names, and ORDER BY without TOP/OFFSET are not silently accepted. Declared
column lists are limited to 1024; checking expanded wildcard column counts and
all SQL Server column-name rules remains unfinished.

Tedious tests cover explicit columns, translated TOP, changed base rows, nested
views, CREATE OR ALTER creation/replacement, failed replacement preservation,
rollback, prepared SELECTs, dropping and invalid definition recovery. The
file-backed Rust restart test queries a persisted translated view after reopening.
These are local tests, not SQL Server differential validation.

Remaining work includes updatable/indexed views, catalog and
permission integration, dependency invalidation/refresh semantics, session SET
option capture, exact error numbers, full view-definition restrictions, and
SQL Server nesting limits. DuckDB dependency and binding behavior is currently
used and does not establish SQL Server equivalence.

The mssqlite reference transpiler emits CREATE VIEW with a translated SELECT;
its interpreter also records catalog objects and replaces views by dropping
them first. msduck uses DuckDB's atomic replacement; SQL Server catalogs remain
separate outstanding work.

Reference: [Microsoft CREATE VIEW documentation](https://learn.microsoft.com/en-us/sql/t-sql/statements/create-view-transact-sql).

## ALTER VIEW

ALTER VIEW shares the CREATE definition validator and query translator, but
requires an existing user view in the selected database. Missing names and
base tables are rejected. A bound DuckDB catalog lookup and replacement run
in one transaction snapshot. When no user transaction exists, the operation
opens and closes an internal transaction; otherwise the caller retains control.
Failed autocommit changes roll back, and successful changes inside a user
transaction can be rolled back by the caller.

Tests cover explicit renamed columns, unqualified dbo resolution, missing
names, wrong object types, failed binding, rollback, placement/parameter
validation, prepared query reuse and reopening an altered persisted view.
SQL Server permissions, schema locks, object identity/catalog records and
exact error codes remain unimplemented. Concurrent DDL behavior still needs
stress testing and a SQL Server oracle.

Reference: [Microsoft ALTER VIEW](https://learn.microsoft.com/en-us/sql/t-sql/statements/alter-view-transact-sql).
