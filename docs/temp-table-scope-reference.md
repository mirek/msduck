# SQL Server temporary-table scope reference

The owner-controlled [capture](../scripts/capture-temp-table-scope.mjs) retains 24 observations from SQL Server 2025 `17.0.4065.4` in [the raw fixture](../reference/temp-table-scope.json). The image is pinned to `mcr.microsoft.com/mssql/server:2025-latest@sha256:86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a`. Four fresh databases in two independent containers produced exactly equal results. A separate four-database replay matched the retained fixture byte for byte. The fixture retains rows, typed column descriptors, error number/state/class/message, INFO events, DONE event kind/count/continuation, and request row counts. No error or descriptor was normalized away.

Observed behavior:

- `#scope_local` survives a later batch on the creating connection. Another connection sees `OBJECT_ID('tempdb..#scope_local') = NULL` and `SELECT` fails with error 208, state 0, class 16.
- Nested `sp_executesql` and a Tedious `execSql` RPC can read the caller's local table. A nested write remains visible to the caller. A `#` table created *inside* either nested call is gone when the call returns, and the caller's next `SELECT` receives error 208.
- A `#` table created in a transaction is absent after rollback. An insert into a pre-existing local table is also undone while the table and its earlier rows remain.
- `##msduck_scope_probe` is visible across the two sessions. The second session's insert is visible to the creator. Explicit `DROP TABLE` removes it for both sessions. Explicitly dropped local tables and local tables owned by a disconnected session are unavailable afterward.
- The declared local `INT NOT NULL`, `NVARCHAR(20) NULL`, and `VARBINARY(4) NULL` columns return `Int`, `NVarChar` length 40 bytes, and `VarBinary` length 4 descriptors. Nested calls emit `doneInProc`/`doneProc`; ordinary batches emit `done`. The exact flags and counts remain in the fixture.

The capture uses separate client connections to the same fresh database, and creates/drops only the named tables in its own container. It asserts expected availability and diagnostic numbers before retaining results. The four complete captures must agree; the replay compares every retained field. Run `node scripts/capture-temp-table-scope.mjs` on a host with the pinned SQL Server image and Docker. `--write-fixture` is accepted only when the retained fixture is absent. The script rejects output aliases, symlinked paths, hard links, and existing output files.

Microsoft's [CREATE TABLE temporary-table documentation](https://learn.microsoft.com/en-us/sql/t-sql/statements/create-table-transact-sql?view=sql-server-ver17) describes local/global visibility and session lifetime. The [sp_executesql documentation](https://learn.microsoft.com/en-us/sql/relational-databases/system-stored-procedures/sp-executesql-transact-sql?view=sql-server-ver17) describes its separate batch and name scope. The exact nested-call, rollback, wire metadata, and error observations above come from the pinned first-party capture.

This fixture is ground truth for future implementation. `msduck` does not yet provide SQL Server-compatible temporary-table creation, per-session or nested-call lifetime, global visibility, or `tempdb` object resolution; successful reference capture is not a runtime compatibility pass.
