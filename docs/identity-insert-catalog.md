# IDENTITY_INSERT catalog resolution

[`identity_insert_catalog.rs`](../src/identity_insert_catalog.rs) is the root
catalog effect for a parsed `SET IDENTITY_INSERT` target. It accepts one- or
two-part identifiers from the deterministic SQL binder and queries the live
`sys.tables`, `sys.schemas` and `sys.identity_columns` views using bound
parameters. The result contains the persistent `object_id`, resolved schema
and table spelling, and the identity column's zero-based physical position.
Alternate spellings of one table resolve to the same object ID; dropping and
recreating it allocates another ID. Database-qualified and linked-server
names remain explicitly unsupported until the multi-database binding rules
are integrated.

For the captured `dbo.missing` and `dbo.plain` targets, it returns the exact
SQL Server [reference](../reference/identity-insert-errors.json) diagnostics:
1088/state 11/class 16 for a missing table and 8106/state 1/class 16 for a
table without an identity, both with DONE command 253. A catalog inconsistency
or backend failure is separate from those SQL diagnostics. Unqualified missing
or nonidentity targets return `Unsupported` because their error spelling has
not been captured; successful unqualified targets still resolve through `dbo`.
Other requested forms have not been independently captured. The resolver never
changes a session setting or executes a SQL fragment supplied by the caller.

The [path-imported integration tests](../tests/identity_insert_catalog.rs)
use the real `Server` catalog for quoted/case aliases, physical identity
position, DDL replacement, transaction rollback and a table name containing
SQL punctuation. This module is not exported while `src/lib.rs` is reserved.
The root engine must later call it after parsing and before applying the pure
session transition; it must also carry the stable database identity alongside
`object_id` when multi-database sessions become available. Passing these
tests does not mean `SET IDENTITY_INSERT` executes over TDS yet.
