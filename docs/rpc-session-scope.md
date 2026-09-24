# RPC session setting scope

`reference/rpc-session-scope.json` retains 14 scenarios captured twice in fresh
SQL Server databases, then independently recaptured against the retained file.
Reproduce with `node scripts/capture-rpc-session-scope.mjs` using the pinned
reference container. Each scenario records a SQL-batch baseline, an execution
through SQL batch or `sp_executesql` RPC, and a subsequent RPC reuse probe.
Rows, descriptors, diagnostics and completion events are retained in full.

SQL-batch changes to DATEFIRST and ANSI_WARNINGS persist into the next request.
RPC changes affect statements inside that RPC but restore the caller's settings
before the next request. Restoration also occurs after RAISERROR and THROW.
The NULL-elimination warning (8153) provides an observable ANSI_WARNINGS probe.

SET LANGUAGE us_english resets DATEFIRST to 7. In SQL batch it also reports
informational message 5703 and retains the reset. In RPC it emits no such message
in these captures and restores the caller's previous DATEFIRST afterward.

The runtime at `436648285daac94d498de3fb108d8e2aa8d2a4f8` matches 6 of 14
complete scenarios. Its eight differences are:

- Three DATEFIRST RPC scenarios leak 3 into the next request instead of restoring 7.
- Three ANSI_WARNINGS RPC scenarios suppress the subsequent 8153 warning.
- The LANGUAGE RPC scenario leaks 7 instead of restoring the caller's 3.
- The LANGUAGE SQL-batch scenario omits informational message 5703.

Runtime revision `783cf874401274fdd01a00fa443217ec03f051f1` matches all 14
complete scenarios and passes 29 focused client tests. DATEFIRST and
ANSI_WARNINGS restore alongside NOCOUNT and XACT_ABORT at the RPC boundary;
SQL-batch settings persist. The captured batch language message is emitted.
Rust session state is authoritative. DATEFIRST is synchronized into DuckDB
before evaluation and after transaction completion, so RPC restoration itself
cannot fail inside an aborted native transaction or replace the original error.
A native regression verifies restoration and recovery after a constraint error.
Full workspace, aggregate-diagnostic and client verification remain pending.

Existing temporal and aggregate tests now establish persistent settings through
SQL batch, preserving their query/result assertions. They no longer depend on
the prior RPC leak. The upstream mssqlite review found that
`packages/engine/src/bind.ts` binds `@@datefirst` to the constant 7; that behavior
cannot supply this session-scope implementation.

This does not establish all SET options, non-English language behavior, stored-procedure
nesting, prepared execution, transaction failures or Attention/cancellation.
Those boundaries need additional evidence before claiming general restoration.

## Prepared execution

Two additional traces use tedious prepare/execute/unprepare with eight total
executions. They retain state probes before and after preparation, complete
execution responses, reuse probes after every execution and a final unprepare
probe. All captures were repeated twice and independently recaptured.

The prepared DATEFIRST trace covers valid values, THROW, zero, NULL and recovery;
the ANSI_WARNINGS trace covers successful execution, THROW and reuse. Preparation
does not change the settings, and execution restores the caller's settings.
SQL Server reports error 2742, state 1, class 16 with message
`SET DATEFIRST 0 is out of range.` for both zero and NULL. Later statements still
execute with the unchanged DATEFIRST value. The retained invalid executions
return RPC status -6.

Runtime revision `783cf874401274fdd01a00fa443217ec03f051f1` matches the entire
ANSI_WARNINGS trace and six of eight complete execution/reuse observations.
The zero and NULL cases instead report error 50000 and end the request, omitting
the subsequent result and completion events. These are retained compatibility
gaps; the fixture does not normalize them away.
