# NOCOUNT completion behavior

`reference/nocount-completions.json` and
`reference/nocount-completions-rpc.json` retain 18 SQL Server 2025 captures each,
including decoded completion command fields. The programs cover PRINT,
assignments, empty SELECT, ordinary DML, DML OUTPUT, informational and error
RAISERROR, THROW, caught arithmetic errors, loop control, transactions, RETURN,
and changing NOCOUNT within a request.

SQL batches retain statement completions with NOCOUNT on, while suppressing the
row-count-valid field. RPC execution suppresses ordinary statement completions
but retains result boundaries, including zero-row results and DML OUTPUT. It
also retains error completions. The final DONEPROC and return status are still
required when every intermediate completion is suppressed.

`msduck_core::completion` expresses visibility from explicit request mode,
NOCOUNT state, and statement/result-set/error outcome. The execution adapter now
returns a named `Execution` value carrying the outcome alongside tokens, count
and command. It marks result-set presence at metadata production, independently
of row count and encoded bytes. Assignments and SELECT INTO remain statements;
query and FOR JSON results remain result sets even when no rows are returned.
DML OUTPUT remains an unsupported parser/lowering path; the explicit result-set
outcome is available for that future adapter implementation.

All 200 root library tests, formatting and strict workspace Clippy passed. A
separate probe executable matched 32 of the 36 client captures. The four OUTPUT
captures retain parser/lowering failures; several decoded command fields also
remain different. The permanent client regression passed for the 32 matched captures, alongside
the IF/WHILE and TRY/CATCH regressions, using the separate probe executable.
Full workspace/client/audit verification of this snapshot is pending. The comparison artifact is
`artifacts/compatibility/nocount-completions-comparison.json`. It preserves all
metadata, row-order, error and decoded command differences; successful client
observations alone do not prove byte-for-byte TDS fidelity or SQL compatibility.


The NOCOUNT snapshot passed formatting, all 400 workspace Rust tests and strict
workspace Clippy. All 314 local audit captures completed: compared with the
preceding 313-case Linux TRY/CATCH capture, 312 are unchanged, one unordered APPLY
result reverses row order, and the NOCOUNT probe is new. The full client run is
still active. Raw differences are in
`artifacts/compatibility/nocount-audit-diff.json`.


The complete local NOCOUNT client run finished successfully: 380 tests passed
with no failures or cancellations. Together with 400 Rust tests, formatting,
strict Clippy and 314 audit captures, this verifies the NOCOUNT snapshot before
the subsequent OUTPUT parser and execution work.
