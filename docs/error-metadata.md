# Result metadata around execution errors

The server now preserves available scalar result metadata when native query
execution fails. It describes prepared columns before `query_arrow`, carries
the encoded prefix as error context, and emits it before an uncaught error or
before a TRY/CATCH handler result. A caught failed query receives a completion
token before the handler's columns. Successful results continue to use executed
Arrow schemas for decoding.

The vendored DuckDB Rust wrapper now exposes `Statement::prepared_columns()`.
It owns names and logical type handles and uses prepared-statement C API
metadata without executing the statement. It is fallible when parameter types
are unresolved. It does not replace executed Arrow metadata for decoding.
Three integration tests establish ownership after dropping the statement,
absence of sequence evaluation, parameter-type handling, and availability of
DECIMAL(13,8) metadata before a native division callback raises an error.

The mapper preserves logical names and properties and declared character,
binary, time and currency types where available. Supported native scalar types
include decimal, integers, floats, Boolean, date/time, strings, binary and UUID.
Unknown/unresolved shapes, unsupported containers, FOR JSON and assignment-only
queries currently do not acquire a prefix. No partial column list is fabricated.
The prefix and batch buffer are bounded by the existing response-size limit.

`reference/error-metadata.json` preserves four live SQL Server captures:

- TRY/CATCH division by zero emits empty failed-query metadata before the
  handler's result, with completion events retained and no uncaught error.
- An uncaught division error between two SELECTs preserves both successful
  results and the failed query's metadata. The final SELECT still executes in
  the tested default session. The current server stops the batch instead.
- TRY/CATCH decimal AVG overflow likewise preserves its failed result shape.
- sp_executesql preserves the failed query's shape and uses procedure completion
  tokens around the error.

`artifacts/compatibility/error-metadata-before.json` preserves the local
baseline. These traces establish default-session behavior for the tested
errors, not a universal rule that all errors continue execution. Compile errors,
XACT_ABORT, transaction recovery, partial rows, streaming failures, JSON results
and output-size limits need their own handling and verification.

The prepared-description API passed all 362 workspace Rust tests, strict
workspace/all-target Clippy and formatting. That API-only snapshot preceded the response-path integration below.

The first integrated comparison is retained in
`artifacts/compatibility/error-metadata-decimal-comparison.json`. Four standalone
AVG overflow captures now match SQL Server completely (six of the full 29 AVG
captures match). Division errors have the correct failed-query metadata and
diagnostics; their companion CONVERT columns still differ in flags. All eight
aggregate-expression captures remain exact. The prior diagnostic comparison
file was refreshed during this run; use this explicitly named metadata capture
for the integrated result.

`artifacts/compatibility/error-metadata-after.json` records the control-flow
comparison including the caught-query completion token. Failed metadata
is present for direct and caught queries. Remaining gaps include full TRY/CATCH
completion sequencing, ERROR_NUMBER/ERROR_STATE result flags, uncaught-error
batch continuation, and the textual sp_executesql parameter-binding case.
The metadata change alone does not establish SQL Server error-control-flow
compatibility or partial-row streaming behavior.

The response integration passed all 363 workspace Rust tests, strict Clippy,
formatting and three focused client tests. The full local client suite subsequently
passed all 367 tests with no failures, cancellations or skips. Its following audit
rebuilt newer sources and completed 303 cases, including arithmetic continuation;
the audit therefore does not represent the original metadata-only snapshot.
Full remote continuation verification also passed and includes the metadata work.

Arithmetic query continuation is now implemented for the described query path;
see `docs/error-continuation.md` for reference comparisons and remaining limits.
