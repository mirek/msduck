# Batch-local scalar variables

Each SQL or RPC batch starts with a private copy of its input bindings. Scalar
DECLARE adds typed values to that scope, using NULL when there is no initializer.
SET evaluates an expression, converts it to the declared type, and updates the
binding only after successful evaluation. Later queries and DML bind the values
through DuckDB parameters. Variables do not survive the request and are not
rolled back with database transactions.

Expressions are interpreted by the existing AST translator and evaluated as a
single DuckDB SELECT. Scalar subqueries returning no row produce NULL; multiple
rows raise error 512. Missing variables return 137, and duplicate declarations
(including case variants and collisions with RPC inputs) return 134. Declarations and variable references are checked across the parsed batch,
including conditional branches, before executing any DML.
Other validation and runtime errors still stop the current batch; complete SQL
Server compile-time/runtime error classification remains unfinished.

Declared numeric types convert at assignment. TIME variables retain their scale
and seven fractional digits; their evaluated values are rebound as exact text
to avoid the native binder's microsecond truncation. Uninitialized values keep
the declared type when selected. Character widths, money-specific rules and
other known type-system gaps remain unchanged. SET updates @@ROWCOUNT to 1.

Independent tedious and tiberius tests cover declarations, multiple variables,
NULL metadata, typed assignment, arithmetic, scalar subqueries, SQL and RPC
batch isolation, DML binding with quote-containing text, case-insensitive names,
transaction rollback, duplicate declaration before DML, missing variables,
conversion errors, and connection reuse. These are local interoperability tests;
SQL Server differential validation remains required.

SELECT assignments execute the translated query internally, convert values to
the target variable types and retain the final row across Arrow batches. They
emit completion tokens without column metadata or row tokens. Zero rows leave
bindings unchanged; a scalar subquery with no match assigns NULL. Multiple
independent assignments, aggregates, ORDER BY and TOP are covered by driver
tests. @@ROWCOUNT reflects rows consumed. Mixing assignment and ordinary result
columns raises 141; quoted aliases such as [@label] remain normal output names.

Right-hand references use the bindings at statement entry. This does not promise
row-by-row accumulation or a specific evaluation order for dependent assignments;
SQL Server itself does not guarantee that order. The implementation and tests
follow the documented last-row/empty-row rules in
[SELECT @local_variable](https://learn.microsoft.com/en-us/sql/t-sql/language-elements/select-local-variable-transact-sql).
Prepared SELECT assignments now use the same typed lowering during validation,
without evaluating the query or changing bindings. Both ordinary and compound
assignments can update declared RPC inputs for later statements in that execution;
each new execution starts with fresh inputs. Tests cover prepare-time nonexecution,
CTEs, empty and NULL results, conversion, overflow recovery, sp_prepexec reuse,
and missing-target/mixed-output validation. Application OUTPUT parameter tokens
remain unsupported. Assignment inside subqueries/set operations remains
unsupported. Error-side effects still require differential validation.

Compound SELECT operators use the same assignment path; see
[compound assignments](compound-assignment.md) for coverage and limitations.

Not yet implemented: table/cursor variables, stored procedure locals, application OUTPUT
parameters, full SQL Server variable type restrictions, and full batch compilation.
Prepared RPC batches accept scalar DECLARE and SET alongside SELECT/DML;
initializers are compiled without evaluation and local bindings are recreated
on each execution. Supported control-flow bodies are checked without executing
their branches or loops; see [prepared RPCs](prepared-rpc.md).
The copied T-SQL skill's `language-elements.md` supplied the variable-scope
and scalar-assignment reference; its broader implementation claims describe
upstream mssqlite, not msduck.
