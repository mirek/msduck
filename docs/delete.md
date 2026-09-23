# DELETE coverage

DELETE accepts an optional FROM before the target and a second FROM for
source tables. Target aliases in flat INNER/CROSS join trees resolve to their
physical table, sharing the UPDATE resolver. The translation uses DuckDB
DELETE USING and moves target-tree ON predicates into parenthesized WHERE
conjunctions. It preserves OR grouping and deletes each matching target row
once even when several source rows match.

CTE-wrapped DELETE executes as DML: it sends affected-row completion counts
without exposing DuckDB's count result column. @@ROWCOUNT remains available
with NOCOUNT enabled. Prepared parameters stay bound. Explicit transaction
rollback restores deleted rows.
Foreign-key constraint failures report error 547 and preserve the statement's
original rows; backend message text remains an approximation.

The implementation follows the forms documented in Microsoft's
[DELETE reference](https://learn.microsoft.com/en-us/sql/t-sql/statements/delete-transact-sql)
and the copied T-SQL DML skill. Tests exercise optional/two-FROM syntax,
target-first and target-later joins, duplicate matches, OR grouping, CTEs,
prepared aliases, zero matches, rollback, NOCOUNT and failed foreign-key deletes.
Tiberius independently checks CTE completion and @@ROWCOUNT.

Remaining work includes TOP, OUTPUT, writable CTE/view targets, target join
trees with outer/lateral/nested joins, complete object resolution, triggers,
cursors, hints, and SQL Server transaction/error semantics. Unsupported target
join kinds fail explicitly. Live SQL Server differential validation remains
outstanding.
