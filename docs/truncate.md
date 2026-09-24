# TRUNCATE TABLE

Whole-table TRUNCATE validates one table target, verifies that it is a user
table, checks incoming foreign keys, and executes native DuckDB truncation.
The metadata check and mutation share one transaction snapshot. Autocommit
failures roll back the internal transaction; explicit transactions remain under
caller control. Completion carries no deleted-row count and @@ROWCOUNT resets
to zero. The table, column defaults and ordinary constraints remain in place.

Foreign keys from another table prevent truncation even if the referencing
table is empty; this reports error 4712. The referencing child can be truncated.
Partition syntax, multiple targets, cascade, identity options and backend-only
modifiers are explicitly rejected. A view is not a valid target.

Tedious tests cover rollback, empty results, retained defaults, continued use,
foreign-key parents/children, empty-child restrictions, view rejection and
unsupported options without data loss. A separate local probe established that
DuckDB rejects a self-referencing table containing parent/child rows. SQL Server
permits that case, so self-referencing truncation remains an observed gap.

Other remaining work includes partition truncation, indexed-view
restrictions, temporal/replication/edge restrictions, permission checks, exact
missing-object diagnostics and SQL Server concurrency/token comparisons.
DuckDB's physical deletion/storage behavior does not emulate SQL Server page
allocation or logging. This is not full TRUNCATE compatibility.

Integer IDENTITY resets to its original seed, including exhausted BIGINT
allocators. The column default, private sequence and stored definition are
replaced within the truncation transaction. Rollback restores the old allocator;
commit retains the new one. Ordinary failed/rolled-back inserts still consume
values on the active allocator. IDENT_CURRENT returns the seed immediately after
reset, while IDENT_SEED/IDENT_INCR retain their original values. Prepared inserts
resolve the replacement default on execution. Tests cover rollback, commit,
negative increments, repeated cleanup, database reopen, failed inserts, and
incoming foreign-key rejection before mutation. Older identity tables without
stored seed/increment definitions cannot yet be reset.

The mssqlite interpreter snapshots identity state for transactional resets;
msduck obtains restoration through DuckDB transactional catalog replacement.

Reference: [Microsoft TRUNCATE TABLE](https://learn.microsoft.com/en-us/sql/t-sql/statements/truncate-table-transact-sql).
