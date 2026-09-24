# User object catalog foundation

`sys.objects` exposes persistent IDs for user tables and views in supported user
schemas. It includes the standard base columns: name, object_id, principal_id,
schema_id, parent_object_id, type, type_desc, create_date, modify_date,
is_ms_shipped, is_published and is_schema_published. Tables use U/USER_TABLE;
views use V/VIEW. Internal DuckDB and msduck allocation/catalog objects are hidden.

CREATE, ALTER and DROP update the registry within the same DDL transaction.
Rollback restores objects and IDs; drop followed by recreation allocates a new
ID. ALTER retains the ID and updates modify_date. Startup backfills older tables
and views and retains existing registry IDs. Historical creation dates for
backfilled objects cannot be reconstructed and use the backfill time.
SELECT INTO registers its table before row population, preserving its existing
two-step semantics: a failed population can leave an empty registered table in
autocommit mode. Explicit rollback removes both.

OBJECT_ID accepts one-/two-part names and an optional U/V type filter. Quoted
identifiers are parsed structurally. OBJECT_NAME and OBJECT_SCHEMA_NAME accept
one ID in the current database and expose NVARCHAR(128) for known expressions,
including NULL/empty results. Missing/malformed names, mismatched type filters,
and invalid IDs return NULL. Prepared queries observe subsequent DDL and
transactional catalog changes. Lookup maps keep arguments outside the catalog
aggregate; a native volatile-input test verifies per-row evaluation, including
predicate placement across the catalog subquery's cross product.

This remains a partial object catalog. Constraint objects, sequences, procedures,
functions, triggers and system objects remain open;
[sys.columns](columns.md) now exposes table declarations and live column state;
[sys.tables/sys.views](table-catalog.md) now expose ordinary object extensions; [column lookups](columns.md) have a persistent ID foundation. Principal ownership and metadata visibility are not enforced;
principal_id is NULL (schema ownership), parent_object_id is 0, and replication/
system flags are false for these user objects. Cross-database lookup arguments
are unsupported. Catalog name/type/type_desc widths, legacy DATETIME descriptors,
nullability and full collation behavior still need exact SQL Server emulation.
Type codes currently use unpadded text to retain existing predicate behavior.
Timestamp generation follows backend clock/transaction semantics. ALTER INDEX,
object transfer/rename and their modification-date rules remain unfinished.

References: [sys.objects](https://learn.microsoft.com/en-us/sql/relational-databases/system-catalog-views/sys-objects-transact-sql),
[OBJECT_ID](https://learn.microsoft.com/en-us/sql/t-sql/functions/object-id-transact-sql),
[OBJECT_NAME](https://learn.microsoft.com/en-us/sql/t-sql/functions/object-name-transact-sql).
