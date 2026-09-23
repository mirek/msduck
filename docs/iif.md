# IIF

IIF accepts three scalar arguments and lowers to a searched CASE expression.
Its first argument must be a search condition; TRUE selects the second
argument, while FALSE or UNKNOWN selects the third. Scalar BIT values require
an explicit comparison. Arguments remain AST expressions and parameters remain
bound, including during prepared execution.

Two untyped NULL result constants fail with 8133. Typed NULL variables remain
valid and retain their result type. Combined CASE/IIF nesting is limited to ten
levels, with error 125 beyond that limit. Aggregate/window modifiers on IIF
are rejected rather than discarded.

Driver tests cover true/false/unknown predicates, Unicode results, BIGINT
promotion, an unselected scalar division error, typed NULLs, invalid predicates,
arity, nesting limits, table data, CHECK expressions and prepared rebinding.
The implementation follows Microsoft's [IIF description](https://learn.microsoft.com/en-us/sql/t-sql/functions/logical-functions-iif-transact-sql).

Known integer/character branches use [SQL Server integer precedence](case-types.md).
Other result coercion still follows the backend CASE implementation. Full SQL Server
type precedence, character widths/collation, mixed-type parameters, decimal
precision, aggregate evaluation order and exact diagnostics remain unfinished.
The scalar branch test does not promise short-circuit evaluation for every
expression. Live SQL Server differential validation is still required.
