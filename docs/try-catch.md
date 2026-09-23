# TRY/CATCH runtime recovery

Paired BEGIN TRY/END TRY and BEGIN CATCH/END CATCH blocks share batch variables.
Success skips the handler. A catchable runtime error discards the remaining try
body and enters the nearest active catch without emitting a client ERROR token.
Handled batches retain normal SQL-batch DONE and RPC return-status completion.

Nested catches restore the outer error after the inner handler finishes.
BREAK, CONTINUE and RETURN unwind handler context. Bare THROW inside a catch
preserves its error number, message and state and can reach an outer handler.
Outside a catch, ERROR_* functions return typed NULLs and bare THROW is rejected.
ERROR_NUMBER, ERROR_STATE and ERROR_MESSAGE expose the caught error. ERROR_SEVERITY retains the caught diagnostic severity; ERROR_LINE is fixed at 1,
and ERROR_PROCEDURE is NULL.
Exact source positions and procedure attribution remain unimplemented.

Parsing and variable preflight run before execution and cannot be caught.
Same-level missing-object and backend binder/parser/catalog errors escape the
handler, as do unsupported features and interpreter resource-limit errors.
Structured application THROW errors remain catchable regardless of message text.
Backend classification still uses message heuristics and is incomplete.

DuckDB can abort an explicit transaction after a runtime failure. A handler can
ROLLBACK before querying its error context; automatic SQL Server transaction
recovery, XACT_STATE, savepoints and XACT_ABORT ON are not implemented.

Tedious coverage includes successful try blocks, conversion/division/unique-key
errors, Unicode error messages, nested contexts, rethrow, loop exits, RETURN,
missing objects and connection reuse. Tiberius checks SQL-batch completion and
rollback from a catch after a constraint failure. These are local behavior tests,
not a differential compatibility result against a live SQL Server.

Reference: [Microsoft TRY/CATCH documentation](https://learn.microsoft.com/en-us/sql/t-sql/language-elements/try-catch-transact-sql).


## ERROR_* declarations

The SQL crate now declares ERROR_NUMBER/STATE/SEVERITY/LINE as nullable INT,
ERROR_MESSAGE as nullable NVARCHAR(4000), and ERROR_PROCEDURE as nullable
NVARCHAR(128). These declarations do not depend on whether a CATCH is active.
Projection, conditional operand inference, result properties and runtime lowering
share the same declaration function. Direct expressions carry computed/nullable
flags; CTE outputs retain derived-column provenance.

Seven live probes in `reference/error-function-metadata.json` cover outside and
inside CATCH, CTE projection, ISNULL/COALESCE and invalid signatures. The existing
session-function preflight validator now checks ERROR_* calls too, rejecting
invalid calls even inside an unreachable branch before earlier writes execute.
All 371 workspace Rust tests, strict Clippy, formatting and three focused
client tests pass. Full local client verification is running; a fresh audit
is queued after it. Complete TRY/CATCH completion sequences, real
line/procedure attribution and unpaired UTF-16 in ERROR_MESSAGE remain open.


`artifacts/compatibility/error-functions-comparison.json` records the new seven
probes and a rerun of all 82 RAISERROR probes. Six of the seven new captures match
completely; the CATCH capture now matches every result column and value, with
only completion-token differences remaining. The earlier RAISERROR CATCH
captures also lose their ERROR_* metadata differences. Their completion-sequence
gaps remain, so the earlier whole-capture count stays 69/82.

The previous local 305-case audit spanned a binary rebuild and is preserved as
`artifacts/compatibility/raiserror-mixed-audit.json`; it is not a single-version
baseline. The remote runtime verification remains on its synchronized source
snapshot without these metadata changes. The next local audit must complete
before comparison of the final metadata snapshot is claimed.
