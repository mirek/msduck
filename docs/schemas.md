# User schemas

Standalone CREATE SCHEMA name creates a DuckDB schema in the current database.
Table and view names can then use that schema; identical object names in two
schemas remain distinct. The default schema remains dbo. Creation and dropping
participate in explicit transactions and persist with file-backed storage.

DROP SCHEMA [IF EXISTS] name requires an empty schema. Cascade and other
non-T-SQL drop options are rejected before execution. Built-in names dbo, sys,
information_schema and guest are protected, as are backend main/temp schemas.
CREATE SCHEMA requires its own batch. AUTHORIZATION, embedded schema elements,
IF NOT EXISTS/OR REPLACE and backend-specific create options remain unsupported
and are rejected rather than treated as successful ownership changes.

Client tests verify schema-qualified tables/views, same-named objects, empty
schema drops, IF EXISTS, rollback, unsupported ownership, invalid placement and
connection recovery. The Rust restart test persists a user schema, altered
table, added-column default and altered view across reopening.

This implements namespaces and an initial schema catalog. Remaining work
includes principals/ownership, grants, user default schemas, atomic
embedded CREATE SCHEMA elements, ALTER SCHEMA transfers, exact errors and
SQL Server differential verification. Schema resolution currently follows
DuckDB rules with dbo as the session default.

References: [CREATE SCHEMA](https://learn.microsoft.com/en-us/sql/t-sql/statements/create-schema-transact-sql),
[DROP SCHEMA](https://learn.microsoft.com/en-us/sql/t-sql/statements/drop-schema-transact-sql).


`sys.schemas` now exposes `name`, `schema_id` and `principal_id`, backed by a
persistent private registry. The four foundational built-ins use dbo=1, guest=2,
INFORMATION_SCHEMA=3 and sys=4. User IDs allocate above that range, are unique,
and survive restart; CREATE/DROP maintenance shares the schema DDL transaction.
Startup backfills user schemas from older databases. Internal DuckDB main/temp
names are excluded. A failed drop of a nonempty schema retains its registry row.
The public view cannot be replaced or dropped through the T-SQL DDL path.

`SCHEMA_ID([name])` and `SCHEMA_NAME([id])` read the current transaction's registry.
Omitted arguments use the server's current dbo default; explicit NULL, absent
names and invalid IDs return NULL. SCHEMA_NAME returns NVARCHAR(128) metadata
for known direct expressions, including NULL/empty results; SCHEMA_ID is INT.
Prepared queries see subsequent DDL. Names compare case-insensitively; full
collation semantics remain pending. A 6,000-row native test checks that a
volatile input is evaluated only once per row.

This is not the complete SQL Server schema/security catalog. Built-in role-owned
schemas are not yet seeded. New user schemas currently record dbo ownership;
AUTHORIZATION and user-specific defaults remain unsupported. Direct sys.schemas
name projections still inherit the backend's unbounded character descriptor,
and exact catalog nullability/permission metadata remains unfinished. The initial [object catalog](objects.md) covers user tables and views; column
catalogs now include [sys.columns](columns.md) and [sys.identity_columns](identity.md),
with their documented limits.

References: [sys.schemas](https://learn.microsoft.com/en-us/sql/relational-databases/system-catalog-views/schemas-catalog-views-sys-schemas),
[SCHEMA_ID](https://learn.microsoft.com/en-us/sql/t-sql/functions/schema-id-transact-sql),
[SCHEMA_NAME](https://learn.microsoft.com/en-us/sql/t-sql/functions/schema-name-transact-sql).
