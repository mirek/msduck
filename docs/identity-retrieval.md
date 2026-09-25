# Identity retrieval reference

`reference/identity-retrieval.json` retains 31 ordered SQL Server observations,
including decoded rows, full column descriptors, diagnostics, and DONE tokens.
`scripts/capture-identity-retrieval.mjs` ran against the pinned SQL Server 2025
image `sha256:86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a`.
Two fresh databases produced identical results; a second independent container
produced the same two-database capture. The fixture SHA-256 is
`eda04bd2d8418c503abd6cea8b5d38cb1ba93d626cff1c852c00e61511311fe0`.
The fixture contains Tedious-decoded values and TDS descriptors, not raw TDS
packet bytes.

The observed `SCOPE_IDENTITY()`, `@@IDENTITY`, and `IDENT_CURRENT` columns are
all `NumericN`, length 17, precision 38, scale 0. Both session functions are
initially NULL. `IDENT_CURRENT` returns each table's seed (10 and 100) before
allocation. After the first parent insert, `SCOPE_IDENTITY()` remains 10 in a
subsequent SQL batch. A second connection starts with NULL session functions,
allocates parent ID 12, and sees its own scope/session value of 12. The first
connection retains its own scope/session value of 10 while `IDENT_CURRENT`
advances to 12. In bound `sp_executesql` RPCs, the inner scope reports IDs 14
and 16; afterward the first connection's caller scope still reports 10, while
`@@IDENTITY` reports the last RPC allocation.

A duplicate-value insert fails with error 2627/state 1/class 14. It advances
the table's current identity to 18, but leaves the first session's
`SCOPE_IDENTITY()` at 10 and `@@IDENTITY` at 16. An insert rolled back in the
same batch advances the table current value and both session functions to 20;
the inserted row disappears. A trigger inserts into a second identity table:
the parent insert returns scope 22 and session 100. A procedure returns its
own scope 24, while the caller scope remains 22 and the session value follows
the trigger to 103. An explicit `IDENTITY_INSERT` of parent ID 50 yields caller
scope 50 and trigger-driven session value 106. `TRUNCATE` resets the parent
table's current identity to seed 10 but does not clear the session values.

The BIGINT-endpoint probe inserts `9223372036854775800` and returns that exact
value as BIGINT and as string conversions of all three identity functions.
Tedious presents the corresponding `NUMERIC(38,0)` values as the rounded
JavaScript number `9223372036854776000`. Implementations and comparisons must
retain exact decimal digits instead of trusting that decoded number.

The owner-controlled [mssqlite identity implementation](https://github.com/mirek/mssqlite/blob/7f71f2081602f8e3051998f5c11f058e65fe24ec/packages/engine/src/identity.ts)
stores separate session and scoped values; its
[tests](https://github.com/mirek/mssqlite/blob/7f71f2081602f8e3051998f5c11f058e65fe24ec/packages/engine/src/identity.test.ts)
cover failed allocations, triggers, procedures, and dynamic SQL. Its nested
execution saves and restores caller scope. The current Rust `Session` in
`src/engine.rs` has no corresponding identity fields. `src/identity_metadata.rs`
implements table-level `IDENT_CURRENT`, but not the two session functions.
Successor integration must record a generated or explicit identity only when
an insert successfully publishes it, preserve allocation gaps after failures,
and distinguish the caller's scope from nested RPC, trigger, and procedure
scopes. A sequence's last value alone cannot implement the session functions.
The successor also needs exact `NUMERIC(38,0)` result typing and independent
TDS/client replay against this fixture. No Rust runtime behavior was changed
by this reference task.

Microsoft's [SCOPE_IDENTITY reference](https://learn.microsoft.com/en-us/sql/t-sql/functions/scope-identity-transact-sql?view=sql-server-ver17)
defines the scoped function and result type. The
[IDENT_CURRENT reference](https://learn.microsoft.com/en-us/sql/t-sql/functions/ident-current-transact-sql?view=sql-server-ver17)
distinguishes table, session, and scope visibility and describes non-rollback
allocation gaps.
