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

## Required integration

The server registers the observer once and carries the matching registry into
its connection wrappers and sessions. Public SQL is not rewritten yet. This
change does not emit warning 8153 or complete the aggregate-warning feature.
The remaining integration must create a scope for each statement that requires
observation. It must preserve statement scope through internal helper
queries, collect the flag after execution, apply the session's ANSI_WARNINGS
policy, and emit at most one warning before that statement's DONE token. See
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

Native regressions exercise a materialized 6000-row volatile source, empty and
all-NULL inputs, HAVING without result rows, concurrent windowed queries on
cloned connections, preparation and reuse, malformed/stale tickets, bounded
allocation and unwind cleanup. They validate this execution mechanism, not
complete SQL Server equivalence or final planner placement.
