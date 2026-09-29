# Deterministic IDENTITY_INSERT binding

[`identity_insert.rs`](../crates/msduck-sql/src/identity_insert.rs) extracts the
typed `SET IDENTITY_INSERT` AST node into an operation with one to three
identifier parts and an ON/OFF value. It retains the parsed identifier spelling
and quote style. Catalog resolution belongs to the root adapter: it must map
qualified, bracketed and unqualified names to a stable table identity, verify
that the table exists and has an identity column, then call the pure state rule.
An unresolved or nonidentity target must not mutate session state.

`SessionState<K>` owns at most one active resolved table key. Repeated ON of
that table succeeds without changing the state; OFF of another table leaves it
active; ON of another table returns the active and requested keys in a conflict
without changing the active key. Repeated OFF succeeds. The caller supplies
`K`, so database and table identity can be distinct from user-visible names.
Separate sessions own separate values. An RPC explicitly forks the caller's
state, applies nested SET operations to that copy, then discards it; this keeps
the caller's setting while allowing the shared identity allocator to advance
separately. Parsing a prepared operation has no state effect.

The path-imported [tests](../crates/msduck-sql/tests/identity_insert.rs) replay
the applicable SET cases in the owner-controlled
[batch/session](../reference/identity-insert.json),
[RPC/prepared](../reference/identity-insert-rpc.json), and
[setting/error](../reference/identity-insert-errors.json) captures. They also
exercise quoted names, malformed syntax, unsupported target shapes, distinct
sessions, and stable catalog keys across different spellings.

This module is not exported while `msduck-sql/src/lib.rs` remains reserved.
The root engine still lacks catalog resolution, 8106/1088/8107 diagnostic
formatting, execution of SET and INSERT permission rules, and RPC state
integration. The [runtime plan](identity-insert-runtime-plan.md) identifies
those boundaries; its earlier unprobed setting and failed-write cases are now
answered by the newer setting/error capture. Passing these pure tests does not
show that msduck executes `SET IDENTITY_INSERT` yet.
