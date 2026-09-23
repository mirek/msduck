# Control-flow completion evidence

SQL Server 2025 RTM-CU7 captures in `reference/control-completions.json`
and `reference/control-completions-rpc.json` cover 19 statements each, using
SQL batches and sp_executesql RPCs respectively. The pinned image and server
version are recorded in each fixture. `reference/control-completion-tokens.json`
also retains decoded token fields, including the current command code.

Each successful IF or WHILE condition evaluation emits an uncounted completion,
including false conditions and the final WHILE evaluation. The command code is
0xC0. SQL batches use DONE; RPCs use DONEINPROC unless NOCOUNT is on. Ordinary
BEGIN/END grouping adds no token. Evaluating a condition does not change the
session's affected-row count. The imperative batch interpreter owns these events;
no database or protocol effects enter the deterministic parser crate.

The client regression compares complete captures for 13 SQL-batch and 12 RPC
cases: true/false IF, a skipped final statement, grouped branches, an unentered
loop, a two-iteration loop, and batch NOCOUNT. It also checks the condition command
code and number of evaluations. The raw comparison of all 38 captures is saved
in `artifacts/compatibility/control-completions-comparison.json`; unresolved cases
are retained rather than filtered out of that artifact.

TRY/CATCH remains incomplete. The token-level captures distinguish BEGIN TRY
(349), entry into CATCH (350), and normal exit (351). A caught RAISERROR also
contributes a completion with command 246. These need explicit interpreter
boundary events and correct unwinding for nested handlers, RETURN and loop
control. RPC NOCOUNT suppression of other non-result completions is also open.
The existing PRINT command code differs from the captured 247 even though the
ordinary client capture does not expose that field. Constant-condition errors
and their completion behavior require separate evidence.

Validation: formatting, strict workspace Clippy and all 400 Rust tests passed.
The final 25-case client regression also passed, including the raw condition
command assertions. Full client and audit runs are in progress. This is not a
claim of full control-flow or SQL Server compatibility.

## Handler unwinding reference matrix

`reference/try-completion-tokens.json` and
`reference/try-completion-tokens-rpc.json` add 16 cases each, retaining both client
captures and decoded DONE fields. They cover nested catches, a bare rethrow,
RETURN from TRY and CATCH, BREAK and CONTINUE from each, empty handlers,
arithmetic errors, and NOCOUNT. The baseline comparison is preserved in
`artifacts/compatibility/try-completions-before.json`.

Observed token commands and effects:

| Operation | Command | Completion behavior |
| --- | ---: | --- |
| Enter TRY | 349 | Uncounted completion before its body |
| Enter CATCH | 350 | After the caught statement's completion |
| Normal handler exit | 351 | After the selected body, even an empty CATCH |
| Caught RAISERROR or THROW | 246 | Uncounted completion without the error flag |
| Caught divide-by-zero query | 193 | Zero-row completion without the error flag; result metadata remains visible |
| RETURN | 219 | Completes the return and abandons pending handler exits |
| BREAK or CONTINUE | 202 | Completes the control transfer and abandons handler exits crossed by that transfer |

A rethrow into an outer handler produces a THROW completion and outer CATCH
entry, without an end token for the abandoned inner handler. Normal nested
handler exits each produce their own end token. NOCOUNT suppresses non-result
DONEINPROC tokens in RPC execution; SQL-batch boundary tokens remain visible.
An entirely empty TRY is a syntax error, while an empty CATCH is accepted.

These observations suggest retaining the interpreter's existing error-context
restoration during unwinding, while adding explicit completion events on normal
entry/exit and on control transfers. Unwinding must discard abandoned completion
events rather than emit them. The baseline also exposes a separate incorrect
RPC return status for RETURN inside CATCH. Rows and other diagnostics match for
the nonempty captured programs; this does not establish error precedence for
unprobed statements or arbitrary procedure bodies.


## Handler completion implementation

The root interpreter now emits entry and normal-exit completions explicitly.
A scheduled CATCH-entry event runs after the failed statement's completion.
Existing stack unwinding restores error context and discards abandoned exit
events, so nested rethrows, RETURN, BREAK and CONTINUE preserve the observed
ordering. PRINT, variable assignment, RETURN and loop-control command fields
also use the captured SQL Server values. Bare RETURN uses the current @@ERROR
value; successful statements inside CATCH can clear it before RETURN.

`reference/catch-return-rpc.json` adds four RPC probes for RETURN immediately
after THROW, after PRINT, after SELECT, and with an explicit value. The focused
client test checks complete observations and decoded completion fields for 33
supported programs across the three fixtures. All 33 passed on Linux. The full
32-case handler comparison now matches 30 client captures and token streams;
the two empty-TRY syntax cases remain mismatches. Value-bearing top-level RETURN
and broader RPC NOCOUNT suppression remain open. The Linux workspace checks passed all 400 Rust tests, formatting and strict
Clippy after updating two runtime prefix assertions and retaining the binding
error assertion. Full client and audit verification is running. A subsequent
local correction records failed runtime leaves as executed when guarding early
binding failures; all 200 root library tests and strict workspace Clippy passed
for that correction. Subsequent NOCOUNT work is tracked in docs/nocount.md.


`reference/try-binding-error.json` confirms that both literal BIT negation and
BIT negation through a VALUES column fail before a TRY-entry completion. When
the first executable leaf fails binding, the interpreter discards speculative
boundary tokens. This preserves the original compile-error test. Compilation
of later statements still lacks a unified whole-batch binding phase.


The frozen Linux TRY/CATCH snapshot completed all 400 Rust tests, formatting,
strict Clippy, 379 client tests and 313 audit captures. It precedes the final
runtime-leaf bookkeeping correction and the NOCOUNT outcome contract. Against
the preserved 308-case macOS baseline, 284 captures are unchanged and five are
new. The 24 changed captures contain only completion events, the already
verified LEN metadata flags, and one unordered APPLY row reversal. Raw deltas
are retained in `artifacts/remote/linux.local/try-completion-audit-diff.json`.
