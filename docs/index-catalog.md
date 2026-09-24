# Table-owned index catalog

`reference/index-catalog.json` retains 24 first-party SQL Server catalog
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

The adapter implementation is pending. Its persistent identity mapping must
separate the logical table-owned index name and SQL Server index ID from a
qualified DuckDB backend name and an incarnation identity. Backend names must
not leak into the public catalog. An old bound drop must not silently target a
replacement that reused the same logical ID. Acquire the catalog and execute
against one consistent transaction, and verify the expected incarnation.

Creation/drop must mutate physical indexes and catalog rows under the same
transaction outcome. Standalone operations may own a transaction; operations
inside an explicit user transaction must preserve caller ownership and leave
rollback/error decisions to the engine's documented policy. Do not infer
transaction ownership from an attempted BEGIN or hide an error by committing.
The separate multi-target DROP binder requires earlier successful drops to
survive a later missing-target error when the caller has no explicit transaction.

This task owns `src/index_catalog.rs` and an isolated native integration test,
plus these captures/docs. Register/acquire/create/drop/sync APIs and their tests
remain to be implemented. Root server initialization, engine DDL hooks, SQL
exports/dialect and manifests remain under their existing claims and must be
integrated separately. Unsupported index kinds must remain explicit; a partial
catalog must not be presented as complete SQL Server index compatibility.
