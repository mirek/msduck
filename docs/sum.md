# Integer SUM result types

SUM inputs with a known integer type now return INT for TINYINT/SMALLINT/INT
and BIGINT for BIGINT. The same descriptors hold for NULL and empty groups.
Known types include explicit casts, typed parameters, integer literals and the
integer expressions recognized by the shared type inference pass. A catalog
annotation pass also resolves integer columns from named tables and views,
including aliases, flat joins, and schema-qualified names. It runs before both
preparation and execution, and checks catalog types again after schema changes.

Each SELECT has its own scope, including set-operation branches and subqueries.
Query-level ORDER BY retains that query's source scope. CTE names shadow catalog
tables, duplicate column names remain ambiguous, and unknown source shapes
disable unqualified inference. Known integer projection types propagate through
CTEs and derived tables, including chained CTEs, plain and qualified wildcards,
column alias lists (including unnamed expressions), prepared inputs, aggregate
outputs (including COUNT, COUNT_BIG, MIN and MAX), and integer set-operation
branches. Recursive references remain unresolved rather than using catalog
objects of the same name. VALUES sources with explicit column aliases infer
integer types across rows, preserving the widest integer input and ignoring
untyped NULL rows. Mixed noninteger and all-untyped-NULL columns remain unknown.
Correlated outer references, table functions and general nested source forms
still need broader inference. An
explicit cast continues to supply a type for those expressions.

Known integer SUM now uses the bounded native implementation described in
[integer aggregates](integer-aggregates.md), together with integer AVG. The
previous final-result helper remains registered only for older persisted views.
Those views must be recreated to adopt bounded accumulation.

Native tests exercise positive/negative signed 128-bit conversion, NULLs across
vector boundaries, and both integer bounds. Tedious tests cover groups, empty
and all-NULL inputs, DISTINCT, running sums, HAVING, prepared parameters and
recovery, widening to BIGINT, TRY/CATCH, views, and outer expressions. The
upstream mssqlite sum-overflow tests supplied the INT_MAX + 1 case. A capture
probe records values and descriptors for future live SQL Server comparison.

Reference: Microsoft [SUM](https://learn.microsoft.com/en-us/sql/t-sql/functions/sum-transact-sql).

Source-column tests also cover source views, stored aggregate views, scalar
subqueries in variable declarations, CTE shadowing, joins, set branches,
preparation and widening an INT source column to BIGINT between executions.

VALUES inference follows integer type precedence described in Microsoft’s
[table value constructor reference](https://learn.microsoft.com/en-us/sql/t-sql/queries/table-value-constructor-transact-sql).
Client tests cover inline rows, aliases, CTE propagation, windows, NULLs, empty
inputs, bounded overflow and exact prepared BIGINT averages.

The shared expression inference also recognizes COUNT as INT and COUNT_BIG as
BIGINT before lowering, so CASE, COALESCE and mixed character arithmetic use
integer precedence. See Microsoft’s
[COUNT reference](https://learn.microsoft.com/en-us/sql/t-sql/functions/count-transact-sql).
Count overflow session options and complete nullability metadata remain separate
compatibility gaps.

MIN and MAX retain known integer input widths, including TINYINT and SMALLINT,
through surrounding expressions and CTE/derived outputs. Catalog annotation
also runs for these functions when no SUM or AVG is present. Their integer
calls reject unsupported modifiers, including DISTINCT with OVER; DuckDB
performs the extrema calculation. General character/collation and noninteger output inference remain unfinished. See Microsoft
[MIN](https://learn.microsoft.com/en-us/sql/t-sql/functions/min-transact-sql) and
[MAX](https://learn.microsoft.com/en-us/sql/t-sql/functions/max-transact-sql).

SUM, AVG, MIN and MAX reject known BIT arguments with error 8117, including
NULL/empty inputs, parameters, supported expressions and resolved source
columns. BIT types propagate through CTEs, derived projections, VALUES and
known set-operation branches; integer alternatives take precedence over BIT.
COUNT/COUNT_BIG and explicit integer casts remain valid. Unknown source shapes
and full compile-time error scope are still incomplete. References: Microsoft
[BIT](https://learn.microsoft.com/en-us/sql/t-sql/data-types/bit-transact-sql) and
[error 8117](https://learn.microsoft.com/en-us/sql/relational-databases/errors-events/database-engine-events-and-errors-8000-to-8999).

SUM, AVG, MIN, MAX, COUNT and COUNT_BIG validate argument shape and modifiers
before type-specific lowering. Wrong arity reports 174; DISTINCT with OVER
reports 10759 for all input types. Ordinary calls reject subqueries and known
nested aggregates with 130. Window aggregates may consume grouped aggregate
results, and separate derived/subquery levels remain valid. Unsupported FILTER,
ordered arguments and wildcard forms fail explicitly. Compile-time batch scope,
full aggregate placement validation remain gaps.
Error numbers follow Microsoft's [0–999 catalog](https://learn.microsoft.com/en-us/sql/relational-databases/errors-events/database-engine-events-and-errors-0-to-999)
and [10000–10999 catalog](https://learn.microsoft.com/en-us/sql/relational-databases/errors-events/database-engine-events-and-errors-10000-to-10999).
The current 130 severity follows the catalog (16); live-version fidelity remains
unverified.

The grouped-window validation regression exposed a DuckDB C API crash. The
pinned [native patch](../vendor/libduckdb-sys/MSDUCK-PATCH.md) flattens update and
combine state vectors before passing per-row pointers to Rust callbacks.

Window functions nested within aggregate arguments or another window's
arguments, partition expressions or ordering expressions report error 4109.
The check respects subquery boundaries and permits ordinary scalar wrappers,
separate derived-table levels and windows ordered by grouping aggregates.
This check runs before native aggregate renaming, including COUNT(*) windows.
Reference: Microsoft [error 4109](https://learn.microsoft.com/en-us/sql/relational-databases/errors-events/database-engine-events-and-errors-4000-to-4999).

ROW_NUMBER, RANK and DENSE_RANK outputs infer BIGINT before lowering, including
outer arithmetic and aggregate inputs through CTE/derived projections. These
functions validate zero arguments, required OVER and explicit ORDER BY, and
reject explicit window frames (174, 10753, 4112 and 4106 respectively). Named
window inheritance is resolved by the [query-level pass](named-windows.md); NTILE argument
semantics and full window placement validation remain unfinished.
References: Microsoft [ROW_NUMBER](https://learn.microsoft.com/en-us/sql/t-sql/functions/row-number-transact-sql),
[RANK](https://learn.microsoft.com/en-us/sql/t-sql/functions/rank-transact-sql),
and [DENSE_RANK](https://learn.microsoft.com/en-us/sql/t-sql/functions/dense-rank-transact-sql).

Explicit ROWS/RANGE frames without ORDER BY report 10756; RANGE numeric
PRECEDING/FOLLOWING offsets report 4194, including zero offsets. GROUPS frames
fail explicitly as unsupported. These checks apply before aggregate type
lowering, including COUNT(*) and noninteger arguments. Named-window inheritance is resolved before validation; complete bound-expression/order
diagnostics remain pending. Client tests
verify ascending/descending NULL ordering, RANGE peers, bounded ROWS with empty
frames, suffix frames and whole-partition frames. Reference: Microsoft
[OVER clause](https://learn.microsoft.com/en-us/sql/t-sql/queries/select-over-clause-transact-sql).
