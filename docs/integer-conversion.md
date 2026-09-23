# Integer conversion

CAST and TRY_CAST to TINYINT, SMALLINT, INT and BIGINT truncate decimal and
floating-point inputs toward zero before checking the destination range.
CONVERT and TRY_CONVERT without style arguments use the same integer path.
The path also applies to local initializers, SET/SELECT assignment, RETURN
expressions and DATEFROMPARTS arguments that the interpreter casts explicitly.
ALTER COLUMN to an integer type also uses this path for existing row values.
[INSERT integer targets](insert-conversion.md) use it for VALUES/SELECT sources
and integer defaults.
[UPDATE integer assignments](update-conversion.md) also use this path, including
DEFAULT expressions and CTE-wrapped statements.

Decimal conversion removes the fractional portion from the backend's exact
fixed-point text without passing through floating point. This preserves all
38 digits and permits fractional inputs at the BIGINT bounds. Finite floats
are truncated after reading the backend's round-trip representation at its
original precision: REAL text is parsed as f32 before widening. The
native normalizer receives each value once; typeof supplies binding metadata.
The resulting integral text goes through the destination cast, keeping its
wire width and allowing TRY_CAST/TRY_CONVERT to return NULL on failure.
Ordinary numeric conversions check the requested width after truncation and
report error 8115 with severity 16 on overflow. This also applies to assignment,
RETURN, DATEFROMPARTS argument conversion and ALTER COLUMN. TRY conversions
skip the throwing check and let the outer cast return NULL. Error state and
message prefixes still use the current backend/session conventions.
Error number/severity reference: [Microsoft error 8115 catalog](https://learn.microsoft.com/en-us/sql/relational-databases/errors-events/database-engine-events-and-errors-8000-to-8999).

Valid signed integer text outside the INT range now reports 248, including
arbitrarily long digit strings and leading zeroes. Malformed text retains 245;
TRY conversions return NULL. The check runs in the shared conversion path, so
ISNULL, arithmetic coercion, assignments and function arguments receive the same
classification. The diagnostic uses a generic character-source message because
DuckDB storage does not retain VARCHAR versus NVARCHAR provenance.
See [Microsoft's error 248 catalog](https://learn.microsoft.com/en-us/sql/relational-databases/errors-events/database-engine-events-and-errors-0-to-999).

Known MONEY/SMALLMONEY casts, variables and RPC parameters retain their source
type until translation and use rounding. Storage still uses exact decimals.
Empty and ASCII-space-only character strings become zero. Signed decimal
integer text is accepted; fractions, exponents and malformed strings fail
instead of being rounded by DuckDB. BIT inputs become 0 or 1.

The mssqlite integer conversion fixture supplied checked boundary and empty
string cases. Tedious tests cover signed fractions, every width, exact BIGINT
limits, NULLs, malformed strings, TRY conversion, variables, stored columns,
prepared reuse, money/decimal RPC distinctions and DATEFROMPARTS arguments.
A database restart test verifies a default containing an integer cast.
Overflow tests cover all four widths, REAL precision boundaries, huge finite
floats, @@ERROR, TRY/CATCH, prepared recovery and failed ALTER preservation.
Tiberius independently checks error 8115 and subsequent typed NULL recovery.

Remaining differences include MERGE conversions, full DML target/type resolution and conversion
of existing column defaults during ALTER,
character overflow diagnostics for the other integer widths, money overflow diagnostics, exact error states/messages, general style-based CONVERT,
binary and temporal conversions, and character whitespace/code-page details.
Declared money column types and money expression provenance are not preserved
in the backend catalog, so those expressions can still truncate incorrectly.
FLOAT precision buckets are now translated explicitly; see [FLOAT and REAL](float.md).
Live SQL Server differential validation remains outstanding.

Reference: [Microsoft CAST and CONVERT](https://learn.microsoft.com/en-us/sql/t-sql/functions/cast-and-convert-transact-sql).
