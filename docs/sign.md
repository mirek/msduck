# SIGN result types

SIGN uses SQL Server result types for known numeric operands: TINYINT, SMALLINT
and INT return INT; BIGINT remains BIGINT; DECIMAL/NUMERIC retain precision and
scale; money operands use the existing MONEY representation; REAL and FLOAT
return FLOAT(53). The value is -1, 0 or 1, with NULL propagation.

The input appears once in the translated expression. Explicit result casts keep
metadata stable for typed NULLs and empty result sets. Known types include casts,
declared variables, RPC parameters, supported integer expressions and nested
ABS/CEILING/FLOOR/SIGN calls. Integer results also participate in surrounding
integer division and comparison conversion.

Rules follow Microsoft's [SIGN reference](https://learn.microsoft.com/en-us/sql/t-sql/functions/sign-transact-sql?view=sql-server-ver17).
Client tests cover signed integer limits, tiny fractional decimals, zero, NULLs,
prepared execution, result metadata, nested functions and SELECT INTO.
Uncast source-column inference, exact invalid-type diagnostics
(including BIT), the full implicit conversion matrix and live SQL Server
differential validation remain unfinished.

Numeric function inference also handles decimal-point literals, whole-number
literals above INT range, scientific notation and unary signs. Decimal precision
is counted from digits without floating-point conversion (up to 38 digits),
and literal fractional scale is retained. Scientific notation uses FLOAT(53).
See Microsoft’s [constant syntax](https://learn.microsoft.com/en-us/sql/t-sql/data-types/constants-transact-sql?view=sql-server-ver17)
and [decimal literal rules](https://learn.microsoft.com/en-us/sql/t-sql/data-types/decimal-and-numeric-transact-sql?view=sql-server-ver17).
Ordinary expressions also use [numeric literal typing](numeric-literals.md); full
decimal arithmetic precision/scale inference remains unfinished.
