# Integer UPDATE assignments

UPDATE assignments to TINYINT, SMALLINT, INT and BIGINT columns now use the
shared integer-conversion path. Numeric fractions truncate before range checks;
numeric overflow reports 8115. Character integer syntax, empty-space inputs,
NULL, and known money rounding follow the explicit-cast rules. DEFAULT resolves
the stored default expression, or NULL when no default exists.

The engine reads physical target types from the backend catalog and wraps each
integer assignment expression. It leaves the backend UPDATE responsible for
row selection and atomic application. Prepared parameter values stay bound.
Known money provenance is captured before type translation. CTE-wrapped UPDATE
receives the same conversion and emits UPDATE completion rather than a query
result containing the affected-row count.

Tests cover all four widths, exact BIGINT boundaries, unchanged rows and labels
after a failed multi-row update, defaults, NULL, money RPC input, prepared reuse,
CTE/FROM sources, invalid character input, explicit rollback and zero matched
rows. Tiberius independently verifies CTE UPDATE completion and converted values.
The copied T-SQL DML reference supplies the assignment, DEFAULT, FROM and
statement-failure semantics used here.

UPDATE target aliases can now resolve to a physical table in a FROM entry or
flat INNER/CROSS join tree, including when the target occurs later in the join
chain. The rewrite removes that target occurrence from FROM and preserves its
alias on the updated table. ON predicates move into parenthesized WHERE
conjunctions, retaining OR grouping. Multiple matching target occurrences are
rejected. Outer/lateral joins in the target tree are explicitly unsupported.

Alias tests cover quoted aliases, target-first and target-later joins, OR
filters, direct target-column expressions, prepared overflow recovery, CTEs
and rollback. Tiberius independently exercises a CTE with an aliased target.

Compound column assignments accept +=, -=, *=, /=, %=, &=, |= and ^=.
The engine expands them with the catalog type of the current column and groups
the entire right-hand expression. Known integral operands use integer division
and report 8134 for zero divisors; known string operands concatenate with +=.
Tests cover all eight operators, prepared aliases, CTEs, statement rollback
after division by zero, transaction rollback and right-hand precedence.
These operators follow the documented
[T-SQL compound assignment forms](https://learn.microsoft.com/en-us/sql/t-sql/language-elements/compound-operators-transact-sql).

Remaining work includes nested/outer/lateral target join trees, full schema
resolution, updatable views/CTEs, variable assignment within UPDATE,
OUTPUT, non-integer widths, stored money provenance, exact error
states/messages and SQL Server transaction behavior after failures. Native
DuckDB behavior still determines unsupported source-type combinations and
multiple-match UPDATE FROM results. Compound arithmetic with untyped source
columns still lacks complete numeric promotion and division diagnostics.
Live differential validation is outstanding.
