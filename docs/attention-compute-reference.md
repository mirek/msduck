# Cancellation during active computation

[`reference/attention-compute.json`](../reference/attention-compute.json) retains
SQL Server observations for 24 scenarios: batch, direct `sp_executesql`, and
prepared `sp_execute`, crossed with explicit/autocommit transactions,
XACT_ABORT ON/OFF, and TRY/CATCH present/absent. The image is pinned in the fixture;
all connections use TLS. This is reference evidence, not implemented cancellation.

Run `node scripts/capture-attention-compute.mjs OUTPUT_DIRECTORY` on the Linux
builder. Each invocation owns a fresh reference container, captures every case
twice in isolated databases, compares complete stable observations, and checks
the retained fixture if present. Raw runs are saved before comparison.

## Establishing active computation

The request inserts row 1, conditionally executes a long aggregate over three
cross-joined `sys.all_objects` instances with MAXDOP 1, then would insert row 2
and return 42. Its optional CATCH handler would insert row 3.

A second connection polls `sys.dm_exec_requests` for the target SPID. Cancellation
requires command SELECT and an exact match between the active statement substring
and the aggregate SQL. A 15-second deadline bounds failure; elapsed time alone
never triggers Attention. An initial comparison exposed transient statement
offsets that described the whole batch. The original permissive substring check
was replaced with exact equality, rather than excluding the differing statement
from comparison. The initial raw runs remain in local diagnostic artifacts.
Scheduler wait types are preserved separately and are not protocol expectations.

## Observed transaction and protocol behavior

All cases return client ECANCEL and successfully reuse the connection. DATEFIRST
remains 2. Neither row 2 nor the CATCH handler's row 3 appears.

| Initial transaction | XACT_ABORT | Follow-up @@TRANCOUNT | Retained rows |
| --- | --- | --- | --- |
| Autocommit | OFF or ON | 0 | Row 1 |
| Explicit | OFF | 1 | Row 1 |
| Explicit | ON | 0 | None |

The combined state query returns XACT_STATE 1 in every case, including when
@@TRANCOUNT is 0. Preserve this statement-specific observation without inferring
an explicit transaction from it.

The first response includes the computed column's COLMETADATA (nullable FloatN,
eight bytes, named `work`) but no result row. Complete token sequences are retained
per case, including differences caused by TRY/CATCH. Explicit/XACT_ABORT ON cases
include rollback ENVCHANGE bound to the connection's active transaction descriptor.
The first message ends with DONE_ERROR for batch or DONEPROC_ERROR for RPC. There
is no ERROR token in these captures. A separate response message contains exactly:

```
FD 20 00 FD 00 00 00 00 00 00 00 00 00
```

This is DONE_ATTN, command 253, count 0. Prepared server handles remain usable via
a fresh `sp_execute` Request with the computation disabled; the cancelled tedious
Request itself still returns ECANCEL. That probe records callback error/count,
not full row metadata. See the [WAITFOR capture](attention-reference.md) for the
same driver-object distinction and transport observation method.

## Evidence fidelity and backend consequence

Both raw packet runs, reassembled response messages, active transaction descriptors,
and scheduler observations are retained. Stable comparisons include exact decoded
token order and message boundaries, metadata bytes, completion status/command/count,
client events, and complete follow-up rows/descriptors/errors/completions. Unknown
tokens fail the decoder. Rollback descriptor comparison checks its exact equality
to the connection's active descriptor before representing that identity symbolically.
Packet SPIDs and raw transaction identities remain available unchanged.

The [native DuckDB probe](native-cancellation-control.md) interrupts a long SELECT
after an earlier insert inside an explicit transaction. Its next query fails with
`Current transaction is aborted (please ROLLBACK)`. These computation captures
confirm a corresponding SQL Server requirement with XACT_ABORT OFF: preserve the
transaction and prior work. Unconditional rollback, resetting the interrupt flag,
or replaying earlier statements does not establish that behavior. Native execution
and cleanup boundaries and a transaction-preserving strategy remain required.

## Coverage limits

This captures cancellation of a read computation after a completed write. It does
not establish cancellation during a write, commit, streamed rows or backpressure;
idle/repeated Attention, completion races, pre-EOM IGNORE, disconnect, plaintext,
MARS, bulk load, stored procedures, and cross-connection transaction visibility
remain separate work. The [Attention design](attention-design.md) describes the
transport/worker integration requirements.

Two fresh runs agreed across all 24 cases. An independent invocation in a new
container captured another two runs and matched the retained fixture: 96 scenario
executions across the final capture and verification. The earlier diagnostic pair
is not counted as successful verification.

No Rust production code changes here. JavaScript syntax and repeated SQL Server
captures validate the harness; native, independent client and audit checks remain
required when runtime behavior changes.
