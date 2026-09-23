# Mixed integer/character arithmetic

Known integer/character operands now use integer precedence for `+`, `-`, `*`,
`/` and `%`. For example, `'1' + 2` returns integer 3, while `'1' + '2'` remains
character concatenation. Character values use the shared integer converter, so
fractional numeric text fails with 245 rather than rounding. Numeric overflow
and zero divisors retain the existing 8115 and 8134 paths.

Known INT/BIGINT arithmetic results feed surrounding comparisons, logical
functions and arithmetic. Nested known character additions feed concatenation
inference. Thus `('1' + 2) / 2` returns 1, while `('1' + '2') + 3` returns 15.
Prepared tests cover all five operators, NULL values, spaces, handle reuse after
conversion/division errors, overflow, exact BIGINT values and nested expressions.

These behaviors follow Microsoft's [conversion examples](https://learn.microsoft.com/en-us/sql/t-sql/functions/cast-and-convert-transact-sql)
and [type precedence rules](https://learn.microsoft.com/en-us/sql/t-sql/data-types/data-type-precedence-transact-sql).
The copied mssqlite corpus includes `'1' + 2` in its implicit-conversion case.

Negation widens known TINYINT operands to SMALLINT before applying minus, following
Microsoft's [unary negative rule](https://learn.microsoft.com/en-us/sql/t-sql/language-elements/unary-operators-negative).
Known unary integer result types feed surrounding arithmetic, comparisons and
logical expressions. Tests cover 255, zero, NULL, double negation, integer division,
CASE/COALESCE, typed empty results and SELECT INTO. Uncast source-column types
still require broader inference; this does not complete unary operator parity.

Full source-column/function type inference, all small-integer result rules,
decimal/floating/money arithmetic, implicit bitwise conversion, character widths,
collations and binary concatenation remain unfinished. Unknown operand types
retain backend behavior. Live SQL Server differential validation remains pending.
