# Table-owned index catalog

`reference/index-catalog.json` retains 27 first-party SQL Server catalog
snapshots captured identically in two fresh databases against the pinned image.
The explicit projections retain column descriptors, flags and values from
`sys.indexes` and `sys.index_columns`, together with each operation's raw
completion tokens, errors and session counters. Reproduce and compare with:

```sh
node scripts/capture-index-catalog.mjs
```

The script preserves both raw runs before comparison and never replaces the
checked-in fixture automatically. Object IDs are not projected as arbitrary
numeric constants: the drop/recreate program explicitly observes whether the
new object has a different identity. Index IDs, column IDs and ordinals remain
literal values in the snapshots.

Observed behavior includes:

- Heaps have index ID 0, NULL name, type 0 and description HEAP. The first
  ordinary nonclustered index has ID 2 and type 2. Index IDs belong to a table.
- The same logical index name can exist independently on different tables,
  including tables in different schemas and quoted names containing dots.
- A freed nonclustered index ID is reused: after dropping ID 2 while IDs 3 and 4
  remain, a replacement receives ID 2. Thus `(object_id,index_id)` alone is not
  an enduring backend identity for an operation bound before replacement.
- Creating and dropping indexes inside a transaction changes visible catalog
  rows immediately; rollback restores physical/logical state. Table-drop
  rollback also restores its indexes. Committed index drops remain absent.
- Dropping/recreating a table changes its object identity, removes its old index
  rows and starts the new nonclustered index numbering at 2.
- Reusing an index name on the same table emits 1913/state 1. Explicitly dropping
  a primary-key enforcement index emits 3723/state 4 and preserves it.
- Descending keys, INCLUDE columns, filtered definitions, disable/rebuild and
  named primary/unique constraints retain their distinct catalog flags and
  index-column ordinals. These captures are requirements, not implementation
  claims for those features.

The initial adapter is implemented in `src/index_catalog.rs`. Its persistent
identity mapping separates the logical table-owned index name and SQL Server index ID from a
qualified DuckDB backend name and a monotonically allocated incarnation.
`drop_index` verifies the complete expected identity before mutation, including
that incarnation. Tests prove stale drops do not delete replacements and
same-named indexes on different tables remain independent.

Creation/drop must mutate physical indexes and catalog rows under the same
transaction outcome. Standalone operations may own a transaction; operations
inside an explicit user transaction must preserve caller ownership and leave
rollback/error decisions to the engine's documented policy. Do not infer
transaction ownership from an attempted BEGIN or hide an error by committing.
The separate multi-target DROP binder requires earlier successful drops to
survive a later missing-target error when the caller has no explicit transaction.

This task owns `src/index_catalog.rs` and an isolated native integration test,
plus these captures/docs. The register/acquire/create/drop/sync APIs and four native regressions now exist.
They are not yet exported or wired into root startup/DDL execution. Root server initialization, engine DDL hooks, SQL
exports/dialect and manifests remain under their existing claims and must be
integrated separately. Unsupported index kinds must remain explicit; a partial
catalog must not be presented as complete SQL Server index compatibility.

`create` currently accepts ordinary ascending column indexes and unique integer
keys. It rejects INCLUDE, filters, descending keys and unimplemented options
before mutation. For unique integer keys, physical key expressions pair an
IS NULL discriminator with a zero-filled value: NULL compares equal to NULL
while remaining distinct from zero. Three additional reference programs confirm
that a second NULL fails with SQL Server error 2601; the original 24 programs
are unchanged. Exact runtime translation of this diagnostic remains engine work.
Other unique key types need their SQL Server comparison rules before support.

Transaction ownership is explicit (`Owned` or `CallerOwned`). The latter is a
contract requiring an already-active caller transaction. The vendored driver's
`is_autocommit()` returns a constant true, so the adapter does not rely on it.
The engine must pass its own transaction state. Sequence allocation may leave
gaps on rollback; incarnations must never be recycled.

`reconcile` now migrates ordinary unmanaged indexes atomically, preserving their
logical names while rebuilding backend identities. Unique integer indexes are
rebuilt with SQL Server NULL comparison; incompatible pre-existing data aborts
and rolls back the migration, preserving the old indexes. Reserved private
backend names without a logical record require explicit recovery.

`acquire` remains a managed-index read. `acquire_complete` additionally rejects
unmanaged and constraint-backed indexes, so a partial snapshot cannot silently
reach the DROP binder. Constraint-backed reconciliation remains implementation work.

`publish_views` now installs `sys.indexes` and `sys.index_columns` for heaps and
ordinary managed indexes. Both views check catalog completeness during reads
and raise on unsupported unmanaged or constraint-backed states. Six captured
setup/index stages match exactly for both row projections, including quoted
names and same logical names on different tables. Native comparisons preserve
Boolean semantics by reading Arrow bool8 extension metadata, as the engine
already does; the initial generic value reader lost that metadata and the raw
failed comparison was retained.

These are row comparisons, not complete wire-descriptor verification. The root
logical declaration/property adapter still needs catalog field metadata for
exact sysname/NVARCHAR widths, nullability and descriptor flags. Startup and
DDL hooks also remain unintegrated. Do not infer those properties from the
passing native row comparisons.

A persistent reopen test initially failed because DuckDB table OIDs changed
across restart. Current ownership joins use live native OIDs, but persistent
identity relies on the logical object ID and recorded incarnation. The caller
must synchronize logical objects after each DDL mutation. The reopen regression
now proves that the logical index identity remains usable after restart.

At runtime checkpoint `d9d423b`, all nine focused Linux native tests, all 649
workspace Rust tests, strict workspace Clippy and formatting passed. The initial
local native build was cancelled for disk pressure; no local pass is claimed.

`fields(view, catalog_collation)` now provides the logical declarations for the
columns currently published by these views. Tests compare system/user type IDs,
length, precision, scale, collation and nullability to captured sys.all_columns,
and provenance flags to direct empty-result descriptors. Sysname retains user
type ID256; type_desc uses Latin1_General_CI_AS_KS_WS independently of the caller's
catalog collation. The root query-catalog hook and the corresponding TDS
collation mapping remain integration work. The five reference columns not yet
published by these views remain an explicit schema gap.

The capture now reads system declarations through sys.all_columns, asserts they
are nonempty, and captures direct empty-result descriptors. These probes run
after the existing programs so they do not change the programs' @@ROWCOUNT
state. All prior27 observations are unchanged. Owned reference containers use
explicit cleanup, preserving startup logs before removal; an earlier automatic
removal race obscured a startup failure and is retained in local evidence.
