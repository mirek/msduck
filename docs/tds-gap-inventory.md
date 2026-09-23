# TDS coverage and remaining implementation work

Reviewed source revision `8d9f9233fbc82335ded210f67ff7e504d65cc1ae` for
[task #10](https://github.com/mirek/msduck/issues/10), under
[workstream #5](https://github.com/mirek/msduck/issues/5). This inventory is a
source/test review, not a new wire-conformance run. “Covered” below means the
named behavior has implementation and regression source; it does not mean the
entire protocol area matches SQL Server. Existing skill implementation notes
refer to upstream mssqlite, not this server.

Use the copied [TDS skill](../.agents/skills/tds-protocol/SKILL.md), especially
[framing](../.agents/skills/tds-protocol/packet-framing.md),
[messages](../.agents/skills/tds-protocol/messages.md),
[tokens](../.agents/skills/tds-protocol/tokens.md),
[response patterns](../.agents/skills/tds-protocol/response-patterns.md), and
[annotated examples](../.agents/skills/tds-protocol/examples.md). Preserve their
upstream attribution. Protocol bytes and SQL transaction semantics need separate
checks: accepting a request token does not prove execution support.

## Current coverage and explicit limits

| Area | Implemented and covered behavior | Remaining boundary and source evidence |
| --- | --- | --- |
| Packet framing | [src/tds.rs](../src/tds.rs) reassembles/splits packets, bounds lengths, rejects type changes and invalid packet-ID progress. `fragmented_round_trip_and_id_wrap` and malformed-input sweeps cover selected paths. | Whole messages are buffered, capped at 16 MiB. Repeated fragment IDs are deliberately accepted for Tiberius. Status aggregation and all malformed offset/flag combinations are not exhaustively proven by the fuzz sweep. |
| PRELOGIN and version negotiation | [pure codecs](../crates/msduck-tds/src/lib.rs), `prelogin_with_policy`, `login`: plaintext/no-TLS and required-TLS policy, packet sizes 512–32767, four explicit TDS 7.2–7.4 version values. Reference vectors and encryption matrix tests exist. | MARS response is always disabled. LOGIN feature-extension negotiation is not implemented by `Login`/`login_response`; do not infer support from accepting a 7.4 version. Older versions and TDS 8 are not negotiated here. |
| TLS transport | [src/tls.rs](../src/tls.rs) wraps handshake flights in PRELOGIN, flushes the last flight, then carries encrypted TDS. [TLS client tests](../tests/tls.test.mjs) force fragmented certificates and RPC packets and check certificate rejection. | Configuration enables TLS 1.2, not a complete SQL Server encryption/version matrix. Login-only encryption and newer protocol negotiation require separate implementation/evidence. |
| Authentication and login lifecycle | [authentication](../src/authentication.rs) verifies a bounded hash-only bootstrap administrator file and reloads it for new logins. TLS is required when an administrator is configured; tests cover Unicode credentials, uniform failures and rotation. | With no administrator configured, the server accepts the supplied login name. SQL-managed principals/permissions are separate unfinished work. Integrated authentication and password-change flags are explicitly rejected by `login`; SSPI/FedAuth exchanges are absent. Native connections are cloned before handshake, while `Session::new` runs after authentication. |
| SQL batches and headers | [server](../src/server.rs), `serve_login`, dispatches SQL batch 1, RPC 3 and transaction manager 14; [request_headers](../crates/msduck-tds/src/lib.rs) validates header lengths and a unique transaction descriptor. [client tests](../tests/tedious.test.mjs) check stale transaction descriptors and reuse. | Other ALL_HEADERS entries are skipped after bounds checking; their semantics are not implemented merely because parsing succeeds. Only `master` is accepted by LOGIN7. SQL decoding uses valid Rust strings, distinct from raw UTF-16 RPC value support. |
| Prepared RPCs | [src/rpc.rs](../src/rpc.rs) supports `sp_executesql`, `sp_prepare`, `sp_execute`, `sp_prepexec`, `sp_unprepare` by the listed IDs/names. Handles are per connection, bounded and non-reused; tests verify no execution during prepare and failure cleanup. | Handles store SQL/declarations, not a cached native plan. General stored-procedure RPC dispatch, nonzero RPC flags, default/encrypted parameters and nonzero prepare metadata options are rejected. |
| Parameter and RETURNVALUE coverage | RPC unit vectors cover numeric/money, GUID, date/time/datetime, bounded/MAX strings and binary, malformed lengths and raw Unicode units. `return_handle` has an integer OUTPUT wire-vector test. | Ordinary output value parameters are rejected by `bind`. TVP, XML/CLR and SQL_VARIANT RPC inputs are not covered by the decoder's accepted type IDs. SQL_VARIANT result support does not imply SQL_VARIANT RPC input support. General output declaration/value encoding remains absent. |
| Result columns, rows and diagnostics | Pure `metadata`, `unicode_value`, decimal/money vectors and root encoding preserve many declared types, NULLs and UTF-16 units. Clients cover empty-result metadata, prepared bindings and metadata-before-error cases. | [engine](../src/engine.rs), `encode_batches`, buffers complete output and enforces a 16 MiB response bound. This is not streaming/backpressure support. `diagnostic_utf16` currently emits empty procedure and line 1; complete source context is absent. ORDER, TABNAME/COLINFO and feature/session tokens have no general emission path. |
| Transactions | Pure transaction request/ENVCHANGE vectors; root BEGIN/COMMIT/ROLLBACK and restart handling; driver tests exercise descriptor changes and connection reuse. | SAVE decodes successfully but [Session::transaction_request](../src/engine.rs) explicitly rejects it because native savepoints are unavailable. Isolation behavior, distributed/enlisted requests and doomed-transaction semantics need their own evidence; token support alone is insufficient. |
| Attention and IGNORE | Message type 6 emits DONE_ATTN; `attention_vector` checks the bytes. IGNORE completes without executing that message's payload. | The same thread reads a request, runs the synchronous batch, then writes the response before reading again. Attention cannot interrupt that active execution. No corresponding in-flight cancellation test was found in the inspected clients. Idle ACK coverage is not cancellation coverage. |
| Pool reset | Reset bits are detected. | `serve_login` rejects status `0x18` with “connection reset is not implemented”; RPC `sp_reset_connection` falls through unsupported dispatch. Transaction cleanup, options, temp objects and handle invalidation are not reset semantics yet. |
| Bulk load | No claimed implementation. | Server dispatch has no message type 7 path. Neither INSERT BULK negotiation nor incremental client COLMETADATA/ROW ingestion is established. |
| MARS / SMP | PRELOGIN advertises disabled. | No SMP codec/session table/flow-control scheduler exists in the Rust TDS crate/server. Multiple TCP connections are not MARS logical sessions. |

Useful existing regression anchors include `login_batches_types_and_empty_metadata`
and `null_rpc_parameters_keep_declared_types` in [Tiberius tests](../tests/client.rs),
`preparation_validates_without_running_sql_and_release_reclaims_storage` in RPC,
`transaction_requests_and_restart_decode` and
`malformed_transactions_are_rejected_before_execution` in the pure codecs, plus
`required TDS TLS encrypts login, fragmented RPC, results and transaction traffic`
in the TLS client suite. These anchors are specific evidence, not family-wide
conformance claims. No shared server tests were launched for this documentation task.

## Proposed bounded tasks and acceptance gates

These are proposals, not newly claimable tasks. Publish each under existing owner
authorization, with explicit file scopes and dependencies, then acquire a claim.
Changes to the shared server dispatcher must be serial; pure codecs and isolated
fixture preparation can be separate tasks once their interfaces are fixed.

1. **Define the request/Attention state machine.** Put deterministic request states,
   request identities and actions in the protocol crate; keep sockets and DuckDB
   out. Derive fixtures for idle Attention, post-EOM cancellation, completion races,
   repeated Attention, IGNORE before EOM and disconnect. Compare separate response
   message boundaries as well as token bytes. A stale cancellation must not target
   the next request. This is a prerequisite for active cancellation and MARS.
2. **Connect active cancellation to execution.** Root transport must continue
   receiving Attention while a session worker runs. The vendored
   [DuckDB interrupt handle](../vendor/duckdb/src/inner_connection.rs) is available,
   but is only an effect primitive, not a response-state implementation. Keep one
   owner of TLS transport state; do not share a blocked TLS stream behind a mutex
   that prevents Attention reception. Tests must cancel an actually running query,
   prove bounded response/reuse, verify transaction state and ensure the next
   request is not accidentally interrupted. Control test progress with a barrier
   or observable work signal, not only a fixed sleep. Preserve SQL Server's
   original-response completion followed by the separate Attention acknowledgement.
3. **Implement reset as one session operation.** Define/reset variables, settings,
   temp objects, active transaction state and RPC handles together; wire both
   reset flags and the RPC entry point to that operation. Capture the exact
   preservation rules for reset-with/without transaction preservation rather than
   treating both bits identically. Test pooled reuse, rollback visibility on a
   second connection, stale handles and pending cancellation. Depends on the
   request lifecycle and transaction cleanup contract.
4. **Generalize RPC output values.** Extract bounded declaration/value encoding
   from integer handle RETURNVALUE into a typed codec; extend binding to distinguish
   input/output directions. Initially scope to `sp_executesql` supported scalar
   families, then general procedure RPC separately. Capture ordinal/name/type,
   NULL and MAX behavior plus RETURNSTATUS/DONEPROC ordering. Test no premature
   output on errors, repeated execution, Unicode units and actual tedious output
   events. TVP decoding is a different task, not an output-parameter shortcut.
5. **Build an incremental bulk decoder, then its engine adapter.** Pure decoder
   tests split metadata, lengths, PLP chunks and rows at every boundary and reject
   malformed or incomplete streams without unbounded buffering. Root integration
   must bind INSERT BULK's target/columns, validate types, apply batches atomically
   and define rollback on IGNORE, Attention, malformed input and disconnect. Test
   identity/NULL/default/constraint behavior and real tedious bulk load; add
   FreeTDS framing only against captured behavior. Do not raise the whole-message
   limit and call that streaming.
6. **Add SMP framing/state before enabling MARS.** Pure fixtures cover SYN/ACK/DATA/
   FIN, session IDs, sequence wrap, credit windows, invalid flags and complete TDS
   packet nesting. Root tests then cover per-session handles/cancellation,
   interleaved responses, fairness, teardown and transaction interactions. Keep
   MARS disabled until both layers work. Depends on request lifecycle and response
   ownership; the codec itself can be implemented independently.
7. **Stream response tokens with backpressure.** Separate row production and token
   emission from whole-response buffering, retaining bounded individual values.
   Test results exceeding 16 MiB, slow readers, disconnect, PLP boundary splitting,
   errors after rows and cancellation during writes. Capture SQL Server's token
   prefix and completion behavior for each failure phase. Requires the same
   transport ownership work as cancellation; do not let competing tasks rewrite
   `serve_login`/`encode_batches` simultaneously.
8. **Make feature negotiation and rejection explicit.** Inventory LOGIN7 option
   flags/extensions and PRELOGIN options against the selected protocol versions.
   Validate feature lengths/termination and explicitly distinguish unsupported
   requests from supported acknowledgements. Add source-context diagnostics,
   ORDER/column-origin tokens, SSPI/FedAuth and TVPs as separately scoped tasks,
   each with its own negotiation and client fixtures. A broad “7.4 supported”
   label must not substitute for this feature matrix.

Transaction savepoints, full authentication/authorization and database routing
need engine/catalog work as well as wire codecs. Keep those dependencies visible
in [ROADMAP.md](../ROADMAP.md); do not mark a protocol task complete because an
unsupported request is now decoded correctly.

## Reuse from mirek/mssqlite

Inspected the existing local checkout at
`7f71f2081602f8e3051998f5c11f058e65fe24ec`. The package split and pure codecs are
useful references; SQLite effects and TypeScript scheduling are not drop-in Rust
implementations. Preserve [third-party notices](../THIRD_PARTY_NOTICES.md) when
copying fixtures or code.

- [`packages/tds/src/smp.ts`](https://github.com/mirek/mssqlite/blob/7f71f2081602f8e3051998f5c11f058e65fe24ec/packages/tds/src/smp.ts)
  and its tests provide a bounded header codec, explicit state and invalid-frame
  cases. Port the fixtures into the deterministic crate; root scheduling and
  native connection ownership need an independent implementation.
- [`packages/tds/src/bulk-load.ts`](https://github.com/mirek/mssqlite/blob/7f71f2081602f8e3051998f5c11f058e65fe24ec/packages/tds/src/bulk-load.ts)
  retains incomplete token state and emits complete rows. Reuse its boundary-case
  inventory and limits discipline, while preserving Rust scalar widths and
  lossless Unicode. Its own unsupported types are not the target compatibility set.
- [`packages/server/src/connection.ts`](https://github.com/mirek/mssqlite/blob/7f71f2081602f8e3051998f5c11f058e65fe24ec/packages/server/src/connection.ts)
  separates request cancellation, bulk cleanup and logical MARS session state.
  Its cancellation path finishes the original response and sends Attention ACK as
  another message; its IGNORE path explicitly avoids executing partial payloads.
  Reuse those distinctions, not an assumption that the current synchronous Rust
  read/execute/write loop can implement them unchanged.
- [`packages/server/src/respond.ts`](https://github.com/mirek/mssqlite/blob/7f71f2081602f8e3051998f5c11f058e65fe24ec/packages/server/src/respond.ts)
  distinguishes result sets, statement counts, messages, errors and procedure
  completion. Use that separation for test cases. The upstream review already
  records ORDER-token and completion gaps, so its emitted bytes are not themselves
  SQL Server ground truth.

## Verification for implementation work

Use exact pure byte vectors for codecs and deterministic state transitions, then
real transport tests for scheduling, framing and TLS ownership. Run independent
clients and pinned SQL Server differential captures for acceptance. Include
malformed/truncated inputs, NULL/empty/max values, multi-packet traffic, errors,
post-error reuse and transaction state. Compare packet message boundaries, complete
column descriptors, rows, ERROR/INFO fields, RETURNVALUE/RETURNSTATUS and DONE
families. Do not normalize missing fields or discard inconvenient token differences.

The diagnostic corpus completing is useful evidence of regressions and remaining
gaps, not proof of full SQL Server compatibility. Record source revision and the
specific protocol/client matrix tested with every result.
