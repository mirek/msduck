# SQL Server session reset reference

`scripts/capture-session-reset.mjs` exercises Tedious 20.0.0 `Connection.reset()`
against the pinned SQL Server 2025 image
`sha256:86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a`.
The retained raw artifact is `reference/session-reset.json` (SHA-256
`931befdde7d43112ebd1f9337ee3b8160a4c480666a23114016a41258386f1ad`).
Two fresh databases in each of two independent containers gave identical
observations: 12 steps per database, including two resets. The artifact keeps
each query's ordered rows, column metadata, errors, info messages and DONE
events. The reset callback and response events are recorded separately.

The installed Tedious 20 source in `connection.js` sets
`resetConnectionOnNextRequest = true` and calls `execSqlBatch` with
`getInitialSql()`. Its outgoing `Message` had `type: 1` (SQL Batch) and
`resetConnection: true` in both captures. The Tedious `packet.js` writer maps
that property to `RESETCONNECTION` status bit 0x08 on the first packet. This
matches the [MS-TDS Status specification](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-tds/ce398f9a-7d47-4ede-8f36-9dd6fc21ca43).
The instrumentation records outgoing message metadata, not packet bytes or
payloads; the exact emitted bit is established from the installed driver source.
The copied mssqlite client skill's older “RPC `sp_reset_connection`” row did
not describe this Tedious version.

| Probe | Before reset | After reset |
|---|---|---|
| Session settings | `@@DATEFIRST = 3`, NOCOUNT on | `@@DATEFIRST = 7`, NOCOUNT off |
| Local temp table | `#reset_probe` exists and contains `7` | `OBJECT_ID` reports absent |
| Open transaction | `@@TRANCOUNT = 1`, inserted `9` visible | `@@TRANCOUNT = 0`, durable table row count `0` |
| Reuse | — | Queries returned `42` and `43` after respective resets |

Both `reset()` callbacks reported no error. Each reset produced 14 DONE
events from Tedious's initial SQL batch, with no rows or error events. The
pre-reset NOCOUNT setting changes the first reset's completion counts; the
retained fixture preserves the exact sequence rather than normalizing it.

This establishes ordinary Tedious pool reset behavior for these settings,
temp objects and one open local transaction. It does not cover prepared
handles, distributed transactions, transaction isolation, other SET options,
MARS, `RESETCONNECTIONSKIPTRAN`, or a raw byte-level packet trace. No msduck
server behavior is inferred: the root server currently rejects reset status
bits, as recorded in `docs/tds-gap-inventory.md`.

To recheck the fixture with a local Docker daemon and installed dependencies,
run `node scripts/capture-session-reset.mjs`. The script creates and removes
its own reference container and fresh databases. It refuses to overwrite the
retained fixture; `--write-fixture` is only for first creation.
