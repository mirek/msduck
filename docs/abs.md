# ABS result types

ABS now widens known TINYINT and SMALLINT operands to INT before evaluation,
so ABS(CAST(-32768 AS SMALLINT)) returns 32768 with four-byte integer metadata.
INT and BIGINT retain their widths and report 8115 for their minimum signed
values. Known BIT and REAL operands produce eight-byte FLOAT results. Known
DECIMAL/NUMERIC operands produce DECIMAL(38,s), retaining the input scale;
SMALLMONEY and MONEY produce the existing MONEY representation.

The target type is applied before evaluation, with one occurrence of the input.
Types are known for declared variables, RPC parameters and explicit casts;
existing integer expression inference also covers supported nested arithmetic,
CASE and logical functions. Integer ABS results participate in surrounding
integer division and comparison conversion. NULL and empty results retain the
same metadata. Source-column inference, general
function return inference and character conversion remain incomplete.

Rules are from Microsoft's [ABS reference](https://learn.microsoft.com/en-us/sql/t-sql/functions/abs-transact-sql?view=sql-server-ver17).
Client tests cover SMALLINT's minimum, NULLs, result metadata, DECIMAL scale,
BIT/REAL, prepared overflow and reuse, nested division, comparisons and SELECT
INTO. Live SQL Server differential validation remains outstanding.

Numeric function inference also handles decimal-point literals, whole-number
literals above INT range, scientific notation and unary signs. Decimal precision
is counted from digits without floating-point conversion (up to 38 digits),
and literal fractional scale is retained. Scientific notation uses FLOAT(53).
See Microsoft’s [constant syntax](https://learn.microsoft.com/en-us/sql/t-sql/data-types/constants-transact-sql?view=sql-server-ver17)
and [decimal literal rules](https://learn.microsoft.com/en-us/sql/t-sql/data-types/decimal-and-numeric-transact-sql?view=sql-server-ver17).
Ordinary expressions also use [numeric literal typing](numeric-literals.md); full
decimal arithmetic precision/scale inference remains unfinished.
