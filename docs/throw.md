# Explicit THROW errors

THROW evaluates its error number, message and state through bound expressions.
It requires a non-NULL INT error number of at least 50000, a non-NULL message
of at most 2048 UTF-16 units, and a state from 0 through 255. Doubled percent
signs become literal percent signs. A structured SqlError retains the supplied
number and state instead of using backend-message heuristics. TDS emits an ERROR
token at severity 16 followed by an error completion when no enclosing catch
handles the error. See [TRY/CATCH](try-catch.md) for recovery and bare rethrow.

The driver test checks both integer boundaries, state 0 and 255, Unicode and
escaped percent signs, bound message text, throwing from a loop, skipped writes,
invalid arguments, and connection reuse. A message deliberately containing
"does not exist" confirms that an application error does not become backend
error 208. With the currently supported XACT_ABORT OFF setting, prior autocommit
writes remain and an explicit transaction stays available for caller rollback.

Remaining work includes XACT_ABORT ON, strict argument grammar and semicolon requirements, exact invalid-argument
errors, line/procedure attribution, and SQL Server differential validation.
Bare THROW is rejected outside a catch context. Unsupported
printf-style formatting and individual percent-sign edge cases still need
SQL Server validation; this does not implement RAISERROR formatting.

Reference: [THROW](https://learn.microsoft.com/en-us/sql/t-sql/language-elements/throw-transact-sql)
and the copied T-SQL transactions-and-error-handling skill reference.
