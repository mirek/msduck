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

The current adapter acquires only indexes created through its APIs. It does not
yet reconcile pre-existing/unmanaged indexes or constraint-backed indexes and
does not install `sys.indexes` or `sys.index_columns`. Consequently its acquired
rows are not yet a complete catalog suitable for the DROP binder's complete-
snapshot contract. Those reconciliation/public-view steps and wire comparisons
remain required before root integration can claim this task complete.

At checkpoint `7024c8c`, all four focused Linux native tests and strict workspace
Clippy passed; all 644 workspace Rust tests also passed. The initial local
native build was cancelled for disk pressure; no local pass is claimed.
