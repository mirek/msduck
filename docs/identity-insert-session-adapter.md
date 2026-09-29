# IDENTITY_INSERT session adapter

[`identity_insert_session.rs`](../src/identity_insert_session.rs) composes the
deterministic SQL operation and state rule with the live catalog resolver. It
recognizes only the parsed `SET IDENTITY_INSERT` AST variant. A successful
catalog lookup yields a persistent object ID and physical identity-column
position before the state can change. Missing and nonidentity targets retain
the captured 1088/8106 diagnostics; unsupported target shapes do not mutate
the setting.

The caller supplies both a stable database ID and the SQL-visible database
name. A session key compares the database ID and catalog object ID, so quoted
and case-varied aliases refer to the same active table, and equal object IDs
in different databases remain distinct. The names are retained for diagnostics.
A conflicting ON request returns the [captured](../reference/identity-insert.json)
8107/state 1/class 16 message and DONE command 253. The active table is shown
as `database.schema.table`; the requested table keeps the caller's identifier
spelling without brackets. Successful ON and OFF return DONE commands 183 and
184. Repeated ON, OFF while no table is active, and OFF of a different table
follow the shared deterministic state rule.

The [root integration tests](../tests/identity_insert_session.rs) use live
`Server` catalog connections and compare 1088, 8106 and 8107 against retained
SQL Server errors, including DONE commands. They check that two connections
have independent settings, rollback does not undo a setting, and an RPC copy
can change its own state without changing the caller's. The adapter path-imports
the SQL and catalog modules while their export files remain reserved.

This module is not yet exported or called by the engine, so it does not make
`SET IDENTITY_INSERT` executable over TDS. The later engine integration must
store `State` on each session, pass the correct logical database identity,
invoke this adapter only at execute time, carry its diagnostics to TDS, and
use the active key for INSERT permission and allocator handling. In particular,
the backend's `current_database()` is not assumed to be the SQL-visible name.
