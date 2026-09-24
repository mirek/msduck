# Session error state

@@ERROR reads the error number stored on the current Session. Expression
translation captures that number before statement execution; repeated reads
within one statement agree. A successful ordinary statement or RETURN resets
it to zero. An initializer can preserve the previous number before that reset,
for example `DECLARE @saved INT = @@ERROR`.

Batch parse/preflight errors, execution errors, condition/RETURN evaluation
errors and interpreter resource-limit errors update the state when emitted.
Explicit THROW records its structured application error number rather than a
message-derived backend mapping. Informational diagnostics do not set it.
The state is per connection and survives request boundaries. Protocol/RPC
validation failures outside the batch interpreter do not currently update it.

Tedious tests cover initial zero, custom THROW numbers, missing objects and
variables, syntax errors, repeated reads, variable initialization, reset after
SET, and informational RETURN NULL warnings. The existing copied T-SQL skill
supplied the immediately-previous-statement rule and cross-batch reference.

Full SQL Server error continuation/classification remains unfinished: current
unhandled batch errors stop execution. [TRY/CATCH](try-catch.md) supports
same-batch runtime recovery and sets @@ERROR on handler entry; subsequent
successful statements reset it. Precise statement-versus-batch behavior remains unfinished.
Condition and transaction-manager completion semantics need differential
validation; this feature does not establish complete SQL Server error handling.
