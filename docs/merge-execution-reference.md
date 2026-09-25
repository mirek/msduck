# SQL Server `MERGE` execution reference

`reference/merge-execution.json` retains 46 observations from each of two fresh databases in the pinned SQL Server 2025 image `mcr.microsoft.com/mssql/server:2025-latest@sha256:86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a` (ProductVersion `17.0.4065.4`). A second independent container with two more fresh databases matched the fixture after binding only the randomly generated database names appearing in CHECK-constraint error messages. The committed fixture preserves those raw names and all ordered TDS descriptors, rows, errors, information and DONE events. Its raw SHA-256 is `2ba7f45ca3d47aea6c2da1d62df1c789307352da144c958b19ba15d9c2cca01e`.

The generator covers matched UPDATE, unmatched INSERT, `NOT MATCHED BY SOURCE` DELETE, conditional mixed actions, CTE source, `OUTPUT $action`, `@@ROWCOUNT`, duplicate source matches, CHECK failure, explicit transaction rollback, `TOP (0)` and `TOP (1)`, negative TOP and invalid matched-arm ordering. Every expected success/error boundary and key final table state is asserted before comparison with the fixture. Multi-action output is inserted into a table and read with an explicit `ORDER BY`; the capture does not rely on an unspecified MERGE action order.

| Probe | SQL Server observation |
| --- | --- |
| Matched UPDATE, unmatched INSERT, by-source DELETE | Each affects one row and emits the corresponding `$action` row. `@@ROWCOUNT` remains `1` in the following request. |
| Direct `OUTPUT $action` | The action column is `NVarChar(20 bytes)` with TDS flags `0` and database collation. Projected non-null integer fields are fixed `Int`, flags `8`. |
| Mixed actions | One UPDATE, one INSERT and two DELETEs; MERGE DONE rowCount and subsequent `@@ROWCOUNT` are both `4`. The target ends as `(1,11),(4,44)`. |
| `OUTPUT ... INTO` | No MERGE result set; sorted output rows retain the four action records. The declared nullable integer output columns return `IntN(4)` with flags `9`. |
| Duplicate source match | Error `8672`; the target remains `(1,10)`. |
| CHECK failure in mixed UPDATE/INSERT | Error `547`; the whole statement is atomic and the target remains `(1,10)`. The raw message names the fresh database. |
| Explicit rollback | A MERGE updating row 1 and inserting row 2 reports DONE rowCount `2`; both changes are visible before rollback and absent after it. |
| `TOP (0)` / `TOP (1)` | Respectively affect zero and one row. Negative TOP raises `127` without changing the target. |
| Invalid matched arms | Unconditional arm before a conditional arm raises `5324`; repeated matched UPDATE raises `10714`. Neither changes the target. |

The copied `mssqlite` corpus includes MERGE semicolon validation, and `msduck` already validates some MERGE syntax. Its engine still rejects executable `MERGE`. This fixture is first-party execution evidence for a future implementation; it does not imply runtime compatibility. The remaining work includes deterministic action planning, statement atomicity, OUTPUT metadata and rows, transaction integration and exact error/completion behavior under separate claims.

To reproduce on a machine with Docker and Node.js 24+, run `node scripts/capture-merge-execution.mjs artifacts/compatibility/merge-execution/recheck.json`. The generator starts its own pinned container, uses two fresh databases, retains raw diagnostic output under ignored `artifacts/`, compares with the committed fixture after binding only generated database names, and removes its own container and databases. It needs no external SQL Server credentials.
