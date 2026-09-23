# Bitwise NOT

The `~` operator uses native overloads for BIT, TINYINT, SMALLINT, INT and BIGINT.
Each overload preserves the input width and result type. BIT flips true/false;
TINYINT complements eight bits, so `~CAST(5 AS TINYINT)` is 250. Signed integer
complements preserve their widths. NULL inputs yield typed NULL results.

Overload resolution uses the bound backend type, so this also supports uncast
source columns and stored views. Known integer result types feed existing
comparison, arithmetic and logical-expression inference. The rule follows
Microsoft's [bitwise NOT reference](https://learn.microsoft.com/en-us/sql/t-sql/language-elements/bitwise-not-transact-sql).

Tedious tests cover all five parameter types, signed boundaries, source columns,
NULL/empty metadata, nesting with negation and COALESCE, and stored views. A native
5,000-row test verifies complements and NULLs across vector batches.

Binary/varbinary operands, the complete implicit-conversion matrix, exact invalid
operand diagnostics, broader BIT expression inference and live SQL Server
comparison remain unfinished. This does not complete binary bitwise operators.
