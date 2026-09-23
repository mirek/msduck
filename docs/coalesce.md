# COALESCE

COALESCE retains the first-non-NULL behavior of the backend function. When all
argument types are known integers, characters or NULL and at least one is
integral, arguments use the highest integer type before backend binding. This
shares the CASE/IIF precedence and integer-conversion path. Result typing also
propagates through nested COALESCE, CASE, IIF and CHOOSE expressions.

An argument list containing only untyped NULL constants fails preflight with
4127. Typed NULL casts and declared NULL variables remain valid. Aggregate and
window modifiers are rejected. Prepared parameters remain bound, and a failed
character conversion does not prevent subsequent execution of the handle.

Tests cover typed/untyped NULLs, known character/integer inputs, invalid numeric
text, spaces, exact BIGINT values, Unicode strings, nesting and prepared reuse.
These rules follow Microsoft's [COALESCE description](https://learn.microsoft.com/en-us/sql/t-sql/language-elements/coalesce-transact-sql).

Full source-column/function inference, other type families, widths/collations,
nullability metadata and exact arity diagnostics remain unfinished. Evaluation
counts and concurrency effects for subqueries or volatile expressions still
follow DuckDB; this does not reproduce every SQL Server COALESCE-to-CASE rewrite.
Live differential validation remains outstanding.
