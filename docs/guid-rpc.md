# Uniqueidentifier RPC and result types

GUIDTYPE (0x24) requires a metadata width of 16 and a value length of either
0 for NULL or 16. The decoder reverses the first three fields from their
little-endian wire representation; the last eight bytes retain network order.
The input becomes a bound canonical UUID string cast to DuckDB UUID.
UNIQUEIDENTIFIER declarations, casts and table columns translate to UUID.

Sessions enable DuckDB lossless Arrow conversion. UUID fields carry the
arrow.uuid extension on fixed-size 16-byte binary arrays; this metadata
selects GUIDTYPE results even for NULL or empty results. Reading values through
the same typed path preserves UUIDs across DECLARE/SET/SELECT assignment.
The Arrow network-order bytes become canonical bound strings for rebinding;
result encoding restores the TDS mixed-endian layout. The accompanying
arrow.bool8 extension is decoded as Boolean so BIT and predicates retain their
previous behavior. Binary width alone never identifies a UUID.

The mssqlite reference codec in packages/tds/src/guid.ts supplied the byte-order
comparison; the Rust uuid crate provides checked conversion. A fixed vector
00112233-4455-6677-8899-aabbccddeeff verifies both directions. Unit tests reject
truncation and invalid lengths. Tedious covers zero/all-one GUIDs, storage,
case-insensitive UUID equality, local assignments, prepared reuse and typed
NULL/empty results.

Remaining work includes SQL Server GUID sort order, exact character-to-GUID
conversion/truncation rules, NEWSEQUENTIALID, catalog metadata and live
SQL Server differential validation. DuckDB UUID ordering must not be treated
as verified SQL Server ordering. No full uniqueidentifier compatibility claim
is made by these input/output tests.

Reference: [copied TDS data types](../.agents/skills/tds-protocol/data-types.md).

## NEWID generation

NEWID() translates to DuckDB's volatile uuid() expression, producing native
UUIDv4 values. It remains an expression during preparation and in persisted
column defaults, so each evaluation generates a value. It takes no arguments;
aggregate/window modifiers are rejected. Scalar variables preserve an evaluated
value until reassigned. Empty results still advertise GUID metadata.

Tedious tests exercise multiple calls per row, multiple rows, default values,
scalar variables, prepared reuse, empty metadata, invalid calls and recovery.
A file-backed Rust test closes and reopens the database before inserting another
default value, checking that old IDs persist and new rows get new IDs. These
checks detect accidental constant folding; they do not prove randomness quality
or full SQL Server evaluation-count equivalence for all query plans.

The reference project maps NEWID to a nondeterministic randomUUID UDF in
packages/engine/src/udf.ts. msduck uses the backend's native UUID expression.
See [Microsoft NEWID](https://learn.microsoft.com/en-us/sql/t-sql/functions/newid-transact-sql)
and [DuckDB UUID functions](https://duckdb.org/docs/current/sql/functions/utility).
