# EOMONTH

EOMONTH returns a native DATE containing the last day of the input month,
optionally shifted by an integer number of months. The Rust scalar preserves
NULL inputs, checks the year 1–9999 range before calendar conversion, and uses
wide intermediate arithmetic so extreme INT offsets cannot wrap. Date-range
overflow reports error 517 and is catchable with TRY/CATCH.

AST lowering validates one or two scalar arguments, converts the date input to
DATE, and uses the shared INT conversion path for the offset. Fractional numeric
offsets truncate toward zero. Each argument occurs once in the lowered call.
The function is registered on the database owner, so client sessions, views,
and stored defaults use the same implementation, including after reopening.

Native tests compare every valid calendar day with DuckDB's month-end calendar
and exercise mixed offsets and NULLs across vector boundaries. Tedious tests
cover leap centuries, boundary dates, positive/negative offsets, prepared-query
recovery, DATE metadata for NULL/empty results, defaults, views, and malformed
calls. The persistent-storage test checks a default after reopening. A reference
comparison probe records values, metadata, and the overflow number.

DATETIME2 inputs use exact native date extraction, including stored columns and
prepared queries. Other date input conversion still uses the backend DATE conversion. SQL Server's
complete string formats, language/DATEFORMAT settings, fractional numeric date coercion,
and conversion diagnostics remain unfinished. Live SQL Server comparison of
this function and its errors is still required; the local audit is diagnostic.

Sources: [Microsoft EOMONTH](https://learn.microsoft.com/en-us/sql/t-sql/functions/eomonth-transact-sql),
and the copied mssqlite engine/transpiler tests documented in
[reference review](reference-review.md).

Integer inputs use checked legacy DATETIME day offsets from 1900-01-01,
sharing YEAR/MONTH/DAY conversion. NULLs retain DATE metadata. Inputs outside
1753-01-01 through 9999-12-31 report 8115; month arithmetic overflow remains 517.
Native tests cover 6,000 rows and single input evaluation. Client tests cover
integer endpoints, source columns, prepared calls, typed empty results and
DATETIMEOFFSET local-month boundaries. Fractional numerics and live reference
comparison remain open.
