# Statement-scoped native diagnostics

`statement_diagnostics::Registry` is explicit root-adapter state. A database
owner creates it, registers its observer once, and retains a clone. Each
execution opens a fresh `Scope`. Registered native callbacks share the registry
but never share one statement's warning flag with another statement.

Scopes use random 128-bit tickets from the operating system cryptographic random
source. The registry bounds active scopes at 4096 and rejects collisions rather
than replacing an entry. A scope drop removes its ticket, including Rust unwind
paths; stale or malformed tickets fail native calls. No global registry, current
session variable, thread-local flag, or second execution of the user's SQL is
used. Registry poisoning fails closed, while cleanup avoids panicking again.

`__msduck_observe_null(ticket, eliminated)` is a volatile, special-NULL-handling
native function returning its BOOLEAN input unchanged. A true input records
NULL elimination. A NULL BOOLEAN returns NULL without recording elimination;
the intended binder-produced `value IS NULL` input is always a non-NULL BOOLEAN.
Preparing a call does not observe anything. Reusing a prepared call requires a
fresh active ticket for every execution. The callback validates the 16-byte
ticket before reading its bytes, caches one resolved ticket per vector, and
retains no native pointers after returning.

## Execution integration

The server registers the observer once and carries the matching registry into
its connection wrappers and sessions. With ANSI_WARNINGS ON, statement execution
opens a scope, instruments supported aggregate expressions and binds its ticket.
Successful execution emits at most one warning 8153 before the statement's DONE
token when the scope recorded NULL elimination. Internal scalar queries used by
SET and DECLARE share their statement's scope. With ANSI_WARNINGS OFF, execution
opens no diagnostic scope and leaves aggregate expressions uninstrumented.
See [aggregate diagnostic integration](aggregate-diagnostics.md) for the exact
reference coverage and remaining gaps, including stored views, specialized DML
and warnings from partially executed failures. These mechanisms do not establish
complete aggregate-warning compatibility. See
[the warning reference contract](https://github.com/mirek/msduck/blob/8e399bb/docs/aggregate-warnings.md)
for captured SQL Server behavior.

The binder must materialize an aggregate operand once before referring to both
its value and its NULL flag. For example, a MATERIALIZED input binding can feed
`CASE WHEN __msduck_observe_null(ticket, v IS NULL) THEN NULL ELSE v END`.
Applying that expression directly to a volatile user expression in both places
would evaluate it twice and is forbidden. Placement must reflect actual
aggregate inputs after filtering and match reference window-frame behavior;
a generic pre-scan of a table is insufficient. Counts of rows and constants must not observe unrelated
nullable columns. TOP(0), empty inputs, correlated execution, DISTINCT, errors,
cancellation and DML consumers need reference-driven integration coverage.

COUNT windows use a separate native aggregate, `__msduck_count_frame`, returning
the count and a NULL-elimination flag as a STRUCT. Its fixed-size state contains
no frame values and performs no statement observation during update or combine.
The scalar observer consumes the flag only from a returned frame result, so
unused intermediate window states cannot emit warnings. Integer and money
SUM/AVG windows use the same paired-result mechanism, retaining their typed
values and bounded arithmetic. Other window aggregate families currently collect
frame values with LIST; their wide-frame time and memory cost remains unresolved.
Grouped aggregates use a singleton binding to evaluate the operand once before
observing its NULLness.

Native regressions exercise a materialized 6000-row volatile source, empty and
all-NULL inputs, HAVING without result rows, concurrent windowed queries on
cloned connections, preparation and reuse, malformed/stale tickets, bounded
allocation and unwind cleanup. They validate this execution mechanism, not
complete SQL Server equivalence or final planner placement.
