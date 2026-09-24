# YEAR, MONTH, DAY

These functions now return INT instead of DuckDB's BIGINT, including NULL and
empty results. Shared scalar-call validation rejects aggregate/window modifiers
and wrong arity. Known-type inference recognizes their INT result so nested
arithmetic, comparisons and logical result expressions use integer precedence.

Registered macros handle DATE/timestamp inputs and ISO date strings. TIME and
time-only strings use the base date 1900-01-01. Integer inputs count days from
1900-01-01 through the legacy DATETIME range (1753-01-01 to 9999-12-31).
A native Rust scalar checks these offsets before arithmetic, including extreme
BIGINT values, and reports overflow rather than wrapping. The functions also
work in stored defaults after reopening the database.

Exact DATETIME2 inputs now use native civil-date extraction before the calendar
function, retaining the full year range without rounding late-night values
into the next day. Stored columns, local variables, RPC parameters, prepared
queries and NULLs use this path. DATE casts and EOMONTH share the extraction.

A native string helper recognizes time-only input and supplies the base date;
other date text passes unchanged to the DATE conversion. The conversion macro
evaluates its input only in the selected type branch, preventing duplicated
evaluation of volatile inputs. A native sequence counter checks exactly one evaluation
per row for each of YEAR/MONTH/DAY over 6,000 rows. Client tests cover correlated
scalar subqueries, prepared subqueries, and source columns whose names match
internal helper parameters.

The upstream mssqlite date-function test provides the basic 2026-07-01 example.
Independent tedious coverage includes base dates, negative offsets, leap days,
NULL/empty INT metadata, DATE and BIGINT prepared inputs, recovery after errors,
source columns, defaults, and composition with EOMONTH/DATEFROMPARTS. A vector
test checks offset arithmetic and mixed NULLs across chunks. The capture harness
includes a probe for live SQL Server comparison.

Remaining work includes floating-point/decimal numeric date conversion, BIT
inputs, language and DATEFORMAT parsing, complete conversion diagnostics,
and datetimeoffset preservation. General string parsing still uses DuckDB conversions; this is
not complete SQL Server date-conversion compatibility. Overflow error details
and all supported cases still need live SQL Server comparison.

References: Microsoft [YEAR](https://learn.microsoft.com/en-us/sql/t-sql/functions/year-transact-sql),
[MONTH](https://learn.microsoft.com/en-us/sql/t-sql/functions/month-transact-sql),
and [DAY](https://learn.microsoft.com/en-us/sql/t-sql/functions/day-transact-sql).
