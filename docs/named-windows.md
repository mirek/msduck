# Named window resolution

The query translator expands WINDOW definitions into inline OVER specifications
before ranking, aggregate and frame validation. Forward and backward references
are supported. Components can be added but not redefined; redefinitions report
4123. Cycles, undefined names and duplicate names fail explicitly. Resolution
walks dependency chains iteratively, with cached results, avoiding recursive
stack growth. Definitions containing same-level window functions are rejected
before expansion to prevent recursive expression growth.

Definitions belong to their SELECT scope. A basic query's ORDER BY shares that
scope; subqueries, CTEs and set-operation branches resolve independently. Names
currently follow the server's case-insensitive identifier convention. Expansion
removes the WINDOW clause, so stored views retain resolved specifications.
Prepared statements repeat expansion and binding without executing the query.

Inherited missing ordering, forbidden ranking frames, RANGE offsets and nested
windows use the same diagnostics as inline specifications. Tests cover chains,
forward references, query ORDER BY, subquery shadowing, prepared queries, CTE
output types, stored views and invalid definitions. Source-free SELECT queries
also recognize WINDOW definitions after an unaliased projection, including
nested queries and set branches. Lookahead preserves ordinary aliases named
WINDOW and quoted identifiers.

Remaining gaps include compatibility-level gating, full identifier-collation
rules, exact diagnostics for missing/duplicate/cyclic names, and exhaustive
validation of unused window definitions and remaining aggregate/window placement
contexts.

Reference: Microsoft [WINDOW clause](https://learn.microsoft.com/en-us/sql/t-sql/queries/select-window-transact-sql).

A batch preflight pass reports 4108 for same-query windows in WHERE, HAVING,
GROUP BY, FROM/join expressions, TOP/paging expressions, VALUES, UPDATE targets
and predicates, DELETE predicates, and CREATE TABLE column/constraint
expressions. It runs before prior batch writes. Nested query boundaries are
preserved, permitting ranking in a derived table or scalar subquery used by
an outer predicate. Procedural expression and remaining DDL contexts still need
broader placement coverage. Reference: Microsoft
[error 4108](https://learn.microsoft.com/en-us/sql/relational-databases/errors-events/database-engine-events-and-errors-4000-to-4999).
