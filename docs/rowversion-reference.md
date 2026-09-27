# SQL Server rowversion reference

The [retained capture](../reference/rowversion.json) contains 31 ordered requests
from each of four fresh databases: two databases in each of two independent
containers of the pinned SQL Server 2025 image. All four raw request/result
streams are identical. Each record keeps ordered rows, TDS column descriptors,
SQL error number/state/class/message, and DONE-family completion tokens. The
capture script uses the same isolated-database, Tedious and container lifecycle
helpers adapted from `mirek/mssqlite`; it does not modify msduck execution.

The official [rowversion reference](https://learn.microsoft.com/en-us/sql/t-sql/data-types/rowversion-transact-sql?view=sql-server-ver17)
describes the database-wide eight-byte counter, no-op update behavior, the
deprecated `timestamp` synonym and the `SELECT INTO` duplicate-value caveat.
The capture below adds exact SQL Server 2025 wire and diagnostic evidence.

| Operation | Captured behavior |
| --- | --- |
| Fresh database | `@@DBTS` was `0x00000000000007D0` in each run, exposed as TDS `VarBinary(8)` (flags 32). This initial value is an observation of the pinned image, not a portable starting-value rule. |
| DDL | Creating four tables, including `ROWVERSION NULL` and an unnamed `TIMESTAMP` column, left `@@DBTS` unchanged. `sys.columns` calls every variant `timestamp`, length 8; the unnamed column is named `timestamp`. |
| Insert and update | First insert allocated `...07D1`; an insert into a second table allocated `...07D2`. A no-op `UPDATE SET v=v` allocated `...07D3`. An update matching no row completed with count 0 and did not advance `@@DBTS`. Separate nullable and synonym tables allocated `...07D4` and `...07D5`. |
| Wire metadata | A nonnullable rowversion column arrives as TDS `Binary(8)` (flags 0); the nullable declaration still arrives as `Binary(8)` with nullable flag 1. `@@DBTS` arrives as `VarBinary(8)`. A literal `CAST(... AS ROWVERSION)` arrived as `Binary(8)` (flags 33) and did not allocate a value. Preserve these descriptors rather than replacing them with the documentation's broad binary/varbinary analogy. |
| Explicit writes | Inserting a supplied rowversion failed with 273/state 1; updating it failed with 272/state 1. Neither advanced `@@DBTS` or changed the row. Both errors had no result descriptor and ended with DONE without a count. |
| Rollback | An insert inside a transaction allocated `...07D6`. Rolling back removed the row but left `@@DBTS` at `...07D6`; the next insert allocated `...07D7`. Allocation therefore cannot be modeled as a transactional table sequence. |
| RPC | A parameterized no-op update allocated `...07D8`. The successful RPC emitted `doneInProc` for the update and select, then `doneProc`; the read RPC emitted one `doneInProc` and `doneProc`. |
| `SELECT INTO` | Copying a rowversion column kept its existing bytes and created a destination `timestamp` column of length 8; it did not advance `@@DBTS`. This can create duplicate rowversion values across tables. |
| DDL rejection | A second rowversion column failed with 2738/state 2. A default failed with two errors, 1755/state 0 then 1750/state 0. `IDENTITY` on rowversion failed with 2749/state 2. The fixture retains complete messages and completion tokens. |

For normal batch `INSERT`/`UPDATE` followed by `SELECT`, SQL Server emitted a
DONE with the affected-row count and `more=true`, then a final DONE with the
selected row count. A zero-row update had count 0 and preserved the prior
version. Error requests emitted no row metadata and a final DONE without a
count. The final reuse probe showed `@@TRANCOUNT=0`, `XACT_STATE()=1` and a
successful subsequent result.

The reference deliberately does not infer multirow allocation order, counter
overflow, crash recovery, cross-database ordering, trigger behavior, or native
JSON/client feature negotiation. The fixture's exact initial value and TDS flags
are evidence for this pinned SQL Server 2025/Tedious configuration, not a claim
about every version or client.

Runtime follow-up should keep effects in the root crate: persist a separate
counter per database, allocate at each inserted or updated row even for a no-op
assignment, and keep allocations consumed after rollback. A deterministic
`msduck-core` helper can encode/decode a supplied eight-byte version and check
counter arithmetic without reading process state. `msduck-sql` binding should
recognize the declaration and compile-time write errors, while root catalog,
storage and DML adapters own the persistent counter, `@@DBTS`, and `SELECT INTO`
copying. TDS encoding must preserve the captured `Binary(8)`/`VarBinary(8)`
families and nullable flags. Public integration tests should replay the
captured transaction, two-table, RPC and error cases against msduck and retain
remaining differences explicitly.

Replay with `node scripts/capture-rowversion.mjs` in a Docker-enabled
environment. `--one-database` is a quick fixture check; `--write-fixture`
requires four fresh runs and refuses to overwrite the retained file. The
script also writes its raw local capture under ignored `artifacts/compatibility/rowversion/`.
