# SELECT INTO

A top-level SELECT INTO now creates a persistent table and inserts the query's
rows. The source is translated and bound before table creation. Destination
columns use its bound names and DuckDB storage types, preserving integer widths,
decimal precision/scale and supported date/time types. Wildcards, empty sources,
CTEs, TOP and ordering use the existing query translation.
The destination can appear on the first SELECT of an outer set operation.
The complete UNION/UNION ALL, INTERSECT or EXCEPT source is bound before creating
columns, so combined backend types determine destination storage. INTO on another
branch or a nested source is rejected.

Creation and insertion are separate operations, as described in Microsoft's
[SELECT INTO documentation](https://learn.microsoft.com/en-us/sql/t-sql/queries/select-into-clause-transact-sql).
If row insertion fails, the destination remains empty. Explicit transaction
rollback removes the newly created table. Existing destinations are not replaced.
The statement emits a completion row count without a result set. Preparation
binds/describes the source without creating the destination or evaluating rows.

Tedious tests cover typed and empty destinations, row counts, prepared execution,
existing-table rejection, conversion failure, transaction rollback, CTE/TOP,
and unnamed/duplicate output rejection. The Rust persistence test verifies the
created table and SMALLINT storage survive reopening.
Additional tests cover UNION ALL integer widening, UNION deduplication, empty
EXCEPT output, prepared set operations and invalid branch placement. Unnamed or
empty output aliases return error 1038; duplicate destination names return 2705
in execution and preparation. These numbers follow Microsoft's
[1000-series](https://learn.microsoft.com/en-us/sql/relational-databases/errors-events/database-engine-events-and-errors-1000-to-1999)
and [2000-series error catalogs](https://learn.microsoft.com/en-us/sql/relational-databases/errors-events/database-engine-events-and-errors-2000-to-2999).

This is partial support. Declared character lengths, full nullability and collation
metadata, identity transfer, full set-operation coercion, temporary targets,
filegroups, exact SQL Server diagnostics and cross-database destinations remain
unfinished. Ordinary expressions require explicit output aliases. The copied
mssqlite type-preservation audit also queries sys.columns, which remains a separate
catalog gap; successful table creation does not establish that audit's full parity.
