# Attention cancellation reference

[`reference/attention.json`](../reference/attention.json) records 24 SQL Server
scenarios: SQL batch, `sp_executesql`, and prepared `sp_execute`, each with
explicit/autocommit transactions, XACT_ABORT ON/OFF, and TRY/CATCH present/absent.
The pinned SQL Server image and TLS transport are recorded in the fixture.

Run `node scripts/capture-attention.mjs OUTPUT_DIRECTORY` on the Linux builder.
The harness owns its container, generates private credentials, and uses fresh
isolated databases for every scenario. It captures twice and compares observations;
when the retained fixture exists, it also compares against that fixture. Raw
per-run artifacts are written before comparison, including when comparison fails.
No server implementation code changed in this task.

## How running execution is established

Each request inserts row 1, waits for 45 seconds, then would insert row 2 and return
42. The TRY/CATCH variant would insert row 3 if its handler ran. A second connection
queries `sys.dm_exec_requests` for the target's numeric SPID and waits until
`wait_type = WAITFOR`. Only then does the client send Attention. A 15-second
observation deadline is a failure bound, not the trigger for cancellation.

Both sides' post-login plaintext TDS packets are observed at the tedious transport
boundary, including when TLS protects the socket. Authentication traffic is not
captured. The observer does not replace or consume the driver's streams. Capture
storage is capped at 4 MiB. Incomplete packets or messages fail the capture.
This uses the repository's installed tedious internals; changes to those internals
must be checked before upgrading the capture harness.

## Observations

All 24 cases produced client error `ECANCEL` (`Canceled.`), then accepted the
follow-up SQL batch. The statement after WAITFOR and the CATCH insert did not run.
DATEFIRST remained 2 in the follow-up probe.

| Transaction before request | XACT_ABORT | Follow-up @@TRANCOUNT | Retained rows |
| --- | --- | --- | --- |
| Autocommit | OFF or ON | 0 | Row 1 |
| Explicit | OFF | 1 | Row 1 in the still-open transaction |
| Explicit | ON | 0 | None; rollback ENVCHANGE identifies the active transaction |

The combined follow-up `SELECT @@TRANCOUNT, XACT_STATE(), @@DATEFIRST` returned
XACT_STATE 1 in all these captures, including rows with @@TRANCOUNT 0. Retain that
observation exactly; it is not proof of an open explicit transaction, nor a general
claim about XACT_STATE evaluated in other statement shapes.

There are two separate response messages for each cancellation. The first contains
statement completion tokens, and in explicit/XACT_ABORT ON cases a rollback
ENVCHANGE. It ends with DONE_ERROR (batch) or DONEPROC_ERROR (RPC), without an ERROR
token in these traces. The second contains exactly:

```
FD 20 00 FD 00 00 00 00 00 00 00 00 00
```

That is DONE_ATTN, command **253**, row count 0. The command differs from the zero
command in the existing pure codec vector and copied mssqlite notes. Preserve the
captured command and message boundaries when implementing this workload. Do not
infer the idle or repeated-Attention response from these active WAITFOR cases.

For prepared requests, re-executing the same cancelled tedious Request object
returns ECANCEL again: the driver's `request.canceled` guard remains set. The
harness records this client outcome without clearing the flag. A fresh RPC Request
calling `sp_execute` with the same server handle and `hold=0` succeeds, as does
`sp_unprepare`. This separates driver-object reuse from server-handle validity.
The prepared reuse probe retains callback error/count; full row metadata for that
fresh RPC is not part of this capture.

## Raw data and comparison

The fixture retains both original raw packet runs, exact response payloads, and
active transaction descriptors. Packet header SPIDs and transaction descriptors
are server-generated identities and may differ between runs. Raw data is never
rewritten. Stable comparison checks complete response message order and decoded
DONE token family, status, command and count, plus client outcomes and complete
follow-up rows/metadata/diagnostics/completions.

The response decoder accepts only the token families actually observed here;
unknown tokens fail instead of being discarded. For rollback ENVCHANGE it verifies
type 10, the empty new descriptor, the eight-byte old descriptor, and exact equality
of that old value with this connection's pre-cancellation transaction descriptor.
Only after this relational check does stable comparison represent it as the active
transaction identity. An unrelated or malformed descriptor cannot compare equal.
This is an explicit identity binding, not tolerance for response differences.

Two complete captures agreed on all stable observations. Independent recapture
in a new container repeated all 24 scenarios twice and matched the retained
fixture. In total, 96 scenario executions completed across the two containers. The initial development run exposed the tedious
Request reuse guard; a subsequent run preserved differing raw transaction IDs and
motivated the explicit descriptor binding above.

## Remaining work

This is evidence for cancellation during WAITFOR, not implemented msduck
cancellation. It does not establish interruption of an actively computing or
writing DuckDB query, cancellation during commit, backpressure, output already
streamed, idle/duplicate Attention, pre-EOM IGNORE, completion races, disconnect,
plain TCP, MARS, bulk load or stored procedures. Cross-connection transactional
visibility beyond the observer's active-request check is also not captured here.
See [the design](attention-design.md) for those acceptance requirements.

No native build is required for this reference-only change. Validate JavaScript
syntax, fixture structure and repeated/independent SQL Server captures. Future
runtime changes must also pass native tests, independent clients and the diagnostic
audit; a successful reference capture does not make the server compatible.
