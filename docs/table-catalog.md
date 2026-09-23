# Table and view catalog extensions

sys.tables and sys.views expose the corresponding user objects from sys.objects,
including the same IDs, schema links and timestamps. Internal catalog objects are
excluded. Table max_column_id_used reads the persistent column counter, retaining
the highest ID after a column is dropped. Transaction rollback and restart
preserve this value. Prepared queries observe later DDL.

The views expose the documented SQL Server table/view extension columns through
ledger metadata (excluding Azure SQL Edge-only columns). Boolean fields use BIT,
numeric IDs use INT, and the small feature codes use TINYINT, including empty
result sets. Ordinary objects report disabled replication, CDC, FILESTREAM,
memory optimization, temporal, graph and ledger flags. These features are not
implemented. Lock escalation, durability and ANSI NULL flags currently describe
the ordinary-object defaults, without implementing SQL Server locking or table
option changes. View options remain rejected by the existing validation.

FILESTREAM data-space IDs are NULL for these ordinary tables.
Physical LOB placement is not tracked: lob_data_space_id returns a typed NULL
rather than claiming a filegroup. Temporal retention fields also return typed
NULL placeholders. Catalog text widths, nullability, inherited type codes and
legacy DATETIME descriptors still need exact SQL Server emulation. Metadata
permissions are not enforced. Exposing these views does not implement the
advanced table/view features represented by their columns.

The client test covers object filtering, schema joins, dropped-column high-water
values, transactional table/view changes, prepared lookups, recreation and empty
numeric/boolean descriptors. A native persistence test verifies the high-water
value before and after adding a column following restart. The audit captures
local behavior without claiming a comparison against SQL Server.

References: [sys.tables](https://learn.microsoft.com/en-us/sql/relational-databases/system-catalog-views/sys-tables-transact-sql),
[sys.views](https://learn.microsoft.com/en-us/sql/relational-databases/system-catalog-views/sys-views-transact-sql).

The upstream catalog join test in packages/server/src/server.test.ts and the
derived views in packages/catalog/src/schema.ts informed this implementation.
The upstream live MAX(column_id) calculation is replaced with the persisted
high-water counter; its sys.views field list is corrected against Microsoft docs.
