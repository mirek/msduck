# Conditional batch execution

IF/ELSE and plain BEGIN/END blocks execute through a stack of statement
references. Conditions use the current bound variables and DuckDB's Boolean
result type. TRUE selects the IF branch; FALSE and UNKNOWN select ELSE, or
execute nothing when there is no ELSE. Numeric conditions return error 4145.
Nested branches, scalar subqueries and EXISTS use the normal expression path.

A BEGIN/END block groups statements; it neither starts a transaction nor creates
a variable scope. sqlparser represents that block as StartTransaction with
has_end_keyword=true, so the interpreter distinguishes it from BEGIN TRANSACTION
before dispatching transaction work. Paired TRY/CATCH blocks use a distinct
modifier and exception body; see [error handling](try-catch.md).
Transaction/exception modifiers are validated across the batch before execution.
Unpaired END TRY/CATCH, standalone END, chained SQL COMMIT/ROLLBACK, and
unimplemented SQL transaction modes cannot silently start, commit or roll back
a transaction. Malformed TRY/CATCH pairs return syntax errors; unsupported
transaction forms return feature errors. Driver transaction-manager restart flags
continue to use their separate implemented path.

A visitor checks declarations and variable references before execution, including
unselected branches. Duplicate names return 134 and missing references return
137 before preceding writes run. Declared variables start as typed NULLs in the
batch scope; an executed DECLARE then evaluates its initializer. Thus a variable
in an unselected DECLARE branch remains visible afterward with NULL value.
This is not full SQL Server batch compilation: table/column resolution and other
semantic checks still occur when the corresponding statement executes.

Only executed ordinary statements emit completion tokens or results. The final
SQL-batch DONE has MORE cleared even when trailing branches execute nothing.
A batch with no executed leaf gets an empty final completion. RPC execution
retains its return-status/DONEPROC ending. Conditions themselves preserve the
current row count; exact SQL Server token and row-count behavior still needs
an oracle comparison.

Tests through tedious cover nested branches, UNKNOWN, branch-local declarations,
block variable visibility, DML selection, no-result batches, EXISTS, malformed
conditions, duplicate names and missing variables before writes, and recovery.
Tiberius additionally verifies final skipped branches, empty SQL batches and
transaction counts. The copied T-SQL language-elements skill supplied the
conditional/block reference.

WHILE/BREAK/CONTINUE are described in [loop behavior](loops.md).
Remaining work includes procedural
prepared RPCs, complete predicate coercion/type rules, and differential checks
against SQL Server. Active-query cancellation remains unimplemented.
