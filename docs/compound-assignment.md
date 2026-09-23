# Compound assignments

The dialect accepts SET @variable with +=, -=, *=, /=, %=, &=, |= and ^=.
It constructs an ordinary SET assignment whose expression references the
current variable, preserving AST traversal, variable preflight and bound values.
The result is converted to the declared variable type before updating it.
The complete right-hand expression is parenthesized when rendering SQL, so
`SET @n *= 2+3` multiplies by five rather than adding three after multiplication.

For expressions known to be integral from variables, casts, int-range literals
or supported arithmetic subexpressions, division uses DuckDB integer division.
This truncates toward zero without converting BIGINT through floating point.
Known integral division/modulo by zero raise error 8134. XOR lowers to DuckDB's
xor function. Plus between known string operands lowers to concatenation.
These expression fixes also apply outside compound SET.

The tedious test covers every operator, negative division, BIGINT precision
beyond JavaScript's exact-number range, bound Unicode/quote-containing strings,
a loop counter, missing variables, zero divisors and connection reuse.
The copied T-SQL language-elements reference supplies the assignment operators.

Compound UPDATE columns also accept these operators; see
[UPDATE coverage](update-conversion.md) for catalog typing and limitations.

SELECT variable assignments accept the same eight operators. The parser
recognizes them only as complete projection items and expands the right-hand
expression with explicit grouping. Existing assignment conversion, variable
preflight and empty-result behavior apply. Tests cover every operator, BIGINT
precision, strings and bound inputs, CTEs, mixed-output rejection, divide by
zero and overflow without changing the variable. Native arithmetic overflow
diagnostics map to 8115. Tiberius independently verifies assignment completion.

Right-hand variable references use their statement-start bindings. SELECT
compound assignment is not an aggregation mechanism: SQL Server does not
guarantee row-by-row accumulation, as documented under
[SELECT variable assignment](https://learn.microsoft.com/en-us/sql/t-sql/language-elements/select-local-variable-transact-sql).

Remaining work includes general expression
and column type inference, complete numeric promotion/conversion and overflow
rules, NULL/type edge cases, exact character widths, and SQL Server differential
validation. Unknown column/function operand types still follow backend arithmetic;
this does not establish full T-SQL division or string-operator compatibility.
