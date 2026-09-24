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

This evidence establishes a runtime gap, not a completed fix. It does not
establish all SET options, non-English language behavior, stored-procedure
nesting, prepared execution, transaction failures or Attention/cancellation.
Those boundaries need additional evidence before claiming general restoration.
