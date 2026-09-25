# MERGE failure boundaries inside SQL Server transactions

`reference/merge-transaction.json` retains 78 ordered TDS observations from each of two fresh databases on the pinned SQL Server 2025 image `mcr.microsoft.com/mssql/server:2025-latest@sha256:86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a` (ProductVersion `17.0.4065.4`, database collation `SQL_Latin1_General_CP1_CI_AS`). A second independent two-database recapture matched the retained fixture after binding only randomly generated database names. The raw fixture SHA-256 is `1169f2021fc50108ed1d570b6f68248b401c7c715ea1713c1fe9a5c3f44a2100`; it preserves unmodified names, result descriptors, rows, errors, information, DONE events and request row counts.

Every scenario starts with target `(1,10)` and inserts `(1,100)` into a separate prior-work table inside an explicit transaction. The MERGE then tries either an update plus a CHECK-violating insert (error 547), or two source rows updating the same target (error 8672). The capture reads `XACT_STATE()` and `@@TRANCOUNT` in a new request, checks all tables, tries a later `(2,200)` prior-work insert, and COMMITs or explicitly ROLLBACKs. A second connection reads the final committed rows. All failing MERGEs leave the target at `(1,10)`; the `OUTPUT ... INTO` sink stays empty.

| Failure and setting | State after MERGE (`XACT_STATE`, `@@TRANCOUNT`) | Prior work after MERGE | Finish and rows after reconnect |
| --- | --- | --- | --- |
| CHECK 547, `XACT_ABORT OFF` | `(1,1)` | `(1,100)` remains pending | Later write succeeds; COMMIT succeeds; prior rows `(1,100),(2,200)`. |
| CHECK 547, `XACT_ABORT ON` | `(0,0)` | Rolled back | Later write succeeds outside a transaction; COMMIT raises 3902; only `(2,200)` remains. |
| Duplicate-target 8672, `XACT_ABORT OFF` | `(0,0)` | Rolled back | Later write succeeds outside a transaction; COMMIT raises 3902; only `(2,200)` remains. |
| Duplicate-target 8672, `XACT_ABORT ON` | `(0,0)` | Rolled back | Same observed boundary as OFF. |
| CHECK 547 with `OUTPUT ... INTO`, `XACT_ABORT OFF` | `(1,1)` | `(1,100)` remains pending | Target and sink stay unchanged; later write and COMMIT succeed. |
| CHECK 547 inside TRY/CATCH, `XACT_ABORT OFF` | `(1,1)` | `(1,100)` remains pending | CATCH returns `(ERROR_NUMBER=547, XACT_STATE=1, @@TRANCOUNT=1)` in the same request; later write and COMMIT succeed. |
| CHECK 547, `XACT_ABORT OFF`, explicit rollback | `(1,1)` | `(1,100)` remains pending | Later write succeeds, then ROLLBACK discards both prior rows. |

The two error classes require different ambient-transaction policies. In particular, 8672 rolls back the enclosing SQL Server transaction even when `XACT_ABORT` is OFF; treating every MERGE error as a recoverable statement failure would be wrong. For 547 with `XACT_ABORT OFF`, a runtime adapter must undo the failed MERGE while retaining earlier transaction writes and keeping the transaction usable. PR #204's pinned DuckDB probe observes the opposite boundary for native CHECK/unique failures inside a DuckDB transaction: the enclosing transaction aborts. This fixture therefore rules out a direct native-MERGE translation as a complete SQL Server implementation.

The capture establishes these combinations and request boundaries only. It does not test concurrency, isolation levels, triggers, foreign keys, unique-key MERGE failures, an uncommittable `XACT_STATE=-1` path, or all TRY/CATCH and `XACT_ABORT` combinations. The generator deliberately leaves error messages and TDS details raw, including generated database names; only the two-run stability comparison binds those names.

Regenerate from an owner-approved checkout with `node scripts/capture-merge-transaction.mjs`. The helper starts and removes a fresh container on a loopback port, and never stores credentials in the fixture. A first-time `--write-fixture` run writes only when no retained fixture exists; later runs compare against it without overwriting. On this revision the pinned image was already cached on `linux.local`, where the exact script was run against two fresh databases and then recaptured in two more.
