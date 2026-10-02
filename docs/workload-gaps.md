# Workload compatibility gaps (v0.2.5)

An mssql/tedious application workload, run against v0.2.4, found gaps in
fixture setup, schema installation, programmable objects, catalogs, queries
and transactions. Issue #710 tracks them. Each area below is now implemented
in its own feature module (see [extension hooks](extension-hooks.md)), and
each has its own page. Every page is checked against SQL Server captures and
lists its remaining limits.

| Area | Page | Main remaining limits |
| --- | --- | --- |
| Application locks (`sp_getapplock`, `sp_releaseapplock`, `APPLOCK_MODE`, `APPLOCK_TEST`) | [gaps-applock](gaps-applock.md) | No `SET LOCK_TIMEOUT`. A cancel during an infinite wait is not read. Locks don't appear in `sys.dm_tran_locks`. |
| `BACKUP DATABASE`, `RESTORE HEADERONLY/FILELISTONLY/DATABASE`, msdb history, `sys.database_files`/`sys.master_files` | [gaps-backup](gaps-backup.md) | Full backups to one DISK device only. msduck's own file format, not `.bak`. A failed BACKUP/RESTORE ends the batch. |
| Keys and indexes on nvarchar/datetimeoffset, single-NULL UNIQUE, `CLUSTERED`/`INCLUDE`/filtered indexes | [gaps-keys](gaps-keys.md) | Keys compare case-sensitively. No `IGNORE_DUP_KEY`. |
| `ALTER TABLE` constraint lifecycle, foreign-key `CASCADE`/`SET NULL`/`SET DEFAULT` | [gaps-constraints](gaps-constraints.md) | A failed UPDATE/DELETE leaves an explicit transaction uncommittable. Cascades don't fire triggers. |
| `rowversion`/`timestamp`, decimal identity, `SCOPE_IDENTITY`, `SET IDENTITY_INSERT` | [gaps-rowversion_identity](gaps-rowversion_identity.md) | MERGE and bulk updates don't assign new rowversions. |
| Computed columns over nvarchar(max)/JSON, session-function defaults | [gaps-computed](gaps-computed.md) | Errors in a computed expression during a write are reported as 50000. |
| Stored procedures, `EXEC (string)`, `sp_executesql` with OUTPUT | [gaps-procedures](gaps-procedures.md) | Error tokens carry no procedure name or line. No numbered procedures. |
| RPC procedure calls and RPC OUTPUT parameters | [gaps-rpc-procedures](gaps-rpc-procedures.md) | Some completion command values differ. |
| Scalar, inline and multi-statement table-valued functions | [gaps-functions](gaps-functions.md) | Loops or recursion with column arguments are refused. No table variables inside functions. |
| DML triggers, `DISABLE`/`ENABLE TRIGGER` | [gaps-triggers](gaps-triggers.md) | MERGE doesn't fire triggers. A RAISERROR inside a trigger, under the caller's TRY, gives 3609. |
| MERGE, including table hints | [gaps-merge](gaps-merge.md) | `sp_prepare` rejects MERGE. A CHECK violation ends the batch. |
| UPDATE/DELETE with outer joins or APPLY in the target tree | [gaps-outer_dml](gaps-outer_dml.md) | TOP in UPDATE/DELETE is unsupported. |
| FOR JSON AUTO, JSON_MODIFY, ordered STRING_AGG, STRING_SPLIT, HASHBYTES | [gaps-json_string](gaps-json_string.md) | `ROOT` without a name doesn't parse. |
| Explicit COLLATE, styled CONVERT, FORMAT, SERVERPROPERTY, DATABASEPROPERTYEX, ROWCOUNT_BIG | [gaps-conversion](gaps-conversion.md) | Default comparisons stay case-sensitive. Errors that depend on row values are reported as 50000. |
| Comparisons, LIKE, ordering and conversions over nvarchar/nchar columns | [gaps-unicode-predicates](gaps-unicode-predicates.md) | GROUP BY/DISTINCT don't ignore trailing spaces. |
| `#temp`/`##temp` tables and table variables | [gaps-temp_tables](gaps-temp_tables.md) | Temp objects stay in the database where they were created. |
| Isolation levels, `SAVE TRANSACTION`, `WAITFOR` | [gaps-transactions](gaps-transactions.md) | Every level runs as DuckDB snapshot isolation, with no locks. No DDL after a savepoint. |
| Bulk load (`INSERT BULK`, BulkLoadBCP) | [gaps-bulk](gaps-bulk.md) | Views can't be bulk targets. `SCOPE_IDENTITY` is unchanged when identity values are kept. |
| Contextual identifiers (`offset`, `at`, ...), database-qualified errors | [gaps-identifiers](gaps-identifiers.md) | T-SQL reserved words are not rejected with 156. |
| Constraint, module and file catalogs, OBJECT_DEFINITION, sp_pkeys/sp_fkeys/sp_rename | [gaps-catalog](gaps-catalog.md) | See its page. |
| Constraint-backed rows in `sys.indexes`/`sys.index_columns` | [index catalog](index-catalog.md) | WITH options such as `fill_factor` show their defaults. |

Transactions still run on DuckDB snapshot isolation. Accepting a lock hint or
an isolation level is not the same as SQL Server's locking semantics. The
workload's multi-connection concurrency behaviour still needs a reference
comparison.
