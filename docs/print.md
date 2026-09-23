# PRINT informational messages

PRINT evaluates its expression once, converts it to the appropriate character
family and emits a severity-zero INFO token (number 0, state 1). It produces no
rows, resets @@ROWCOUNT and @@ERROR, and lets the batch continue. Live SQL Server
captures establish that both NULL and empty input produce a one-space message.
Embedded NULs remain in the message. INFO output is buffered with the batch.

Known Unicode sources use a 4,000 UTF-16-unit limit; known ANSI sources use
8,000 units after character conversion. Logical source inference recognizes
NCHAR as Unicode and retains function result families, including REPLICATE.
Unknown source types conservatively use the Unicode path. Under the captured
non-SC collation, truncation can retain an isolated high surrogate at the cutoff.
Core `print::message` implements truncation and blank-message policy over borrowed
units. Root evaluation preserves carrier values and the TDS codec writes their
units directly, without ANSI substitution or lossy Unicode decoding.

Prepared PRINT validation uses the same conversion target without executing the
statement. The tedious test verifies no message during preparation and an exact
surrogate message during execution. This exercises the supported prepared path;
it does not establish compatibility for every procedural preparation case.

`reference/print-unicode.json` contains twelve SQL batch captures and
`reference/print-rpc.json` contains three parameterized RPC captures. Fourteen of
fifteen complete captures match in
`artifacts/compatibility/print-unicode-comparison.json`. Every message matches;
the remaining difference is the DONE sequence for a skipped IF branch. No raw
differences were normalized. The captures cover NULL/empty input, NULs, high/low
surrogates, NCHAR padding, Unicode cutoff boundaries, ANSI limits, numeric text,
variable bindings and continuation.

Three focused tedious tests pass, including the updated legacy PRINT test,
Unicode binding regression and new PRINT/preparation test. The core PRINT test,
seven native carrier tests, strict workspace Clippy and formatting pass. The frozen binding/PRINT snapshot also passed all 400 Rust tests, strict Clippy
and formatting on Linux. Its full client and audit phases are still running.
Further work includes other collations, general source-type inference,
non-character conversion fidelity, best-fit code-page mappings, completion
boundaries and precise diagnostic source lines/procedures.

The upstream mssqlite PRINT interpreter emits message items from its scalar
evaluator. Its NULL-to-empty policy was not adopted as evidence; the SQL Server
captures above determine the blank-message behavior implemented here.
