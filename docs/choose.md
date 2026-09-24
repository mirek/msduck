# CHOOSE

CHOOSE lowers to a simple CASE whose operand is the index converted to INT.
Choice positions start at one. NULL, zero, negative and out-of-range indexes
produce NULL. Numeric index conversion uses the existing integer-conversion
rules, including fractional truncation and range errors.

The function accepts an index and at least two scalar choices. Unsupported
aggregate/window modifiers are rejected. Known integer/character choice types
use the shared CASE precedence path, including when CHOOSE is nested inside
IIF. Other choice types retain backend CASE coercion. Parameters stay bound.

Tests cover Unicode strings, BIGINT results, boundary and fractional indexes,
column indexes, NULL, nested expressions, prepared rebinding, invalid numeric
text, index overflow and reuse after errors. The implementation follows
[Microsoft's CHOOSE contract](https://learn.microsoft.com/en-us/sql/t-sql/functions/logical-functions-choose-transact-sql).

Remaining work includes full type precedence, catalog-derived choice types,
decimal/temporal and character-width rules, NULL-constant edge cases, exact
diagnostics, and evaluation counts/order for volatile or aggregate expressions.
Live SQL Server differential validation remains outstanding.
