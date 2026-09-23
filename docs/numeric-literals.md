# Numeric literals

Ordinary translated expressions now type whole-number literals above INT's
positive limit as DECIMAL with the required precision, rather than DuckDB's
BIGINT/HUGEINT. Decimal-point literals receive minimum precision with their
written fractional scale, ignoring leading integer zeros. Digit counting avoids
floating-point conversion and supports precision up to 38. Scientific literals
continue through the backend's DOUBLE representation.

This follows Microsoft's [integer constant rule](https://learn.microsoft.com/en-us/sql/t-sql/data-types/int-bigint-smallint-and-tinyint-transact-sql?view=sql-server-ver17)
and [decimal literal rule](https://learn.microsoft.com/en-us/sql/t-sql/data-types/decimal-and-numeric-transact-sql?view=sql-server-ver17).
The expression visitor adds DECIMAL casts after visiting the literal. Small
integer tokens remain bare to preserve positional ORDER BY/GROUP BY syntax.

Client tests cover result metadata (including empty results), prepared execution,
views, SELECT INTO, leading zeros, scientific notation, fractional division,
and exact 19/38-digit values converted to text. Tedious exposes DECIMAL values as
JavaScript numbers, which can lose precision; text assertions check server-side
exactness independently of that client conversion.

Integer conversion also handles DuckDB's leading-zero-free formatting of
DECIMAL(p,p): `.9` and `-.9` truncate to zero. Character strings still follow
the separate integer-text validation rules.

SQL Server's full decimal arithmetic precision/scale formulas, automatic
parameterization, over-38-digit errors, literal-derived column labels and the
full implicit conversion matrix remain unfinished. The fractional division test
checks its value only; it does not establish decimal result metadata parity.
Live SQL Server differential validation remains outstanding.
