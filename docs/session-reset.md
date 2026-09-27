# Session reset (RESETCONNECTION)

Connection pools reset a pooled connection by setting the TDS RESETCONNECTION
status bit (0x08) on the first packet of the next request. Tedious
`Connection.reset()` and go-mssqldb (go-sqlcmd) both use it. msduck handles the
bit in `src/server.rs` as one session operation, before the request runs:

1. The request's transaction descriptor is validated against the session being
   reset, because the client names the transaction it still believes is open.
2. An open transaction is rolled back through the ordinary rollback path, which
   also sends the transaction-rollback ENVCHANGE (type 10). The client then
   clears its descriptor.
3. The session is replaced with a fresh one on a new DuckDB connection. This
   discards connection-scoped state: SET options such as NOCOUNT and
   DATEFIRST, DuckDB temporary objects and variables, and diagnostics.
   RPC prepared handles are released, so reusing one fails with error 8179, as
   in SQL Server. Only the authenticated `ORIGINAL_LOGIN()` survives.
4. The response starts with ENVCHANGE type 18, the reset completion
   acknowledgement defined by the ENVCHANGE token in
   [MS-TDS](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-tds/),
   followed by the request's own results.

`tests/session_reset.test.mjs` replays the scenarios of
[the SQL Server reference](session-reset-reference.md) that msduck can express.
It covers restored settings, the preserved login, rollback seen from a second
connection, a new transaction after reset, a stale prepared handle (8179), and
reuse after repeated resets. All three tests failed before this change with
"connection reset is not implemented".

## Differences and gaps

- **RESETCONNECTIONSKIPTRAN (0x10).** Alone or combined with 0x08, it still
  returns an explicit "not implemented" error. It has no reference capture yet
  (`session-reset-skiptran-reference-v1`), and SQL Server preserves the
  transaction in that mode.
- **Local temporary tables.** The reference drops `#reset_probe`, but msduck
  does not support `#` temp tables or `OBJECT_ID('tempdb..…')`, so that probe
  is not replayed. DuckDB `TEMP` objects are per-connection and are discarded.
- **ENVCHANGE ordering.** The reference records events, not packet bytes. The
  rollback-then-acknowledgement ENVCHANGE order follows MS-TDS and keeps
  Tedious's descriptor consistent. It is not a byte-level SQL Server capture.
- **RPC `sp_reset_connection`.** It still falls through unsupported dispatch.
- **Packet-level status.** Detection uses the assembled message status.
  First-packet-only handling is `tds-first-packet-reset-status-v1`.
