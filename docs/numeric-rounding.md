# CEILING and FLOOR

Known TINYINT, SMALLINT and INT inputs return INT; BIGINT remains BIGINT.
Rounding an integer is an identity operation, so these expressions preserve all
64 bits rather than passing through a floating-point backend overload. Known
BIT and REAL inputs return FLOAT(53); money inputs retain the existing MONEY
representation. Known DECIMAL(p,s) inputs return DECIMAL(p,0), following the
current SQL Server 17 documentation for
[CEILING](https://learn.microsoft.com/en-us/sql/t-sql/functions/ceiling-transact-sql?view=sql-server-ver17)
and [FLOOR](https://learn.microsoft.com/en-us/sql/t-sql/functions/floor-transact-sql?view=sql-server-ver17).
Decimal input keeps its fractional part until the rounding function executes.

The implementation shares result-type inference with ABS for declared variables,
RPC parameters, explicit casts, and supported nested numeric functions and integer
expressions. Each input occurs once. Result casts preserve descriptors for NULL
and empty results. FLOOR's dedicated parser node is normalized to the same
function path used for CEILING.

Client tests cover positive and negative fractional values, both signed BIGINT
limits, NULL and empty metadata, prepared decimal parameters, and nested calls.
Uncast source-column inference, the full implicit conversion
matrix, and live SQL Server differential validation remain unfinished. Decimal
precision/scale behavior across SQL Server versions needs differential evidence.

Numeric function inference also handles decimal-point literals, whole-number
literals above INT range, scientific notation and unary signs. Decimal precision
is counted from digits without floating-point conversion (up to 38 digits),
and literal fractional scale is retained. Scientific notation uses FLOAT(53).
See Microsoft’s [constant syntax](https://learn.microsoft.com/en-us/sql/t-sql/data-types/constants-transact-sql?view=sql-server-ver17)
and [decimal literal rules](https://learn.microsoft.com/en-us/sql/t-sql/data-types/decimal-and-numeric-transact-sql?view=sql-server-ver17).
Ordinary expressions also use [numeric literal typing](numeric-literals.md); full
decimal arithmetic precision/scale inference remains unfinished.
