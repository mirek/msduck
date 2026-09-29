# Deterministic IDENTITY_INSERT permission gate

[`identity_insert_gate.rs`](../crates/msduck-sql/src/identity_insert_gate.rs)
classifies a parsed INSERT after the root adapter supplies a resolved table
identity, schema/table names, identity-column position, catalog column count,
caller session setting and column resolver. It does not query a catalog,
evaluate a source expression or advance DuckDB's allocator. The output is
`Generated`, `Explicit { source_column }` or `NotApplicable` for a table with
no identity. An error is either a captured SQL diagnostic with its DONE
command or an explicit unsupported shape.

For the captured shapes, a listed identity column while the target table is
OFF yields 544/state 1/class 16 and DONE command 195. Omitting it, including
`DEFAULT VALUES`, while ON yields 545 with command 195. Positional explicit
VALUES without a column list while ON yields 8101 with command 253. Messages
are constructed from the resolved base/qualified table names supplied by the
adapter and compared byte for byte with the first-party captures. A setting
for a different stable table key is OFF for this target, even when a textual
alias resembles it. Preflight precedes source conversion or constraint checks;
permission for a statement does not mean its rows can be committed.

The [path-imported tests](../crates/msduck-sql/tests/identity_insert_gate.rs)
replay applicable cases from the owner-controlled
[batch/session](../reference/identity-insert.json),
[setting/error](../reference/identity-insert-errors.json),
[RPC/prepared](../reference/identity-insert-rpc.json) and
[multi-row/OUTPUT](../reference/identity-insert-multirow.json) fixtures. They
also check quoted aliases, independent settings, identity-column position and
unresolved columns. The module remains unexported while
`msduck-sql/src/lib.rs` is reserved by another worker.

The root still must resolve table and column identities, bind the current
session setting at execute time, apply the shared native sequence advance,
publish identity scope values, and coordinate INSERT/OUTPUT atomicity. The
multi-row capture shows an OUTPUT row can be emitted before a later failure
rolls back the statement while retaining allocator advances. That behavior
cannot be achieved by this preflight alone. Uncaptured positional DEFAULT,
unlisted INSERT SELECT while ON, duplicate/invalid target columns and unusual
source shapes return unsupported rather than assigning guessed SQL Server
error precedence. The broader [runtime plan](identity-insert-runtime-plan.md)
maps the remaining engine integration.
