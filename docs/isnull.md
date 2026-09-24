# ISNULL

ISNULL uses the first argument's bound storage type for its result and replacement
conversion. This works for declared parameters, casts and table columns. A literal
NULL first argument instead uses the replacement expression; two literal NULLs
produce a nullable INT. These rules follow Microsoft's
[ISNULL reference](https://learn.microsoft.com/en-us/sql/t-sql/functions/isnull-transact-sql).

Integer targets use the shared conversion path: numeric fractions truncate toward
zero, invalid numeric text raises 245 and out-of-range values raise 8115. The
translation uses a shared macro so stored views can resolve the same implementation.
Known result types participate in the existing CASE/IIF/COALESCE precedence logic.

Tedious coverage checks prepared reuse, both argument orders, NULL results,
Unicode, integer truncation and overflow, table columns, stored views, nesting,
and integer metadata for NULL and empty results. Invalid arity and modifiers are
rejected during batch preflight.

Known DATETIME2 inputs now retain the first argument's scale and convert the
replacement with the exact temporal codec. This covers casts, variables, table
columns, known scalar-subquery outputs and nested expressions. A literal NULL
first argument inherits the replacement's DATETIME2 type and precision.
Tests cover all scale pairs, rounded replacements, NULL/empty metadata, prepared
error recovery and range overflow. A native sequence test verifies one evaluation
per needed operand across 6,000 rows. This also follows the inspected upstream
`implicit.ts` rule that chooses the first ISNULL argument's inferred type.

This is partial compatibility. Declared character lengths and SQL Server string
truncation, MONEY source rounding, the full implicit-conversion matrix, collations,
nullability metadata and precise diagnostics remain unfinished. Noninteger
conversion outside the exact DATETIME2 path uses DuckDB behavior. General volatile-expression/subquery evaluation and
live SQL Server differential validation remain unverified.

Known CHAR/SPACE expressions and their supported logical compositions now retain
the first argument's character width. Replacement values truncate to that width;
CHAR results pad with spaces when shorter. The result descriptor remains CHAR or
VARCHAR, including NULL and empty results. Literal-NULL first arguments still
inherit the replacement type. The width wrapper contains the ISNULL expression
once and works in stored defaults as well as projections.

This implements the documented [replacement conversion and truncation](https://learn.microsoft.com/en-us/sql/t-sql/functions/isnull-transact-sql?view=sql-server-ver17)
for inferred Windows-1252 text. Declared character columns/variables, broader
casts, codepage results with unknown replacement types and full collation conversion
still need broader inference. Tests cover zero widths, padding, truncation,
nested calls, prepared values, empty metadata and defaults.

For an inferred NVARCHAR first argument, ISNULL retains that width even when
the replacement's descriptor is unknown, and bounds the converted value in
UTF-16 storage units. Explicit NVARCHAR casts also supply this width (see
[cast coverage](nvarchar.md)). DATENAME-based tests cover replacement text, integer
conversion, NULL/empty results, supplementary characters, a non-NULL first
argument and prepared recovery. A native test crosses 6,000 rows.
The current Rust/Arrow string representation cannot retain an isolated UTF-16
surrogate. A truncation boundary inside a surrogate pair reports an explicit
unsupported error; complete non-SC/SC collation behavior remains unfinished.
