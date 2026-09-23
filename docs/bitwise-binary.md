# Binary bitwise operators

AND (`&`), OR (`|`) and XOR (`^`) use native overloads for every pairing of BIT,
TINYINT, SMALLINT, INT and BIGINT. Inputs widen to the larger integer width; BIT
pairs produce TINYINT. NULL in either operand produces a typed NULL. Bound column
types participate directly, so operation widths do not depend on AST source-type
inference. Known integer expression types propagate into surrounding operations.

This follows Microsoft's [AND](https://learn.microsoft.com/en-us/sql/t-sql/language-elements/bitwise-and-transact-sql),
[OR](https://learn.microsoft.com/en-us/sql/t-sql/language-elements/bitwise-or-transact-sql)
and [XOR](https://learn.microsoft.com/en-us/sql/t-sql/language-elements/bitwise-exclusive-or-transact-sql)
references. The tokenizer also handles XOR immediately followed by a variable,
such as `@a^@b`, without interpreting `^@` as a PostgreSQL operator.

Tedious tests cover BIT pairs, mixed widths, both operand orders, signed values,
BIGINT precision, NULL/empty metadata, table columns and compound SET/UPDATE.
A native matrix test checks all 25 input-type pairs for all three operations,
using 5,000 rows per pair with NULLs across vector boundaries.

Binary/varbinary operands, the complete implicit-conversion matrix, broader BIT
expression inference, exact invalid-operand diagnostics and live SQL Server
validation remain unfinished.
