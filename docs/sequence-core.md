# Deterministic sequence allocation core

`crates/msduck-core/src/sequence.rs` models the arithmetic behind `NEXT VALUE
FOR` using an explicit integer definition and caller-owned state. It covers
TINYINT, SMALLINT, INT and BIGINT bounds, nonzero signed increments, first
allocation, checked advancement, exhaustion and cycling. Advancing uses `i128`
for intermediate arithmetic so an `i64` endpoint or step cannot wrap. Cycling
uses the configured minimum for an ascending sequence and maximum for a
descending sequence; it does not return to `START WITH`. A failed advance leaves
the caller's state unchanged.
The adapter can restore persisted state from a current value and allocation
flag using `SequenceSpec::restore_state`; it rejects values outside the
configured bounds and an unallocated value different from `START WITH`.

A `RowAllocations` instance belongs to one result row. It maps a caller-supplied,
database-scoped sequence identity to its allocated value. The first reference
advances the supplied state; another reference to the same identity in that row
returns the saved value. A new row uses a new instance. The map is a local,
deterministic `BTreeMap`; there is no clock, randomness, database access or
process-global allocator in this module.

The [standalone test](../crates/msduck-core/tests/sequence.rs) reads the
[pinned SQL Server sequence capture](../reference/sequence-reference.json) and
checks ascending 10/12/14/16, descending 0/-1/-2, per-row sharing, exhaustion
and restart/cycle behavior. Additional tests cover numeric endpoints, invalid
definitions, oversized steps, independent identities and unchanged state after
failure. The module is staged without an export from `msduck-core` while that
file is reserved by another live claim; the test imports it by path.

The root adapter must still bind CREATE/ALTER/DROP SEQUENCE and NEXT VALUE FOR,
acquire database-scoped identities, persist allocation outside rollback, lock or
otherwise serialize concurrent allocation, bind one value per output row,
represent `sys.sequences` with typed `SQL_VARIANT` values, and emit the captured
TDS descriptors, error 11728 and DONE status. This pure core does not claim
those behaviors. Microsoft documents [sequence bounds and cycling](https://learn.microsoft.com/en-us/sql/t-sql/statements/create-sequence-transact-sql?view=sql-server-ver17)
and [one allocation per row for repeated references](https://learn.microsoft.com/en-us/sql/t-sql/functions/next-value-for-transact-sql?view=sql-server-ver17);
the pinned capture supplies the specific SQL Server 2025 cases tested here.
