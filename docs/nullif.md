# NULLIF

NULLIF lowers to a searched CASE: equal arguments produce NULL; otherwise the
original first argument is returned. Comparison conversion is separate from result
typing, so comparing a character first argument with an INT can return the original
character value, including leading zeroes, when unequal.

Known integer/character comparison arguments use the highest integer type and the
shared integer conversion path. Invalid fractional numeric text raises 245 instead
of being rounded into an integer by DuckDB. The first argument determines result
metadata, including for NULL and empty results. A literal NULL first argument
fails batch preflight with 4151; typed NULLs remain valid. Argument shape and
modifiers are validated during preparation as well as execution.

These rules follow Microsoft's [NULLIF reference](https://learn.microsoft.com/en-us/sql/t-sql/language-elements/nullif-transact-sql)
and [error catalog](https://learn.microsoft.com/en-us/sql/relational-databases/errors-events/database-engine-events-and-errors-4000-to-4999).
Tedious tests cover NULL/equal/unequal inputs, prepared parameter reuse after errors,
comparison conversion without result conversion, nested COALESCE, table columns,
metadata, and preflight failure before earlier batch writes.
Known integral logical-function results also feed arithmetic type inference:
`7 / NULLIF(@n, 0)` uses integer division for an INT parameter and returns NULL
when the denominator becomes NULL. Prepared tests exercise this guard.

Known DATETIME2 arguments compare exact 100-nanosecond ticks across scales,
while the result retains the first argument's scale. Source-column annotation
and result inference include NULLIF, so these rules also apply through CTEs
and nested COALESCE. Tests cover all 64 scale pairs, equal/unequal/NULL values,
year 1 and year 9999, empty result metadata, and prepared conversion errors
followed by successful reuse. The local audit records a cross-scale probe;
SQL Server differential validation has not yet run.

Full column/function type inference, other comparison type families, character
padding and collations, precise width/nullability metadata, implicit CASE nesting
limits and volatile-expression evaluation remain unfinished. Unknown comparison
types still use DuckDB coercion. Live SQL Server differential validation remains
outstanding.


Known integer variant arguments compare by numeric value across base tags while
retaining the original first payload type on unequal values. Variant result
inference feeds enclosing CASE/COALESCE/ISNULL and CTE predicates. The first
argument's SQL type still controls the result when it is an ordinary integer.
This path retains searched-CASE evaluation; do not assume one evaluation of
volatile NULLIF inputs. See [variant limits](identity.md).
