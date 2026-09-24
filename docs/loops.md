# WHILE and loop control

WHILE evaluates its condition against current batch variables before every
iteration. A single following statement or a BEGIN/END block forms its body.
FALSE and UNKNOWN skip the body. BREAK removes pending work through the
innermost active loop boundary; CONTINUE removes pending body work but retains
that boundary, so its next action reevaluates the condition. Nested loops use
separate boundaries. Loop control outside a loop is rejected during variable
preflight, before preceding writes can execute.

`crates/msduck-sql/src/dialect.rs` wraps the pinned sqlparser MSSQL dialect, preserving its type
identity and forwarding its overridden capabilities. It adds single-statement
WHILE parsing, semicolon-optional blocks, and BREAK/CONTINUE markers because
upstream lacks dedicated AST nodes for those statements. The markers use
internal placeholders that cannot be emitted by the T-SQL tokenizer. They are
intercepted by the interpreter and never submitted to DuckDB. Quoted identifiers
and strings containing control keywords retain their ordinary meaning.

Execution uses an explicit work stack. As an interim resource boundary, a batch
may execute 10,000 interpreter steps, counting ordinary statements, condition
checks, blocks and loop control. Exceeding this returns an error and leaves the
connection reusable. This is an msduck limit, not SQL Server behavior, and does
not replace active cancellation or a time limit on a single DuckDB query.
Earlier autocommit writes are not undone by this error. Explicit transactions
remain available for caller rollback.

Accumulated batch results now share the 16 MiB response cap. Repeated result
sets cannot independently consume that allowance each time. On overflow, prior
results are followed by an error completion; later statements do not execute.

Tedious tests cover single-statement loops, zero iterations, nested BREAK and
CONTINUE, result sets per iteration, DML, non-Boolean conditions, invalid loop
control, and connection reuse. Tiberius checks final SQL-batch completion after
loops and zero iterations. Unit tests cover runaway execution recovery, whole
batch response limits, statement boundaries and malformed block syntax.

The copied T-SQL `language-elements.md` supplied the control-flow reference.
Remaining work includes compound SELECT/UPDATE assignments, active-query
cancellation, procedural prepared RPCs and SQL Server differential validation.
