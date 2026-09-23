# Transaction-manager implementation

Implemented local TDS transaction requests in addition to SQL batch transaction
statements. The two entry points share the same Session methods and DuckDB
connection. Nested BEGIN increments @@TRANCOUNT without starting another
DuckDB transaction. Only the first begin allocates a descriptor; only the
outermost commit or full rollback clears it and emits a completion ENVCHANGE.

Implemented wire operations:

- TM_BEGIN_XACT (5), including a UTF-16 transaction name.
- TM_COMMIT_XACT (7), including the begin-after-commit flag.
- TM_ROLLBACK_XACT (8), including named outer rollback and begin-after-rollback.
- Environment changes 8/9/10 with eight-byte descriptors.
- ALL_HEADERS transaction descriptor extraction and validation for SQL, RPC,
  and transaction-manager requests. Duplicate descriptor headers are rejected.

The decoder validates lengths, names, isolation bytes, flags, trailing bytes,
and all optional restart data before execution. Restart isolation validation
also precedes mutation of the existing transaction. Unknown rollback names
return 6401; unmatched commit/rollback return 3902/3903. Declared savepoints and
distributed transaction requests fail explicitly instead of becoming no-ops.

Verification added:

- Real tedious driver begin/commit/rollback combined with SQL BEGIN and RPC
  queries, nested counts, transaction descriptors and connection reuse.
- Stale descriptor requests rejected before an INSERT; unknown rollback names
  and unsupported savepoints leave the active transaction usable.
- Decoder malformed/truncated input tests and exact ENVCHANGE byte vectors.
- Commit/rollback restart retains or removes actual rows as appropriate and
  generates a fresh descriptor. Unsupported restart isolation preserves state.
- Dropping a session rolls back uncommitted writes in shared DuckDB storage.

Remaining requirements: full SQL Server isolation and transaction-error
semantics, savepoints, distributed transactions, named SQL BEGIN parser
coverage, aborted-transaction XACT_STATE behavior, reset, and live differential
validation against SQL Server. Current/read-committed/snapshot requests run on
DuckDB snapshot isolation; this is not proof of READ COMMITTED equivalence.

Primary contract:
[MS-TDS Transaction Manager Request](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-tds/0fb28ba5-ddcb-4d02-95c3-aa5b05ec6092).
Upstream source reviewed:
`mirek/mssqlite@7f71f2081602f8e3051998f5c11f058e65fe24ec`,
`packages/tds/src/transaction-manager.ts`, and the copied TDS/tedious skills.
Unlike the upstream decoder, this decoder rejects incomplete commit flags and
parses/validates the optional restart payload rather than discarding it.

SQL transaction syntax now validates modifiers before batch execution. The
regression test attempts unsupported exception delimiters and chaining both
before and during an active transaction, verifies that preceding writes do not
run on preflight rejection, and confirms that later rollback removes the
uncommitted row. Plain BEGIN/END grouping and ordinary BEGIN TRAN remain distinct.
