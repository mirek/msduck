# Window frame boundaries

A syntax pass before batch execution checks offsets in inline windows and all
named definitions, including unused definitions. ROWS offsets must be unsigned
integer literals. Variables, column references, subqueries, arithmetic,
parentheses, signs, fractional/scientific literals and strings fail with syntax
error 102 (severity 15). Earlier batch statements do not execute after this
syntax failure. Prepared queries use the same pass before parameter binding.

After named-window expansion, frame validation checks ordering, rejects RANGE
offsets (4194), and rejects reversed boundaries (4193). Decimal digit comparison
avoids narrowing large offsets during ordering checks. Leading zeroes are
accepted. Implicit frame ends use CURRENT ROW; a short FOLLOWING frame is
therefore rejected. Existing function-specific frame restrictions still apply.

Tedious tests cover positive preceding/following frames, empty frames at
partition edges, SUM NULL versus COUNT zero, syntax failures, named frames,
prepared rejection and prevention of earlier batch writes. The audit records
forward-frame results followed by a reversed-frame error for later comparison.

Remaining work includes SQL Server reference confirmation of exact diagnostic
text/precedence, accepted maximum offset size, zero-offset direction edge cases,
and consistent diagnostics for shapes the upstream parser rejects first.

References: Microsoft [OVER clause](https://learn.microsoft.com/en-us/sql/t-sql/queries/select-over-clause-transact-sql)
and [error 4193](https://learn.microsoft.com/en-us/sql/relational-databases/errors-events/database-engine-events-and-errors-4000-to-4999).
