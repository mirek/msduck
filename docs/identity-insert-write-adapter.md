# IDENTITY_INSERT INSERT preflight adapter

[`identity_insert_write.rs`](../src/identity_insert_write.rs) combines the
deterministic [permission gate](../crates/msduck-sql/src/identity_insert_gate.rs)
with a live root catalog lookup. For an ordinary one- or two-part INSERT target,
it resolves a persistent object ID and the identity column's physical position,
then reads the ordered target column names through bound catalog parameters.
It passes these facts and the caller-owned `IDENTITY_INSERT` state into the pure
gate. No INSERT source expression is evaluated, no row is written, and no
identity allocator is advanced during preflight.

The gate returns `Generated` for a permitted implicit identity write,
`Explicit { source_column }` for a permitted listed identity source, or a
captured 544, 545, 8101, 264, 207 or 339 diagnostic with its DONE command. The session key
uses the caller-supplied stable database ID plus the catalog object ID, so a
quoted alias of the active table remains ON and an unrelated active table does
not authorize an explicit write. A table without identity and a missing target
return `NotApplicable`; the ordinary INSERT binder must handle those cases and
report their own errors. The captured single invalid column and duplicate
identity column are diagnosed before source execution. Mixed, ambiguous or
multipart column defects, other unknown source shapes and temporary/multipart
targets remain explicitly unsupported.
The captured single-row `NULL` identity value while ON produces 339 and failed
DONE command 253 before any row write or allocator advance. A root test replays
both retained runs from the [conversion fixture](../reference/identity-insert-conversion.json)
against a live catalog and confirms unchanged session state, row count and
private-sequence last value.

The [root integration tests](../tests/identity_insert_write.rs) replay the
owner-controlled [batch](../reference/identity-insert.json),
[setting/error](../reference/identity-insert-errors.json) and
[INSERT-shape](../reference/identity-insert-shapes.json) captures against a live
`Server` catalog. They also put the identity column second in a real table and
check the returned source position for two column orders. This module is
path-imported while SQL and root export files are reserved.

The engine does not call this adapter yet. The later integration must invoke
preflight before source evaluation, feed `Explicit` values into the shared
allocator and session identity functions, coordinate statement/OUTPUT
atomicity, and emit the chosen diagnostic and DONE token over TDS. These tests
prove the adapter's decisions, not end-to-end `SET IDENTITY_INSERT` execution.
